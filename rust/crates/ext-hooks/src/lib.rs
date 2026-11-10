//! ghmcp-ext-hooks : outils étendus gh_ : webhooks (dépôt/org), livraisons, administration Actions
//! (secrets, variables, runners, runs, logs, permissions). Contrat : en-tête de crates/core/src/lib.rs.
//! Les endpoints simples sont décrits par une table `Spec` ; seuls `secret_set` (chiffrement) et
//! `run_logs` (rest_raw) ont un handler dédié.
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ghmcp_core::{arg_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::sync::Arc;

#[derive(Clone)]
struct Spec {
    name: String,
    desc: String,
    method: &'static str,
    path: String, // `{x}` = paramètre de chemin ; `{base}` = racine dépôt/org/environnement selon les arguments
    p: &'static [&'static str], // paramètres de chemin requis
    q: &'static [&'static str], // query optionnels
    b: &'static [&'static str], // champs de corps optionnels
    rb: &'static [&'static str], // champs de corps requis
    sc: &'static str, // "" | "secrets" | "variables" | "runners" : active `{base}`
    confirm: Option<&'static str>,
    scopes: &'static [&'static str],
    perm: &'static str,
    level: Level,
}

fn spec(name: &str, desc: &str, method: &'static str, path: &str) -> Spec {
    Spec {
        name: format!("gh_{name}"),
        desc: desc.into(),
        method,
        path: path.into(),
        p: &[],
        q: &[],
        b: &[],
        rb: &[],
        sc: "",
        confirm: None,
        scopes: &["repo"],
        perm: "actions",
        level: Level::Cicd,
    }
}

const PAGE: &[&str] = &["per_page", "page"];

fn ty(k: &str) -> Value {
    match k {
        k if k.ends_with("_id") => json!({"type": ["integer", "string"]}),
        "events" | "selected_repository_ids" => {
            json!({"type": "array", "items": {"type": ["string", "integer"]}})
        }
        "config" => json!({"type": "object"}),
        "active" | "enabled" => json!({"type": "boolean"}),
        "per_page" | "page" => json!({"type": "integer"}),
        _ => json!({"type": "string"}),
    }
}

impl Spec {
    fn schema(&self) -> Value {
        let mut props = Map::new();
        let mut names: Vec<&str> = [self.p, self.q, self.b, self.rb].concat();
        if !self.sc.is_empty() {
            names.extend(["owner", "repo", "org", "environment_name"]);
        }
        for k in names {
            props.insert(k.into(), ty(k));
        }
        let req: Vec<&str> = [self.p, self.rb].concat();
        let mut s = json!({"type": "object", "properties": props, "required": req});
        if !self.sc.is_empty() {
            s["description"] = json!("Cible : org (organisation) OU owner+repo (dépôt) ; avec environment_name = environnement du dépôt (secrets/variables).");
        }
        s
    }

    fn tool_with<F, Fut>(self, f: F) -> Tool
    where
        F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        let read = self.method == "GET";
        let mut t = Tool::new(&self.name, &self.desc, self.schema(), f)
            .annotations(json!({"readOnlyHint": read, "destructiveHint": self.method == "DELETE"}))
            .scopes(self.scopes)
            .perm(self.perm, if read { Access::Read } else { Access::Write })
            .level(self.level);
        if self.confirm.is_some() {
            t = t.approval();
        }
        t
    }

    fn tool(self) -> Tool {
        let s = Arc::new(self.clone());
        self.tool_with(move |c, a| run(s.clone(), c, a))
    }
}

/// Valeur d'un paramètre (chaîne ou entier) pour un chemin, encodée.
fn pv(args: &Value, k: &str) -> Result<String> {
    match args.get(k) {
        Some(Value::String(s)) if !s.is_empty() => Ok(urlencoding::encode(s).into_owned()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        _ => Err(GhError::InvalidArgs(format!(
            "paramètre requis manquant: {k}"
        ))),
    }
}

/// Racine REST d'une ressource Actions : environnement > organisation > dépôt.
fn base(args: &Value, sc: &str) -> Result<String> {
    if arg_str(args, "environment_name").is_some() && sc != "runners" {
        return Ok(format!(
            "/repos/{}/{}/environments/{}/{sc}",
            pv(args, "owner")?,
            pv(args, "repo")?,
            pv(args, "environment_name")?
        ));
    }
    if arg_str(args, "repo").is_none() && arg_str(args, "org").is_some() {
        return Ok(format!("/orgs/{}/actions/{sc}", pv(args, "org")?));
    }
    if arg_str(args, "repo").is_none() && arg_str(args, "owner").is_none() {
        return Err(GhError::InvalidArgs(
            "cible requise: org, ou owner + repo".into(),
        ));
    }
    Ok(format!(
        "/repos/{}/{}/actions/{sc}",
        pv(args, "owner")?,
        pv(args, "repo")?
    ))
}

fn path_of(s: &Spec, args: &Value) -> Result<String> {
    let mut path = s.path.clone();
    if !s.sc.is_empty() {
        path = path.replace("{base}", &base(args, s.sc)?);
    }
    for k in s.p {
        path = path.replace(&format!("{{{k}}}"), &pv(args, k)?);
    }
    Ok(path)
}

async fn run(s: Arc<Spec>, ctx: Ctx, args: Value) -> Result<Value> {
    let gh = ctx.gh(&args)?;
    let path = path_of(&s, &args)?;
    let query: Vec<(&str, String)> =
        s.q.iter()
            .filter_map(|k| {
                args.get(*k).map(|v| {
                    (
                        *k,
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
            })
            .collect();
    let mut body = Map::new();
    for k in s.rb {
        body.insert(
            (*k).into(),
            args.get(*k)
                .cloned()
                .ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))?,
        );
    }
    for k in s.b {
        if let Some(v) = args.get(*k) {
            body.insert((*k).into(), v.clone());
        }
    }
    if let Some(m) = s.confirm {
        ctx.confirm(&args, &format!("{m} ({} {path})", s.method))
            .await?;
    }
    gh.rest(
        s.method,
        &path,
        &query,
        if body.is_empty() {
            None
        } else {
            Some(Value::Object(body))
        },
    )
    .await
}

/// Chiffrement "sealed box" libsodium attendu par l'API secrets de GitHub (clé publique en base64).
pub fn seal(key_b64: &str, value: &str) -> Result<String> {
    let raw = B64
        .decode(key_b64)
        .map_err(|e| GhError::Other(format!("clé publique invalide: {e}")))?;
    let arr: [u8; 32] = raw
        .try_into()
        .map_err(|_| GhError::Other("clé publique: 32 octets attendus".into()))?;
    let ct = crypto_box::PublicKey::from(arr)
        .seal(&mut crypto_box::aead::OsRng, value.as_bytes())
        .map_err(|e| GhError::Other(format!("chiffrement: {e}")))?;
    Ok(B64.encode(ct))
}

async fn secret_set(ctx: Ctx, args: Value) -> Result<Value> {
    let gh = ctx.gh(&args)?;
    let (b, name) = (base(&args, "secrets")?, pv(&args, "secret_name")?);
    let value = arg_str(&args, "value")
        .ok_or_else(|| GhError::InvalidArgs("paramètre requis manquant: value".into()))?;
    let pk = gh
        .rest("GET", &format!("{b}/public-key"), &[], None)
        .await?;
    let (key_id, key) = match (pk["key_id"].as_str(), pk["key"].as_str()) {
        (Some(i), Some(k)) => (i.to_string(), k.to_string()),
        // mode test sans fixture : clé fictive, pour exercer quand même le chiffrement
        _ if ctx.test_mode() => (
            "test-key-id".into(),
            B64.encode(
                crypto_box::SecretKey::generate(&mut crypto_box::aead::OsRng)
                    .public_key()
                    .as_bytes(),
            ),
        ),
        _ => return Err(GhError::Other("clé publique absente de la réponse".into())),
    };
    let mut body = json!({"encrypted_value": seal(&key, value)?, "key_id": key_id});
    for k in ["visibility", "selected_repository_ids"] {
        if let Some(v) = args.get(k) {
            body[k] = v.clone();
        }
    }
    gh.rest("PUT", &format!("{b}/{name}"), &[], Some(body))
        .await
}

/// Lien de téléchargement des journaux d'un run (l'archive elle-même n'est pas renvoyée).
async fn run_logs(ctx: Ctx, args: Value) -> Result<Value> {
    let gh = ctx.gh(&args)?;
    let path = format!(
        "/repos/{}/{}/actions/runs/{}/logs",
        pv(&args, "owner")?,
        pv(&args, "repo")?,
        pv(&args, "run_id")?
    );
    // 302 non suivi (comme le Go) ; shortcut : copie de actions.rs::redirect_url, ext-hooks ne dépend pas de tools-adp.
    let opts = ghmcp_core::net::ReqOpts {
        no_redirect: true,
        ..Default::default()
    };
    let raw = gh.rest_raw_with("GET", &path, &[], None, &opts).await?;
    let url = raw
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.clone())
        .ok_or_else(|| {
            GhError::Other(format!(
                "pas de redirection (statut {}) pour {path}",
                raw.status
            ))
        })?;
    Ok(json!({"download_url": url, "api_path": path}))
}

fn hooks(org: bool) -> Vec<Tool> {
    let (n, w, root, p, scope): (
        &str,
        &str,
        &str,
        &'static [&'static str],
        &'static [&'static str],
    ) = if org {
        (
            "org_hook",
            "organization",
            "/orgs/{org}/hooks",
            &["org"],
            &["admin:org_hook"],
        )
    } else {
        (
            "repo_hook",
            "repository",
            "/repos/{owner}/{repo}/hooks",
            &["owner", "repo"],
            &["repo", "admin:repo_hook"],
        )
    };
    let one = format!("{root}/{{hook_id}}");
    let ps: &'static [&'static str] = if org {
        &["org", "hook_id"]
    } else {
        &["owner", "repo", "hook_id"]
    };
    let pd: &'static [&'static str] = if org {
        &["org", "hook_id", "delivery_id"]
    } else {
        &["owner", "repo", "hook_id", "delivery_id"]
    };
    let mut v = vec![
        Spec {
            p,
            q: PAGE,
            ..spec(
                &format!("{n}s_list"),
                &format!("List {w} webhooks."),
                "GET",
                root,
            )
        },
        Spec {
            p: ps,
            ..spec(
                &format!("{n}_get"),
                &format!("Get one {w} webhook."),
                "GET",
                &one,
            )
        },
        Spec {
            p,
            b: &["name", "events", "active"],
            rb: &["config"],
            ..spec(
                &format!("{n}_create"),
                &format!("Create a {w} webhook (config: url, content_type, secret, insecure_ssl)."),
                "POST",
                root,
            )
        },
        Spec {
            p: ps,
            b: &["events", "active", "config"],
            ..spec(
                &format!("{n}_update"),
                &format!("Update a {w} webhook."),
                "PATCH",
                &one,
            )
        },
        Spec {
            p: ps,
            confirm: Some("Supprimer ce webhook ?"),
            ..spec(
                &format!("{n}_delete"),
                &format!("Delete a {w} webhook. Requires human approval."),
                "DELETE",
                &one,
            )
        },
        Spec {
            p: ps,
            q: &["per_page", "cursor", "redelivery"],
            ..spec(
                &format!("{n}_deliveries"),
                &format!("List recent deliveries of a {w} webhook."),
                "GET",
                &format!("{one}/deliveries"),
            )
        },
        Spec {
            p: pd,
            ..spec(
                &format!("{n}_delivery_get"),
                "Get one webhook delivery (request and response payload).",
                "GET",
                &format!("{one}/deliveries/{{delivery_id}}"),
            )
        },
        Spec {
            p: pd,
            ..spec(
                &format!("{n}_redeliver"),
                "Redeliver a webhook delivery.",
                "POST",
                &format!("{one}/deliveries/{{delivery_id}}/attempts"),
            )
        },
        Spec {
            p: ps,
            ..spec(
                &format!("{n}_ping"),
                &format!("Send a ping event to a {w} webhook."),
                "POST",
                &format!("{one}/pings"),
            )
        },
    ];
    for s in &mut v {
        (s.scopes, s.perm, s.level) = (scope, "webhooks", Level::Admin);
    }
    v.into_iter().map(Spec::tool).collect()
}

fn actions() -> Vec<Tool> {
    const RUN: &[&str] = &["owner", "repo", "run_id"];
    const REPO: &[&str] = &["owner", "repo"];
    let mut v = vec![
        Spec { q: PAGE, sc: "secrets", ..spec("actions_secrets_list", "List Actions secret names (repo, org or environment).", "GET", "{base}") },
        Spec { p: &["secret_name"], sc: "secrets", confirm: Some("Supprimer ce secret Actions ?"), ..spec("actions_secret_delete", "Delete an Actions secret. Requires human approval.", "DELETE", "{base}/{secret_name}") },
        Spec { q: PAGE, sc: "variables", ..spec("actions_variables_list", "List Actions variables (repo, org or environment).", "GET", "{base}") },
        Spec { p: &["name"], sc: "variables", ..spec("actions_variable_get", "Get an Actions variable.", "GET", "{base}/{name}") },
        Spec { b: &["visibility", "selected_repository_ids"], rb: &["name", "value"], sc: "variables", ..spec("actions_variable_create", "Create an Actions variable.", "POST", "{base}") },
        Spec { p: &["name"], b: &["value", "visibility", "selected_repository_ids"], sc: "variables", ..spec("actions_variable_update", "Update an Actions variable.", "PATCH", "{base}/{name}") },
        Spec { p: &["name"], sc: "variables", confirm: Some("Supprimer cette variable Actions ?"), ..spec("actions_variable_delete", "Delete an Actions variable. Requires human approval.", "DELETE", "{base}/{name}") },
        Spec { q: PAGE, sc: "runners", ..spec("actions_runners_list", "List self-hosted runners (repo or org).", "GET", "{base}") },
        Spec { sc: "runners", ..spec("actions_runner_token", "Create a runner registration token (repo or org). The token is a secret.", "POST", "{base}/registration-token") },
        Spec { p: &["runner_id"], sc: "runners", confirm: Some("Supprimer ce runner auto-hébergé ?"), ..spec("actions_runner_delete", "Remove a self-hosted runner. Requires human approval.", "DELETE", "{base}/{runner_id}") },
        Spec { p: RUN, ..spec("actions_run_rerun", "Re-run a workflow run.", "POST", "/repos/{owner}/{repo}/actions/runs/{run_id}/rerun") },
        Spec { p: RUN, ..spec("actions_run_rerun_failed", "Re-run only the failed jobs of a workflow run.", "POST", "/repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs") },
        Spec { p: RUN, ..spec("actions_run_cancel", "Cancel a workflow run.", "POST", "/repos/{owner}/{repo}/actions/runs/{run_id}/cancel") },
        Spec { p: REPO, ..spec("actions_permissions_get", "Get the repository Actions permissions.", "GET", "/repos/{owner}/{repo}/actions/permissions") },
        Spec { p: REPO, rb: &["enabled"], b: &["allowed_actions"], ..spec("actions_permissions_set", "Set the repository Actions permissions (enabled, allowed_actions: all|local_only|selected).", "PUT", "/repos/{owner}/{repo}/actions/permissions") },
    ];
    for s in &mut v {
        match s.name.as_str() {
            n if n.contains("secret") => {
                (s.scopes, s.perm, s.level) = (&["repo", "admin:org"], "secrets", Level::Admin)
            }
            n if n.contains("runner") => {
                (s.scopes, s.perm, s.level) =
                    (&["repo", "admin:org"], "administration", Level::Admin)
            }
            n if n.contains("permissions") => {
                (s.scopes, s.perm, s.level) = (&["repo"], "administration", Level::Admin)
            }
            n if n.contains("variable") => {
                (s.scopes, s.perm) = (&["repo", "admin:org"], "variables")
            }
            _ => s.scopes = &["repo", "workflow"],
        }
    }
    let mut tools: Vec<Tool> = v.into_iter().map(Spec::tool).collect();
    let mut set = spec("actions_secret_set", "Create or update an Actions secret (value encrypted client-side with the repo/org public key, never logged).", "PUT", "");
    (
        set.p, set.b, set.rb, set.sc, set.scopes, set.perm, set.level,
    ) = (
        &["secret_name"],
        &["visibility", "selected_repository_ids"],
        &["value"],
        "secrets",
        &["repo", "admin:org"],
        "secrets",
        Level::Admin,
    );
    tools.push(set.tool_with(secret_set));
    let mut logs = spec(
        "actions_run_logs",
        "Get the download link of a workflow run's logs.",
        "GET",
        "",
    );
    (logs.p, logs.scopes) = (RUN, &["repo"]);
    tools.push(logs.tool_with(run_logs).events(&["workflow_run"]));
    tools
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<ghmcp_core::Tool> {
    let mut t = hooks(false);
    t.extend(hooks(true));
    t.extend(actions());
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};

    const CFG: &str = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";

    #[test]
    fn scelle_et_rouvre_avec_cle_fictive() {
        let sk = crypto_box::SecretKey::generate(&mut crypto_box::aead::OsRng);
        let key = B64.encode(sk.public_key().as_bytes());
        let enc = seal(&key, "valeur-fictive").unwrap();
        let ct = B64.decode(&enc).unwrap();
        assert_eq!(ct.len(), 32 + 16 + "valeur-fictive".len()); // clé éphémère + tag + message
        assert_eq!(sk.unseal(&ct).unwrap(), b"valeur-fictive");
        assert_ne!(enc, seal(&key, "valeur-fictive").unwrap()); // clé éphémère : jamais deux fois pareil
        assert!(seal("pas-du-base64!", "x").is_err());
        assert!(seal(&B64.encode([1u8; 5]), "x").is_err());
    }

    #[test]
    fn outils_attendus_et_metadonnees() {
        let t = tools();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        for n in [
            "gh_repo_hook_redeliver",
            "gh_repo_hook_ping",
            "gh_org_hook_redeliver",
            "gh_org_hook_ping",
            "gh_repo_hook_deliveries",
            "gh_org_hook_delivery_get",
            "gh_actions_secret_set",
            "gh_actions_secrets_list",
            "gh_actions_variable_create",
            "gh_actions_runners_list",
            "gh_actions_runner_token",
            "gh_actions_run_rerun",
            "gh_actions_run_rerun_failed",
            "gh_actions_run_cancel",
            "gh_actions_run_logs",
            "gh_actions_permissions_set",
        ] {
            assert!(names.contains(&n), "outil manquant: {n}");
        }
        assert_eq!(names.len(), 18 + 17, "nombre d'outils");
        let mut u = names.clone();
        u.sort();
        u.dedup();
        assert_eq!(u.len(), names.len(), "noms en double");
        for t in &t {
            assert!(t.name.starts_with("gh_"));
            assert!(
                !t.classic_scopes.is_empty() && !t.fine_perms.is_empty(),
                "{} sans permissions",
                t.name
            );
            assert_eq!(
                t.needs_approval,
                t.name.contains("delete"),
                "approbation: {}",
                t.name
            );
        }
    }

    #[tokio::test]
    async fn tous_repondent_en_mode_test() {
        ghmcp_core::net::init(true, ghmcp_core::Approver::Approve);
        let ctx = Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus());
        let args = json!({"owner": "o-fictif", "repo": "r-fictif", "org": "org-fictive", "hook_id": 1, "delivery_id": 2, "run_id": 3,
            "secret_name": "S_FICTIF", "name": "V_FICTIVE", "value": "v", "runner_id": 4, "config": {"url": "https://example.invalid"}, "enabled": true});
        for t in tools() {
            let mut a = args.clone();
            if t.name.contains("org_hook") {
                a.as_object_mut().unwrap().remove("owner");
                a.as_object_mut().unwrap().remove("repo");
            }
            let r = (t.handler)(ctx.clone(), a)
                .await
                .unwrap_or_else(|e| panic!("{} : {e}", t.name));
            assert!(
                r.get("test_mode").is_some() || r.get("api_path").is_some(),
                "{} : {r}",
                t.name
            );
        }
        // secret d'org : chemin org, valeur chiffrée dans le corps
        let find = |n: &str| tools().into_iter().find(|t| t.name == n).unwrap().handler;
        let r = find("gh_actions_secret_set")(
            ctx.clone(),
            json!({"org": "org-fictive", "secret_name": "S", "value": "clair"}),
        )
        .await
        .unwrap();
        assert_eq!(r["path"], "/orgs/org-fictive/actions/secrets/S");
        assert_ne!(r["body"]["encrypted_value"], "clair");
        let e = find("gh_actions_secrets_list")(ctx, json!({}))
            .await
            .unwrap_err();
        assert_eq!(e.code(), "INVALID_ARGS");
    }
}
