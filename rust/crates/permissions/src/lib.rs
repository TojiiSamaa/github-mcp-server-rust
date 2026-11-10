//! ghmcp-permissions : Outils gh_ : audit des permissions des comptes contre ctx.registry().
//! Contrat : voir l en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//!
//! Trois outils en lecture seule : `gh_permissions_schema`, `gh_account_audit`, `gh_setup_urls`.
//! Niveaux cumulatifs : le niveau N = union des besoins des outils de niveau <= N.

use ghmcp_core::{arg_str, Access, Ctx, GhError, Kind, Level, Result, Tool, ToolMeta};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const LEVELS: [(&str, Level); 5] = [
    ("lecture", Level::Lecture),
    ("dev", Level::Dev),
    ("cicd", Level::Cicd),
    ("securite", Level::Securite),
    ("admin", Level::Admin),
];
const LEVEL_ENUM: [&str; 5] = ["lecture", "dev", "cicd", "securite", "admin"];

fn level_name(l: Level) -> &'static str {
    LEVELS
        .iter()
        .find(|(_, x)| *x == l)
        .map(|(n, _)| *n)
        .unwrap_or("lecture")
}

fn parse_level(s: &str) -> Result<Level> {
    LEVELS
        .iter()
        .find(|(n, _)| *n == s)
        .map(|(_, l)| *l)
        .ok_or_else(|| {
            GhError::Other(format!(
                "niveau inconnu: {s} (lecture|dev|cicd|securite|admin)"
            ))
        })
}

fn acc(a: Access) -> &'static str {
    if a == Access::Write {
        "write"
    } else {
        "read"
    }
}

/// Besoins cumulés : scopes classiques, permissions fines (write l'emporte), événements.
struct Needs {
    scopes: BTreeSet<&'static str>,
    perms: BTreeMap<&'static str, Access>,
    events: BTreeSet<&'static str>,
}

fn up_to(reg: &[ToolMeta], max: Level) -> Needs {
    let mut n = Needs {
        scopes: BTreeSet::new(),
        perms: BTreeMap::new(),
        events: BTreeSet::new(),
    };
    for t in reg.iter().filter(|t| t.level <= max) {
        n.scopes.extend(t.classic_scopes.iter().copied());
        n.events.extend(t.app_events.iter().copied());
        for (p, a) in &t.fine_perms {
            let e = n.perms.entry(*p).or_insert(*a);
            if *a == Access::Write {
                *e = Access::Write;
            }
        }
    }
    n
}

fn perms_json(it: impl Iterator<Item = (&'static str, Access)>) -> Map<String, Value> {
    it.map(|(p, a)| (p.to_string(), json!(acc(a)))).collect()
}

fn needs_json(n: &Needs) -> Value {
    json!({"classic_scopes": n.scopes, "fine_permissions": perms_json(n.perms.iter().map(|(p, a)| (*p, *a))), "app_events": n.events})
}

// ---------- gh_permissions_schema ----------

fn schema_value(reg: &[ToolMeta], name: Option<&str>, level: Option<Level>) -> Value {
    let tools: Vec<Value> = reg
        .iter()
        .filter(|t| name.is_none_or(|n| t.name == n) && level.is_none_or(|l| t.level == l))
        .map(|t| json!({"name": t.name, "level": level_name(t.level), "classic_scopes": t.classic_scopes,
                        "fine_permissions": perms_json(t.fine_perms.iter().copied()), "app_events": t.app_events}))
        .collect();
    let levels: Map<String, Value> = LEVELS
        .iter()
        .map(|(n, l)| (n.to_string(), needs_json(&up_to(reg, *l))))
        .collect();
    json!({"tools": tools, "levels": levels, "note": "niveaux cumulatifs : union des besoins des outils de niveau <= N"})
}

async fn permissions_schema(ctx: Ctx, args: Value) -> Result<Value> {
    let level = arg_str(&args, "level").map(parse_level).transpose()?;
    Ok(schema_value(ctx.registry(), arg_str(&args, "name"), level))
}

// ---------- gh_account_audit ----------

/// Un scope accordé en couvre d'autres (hiérarchie documentée par GitHub).
const IMPLIES: &[(&str, &[&str])] = &[
    (
        "repo",
        &[
            "public_repo",
            "repo:status",
            "repo_deployment",
            "repo:invite",
            "security_events",
        ],
    ),
    (
        "admin:org",
        &["write:org", "read:org", "manage_runners:org"],
    ),
    ("write:org", &["read:org"]),
    ("admin:repo_hook", &["write:repo_hook", "read:repo_hook"]),
    ("write:repo_hook", &["read:repo_hook"]),
    ("user", &["read:user", "user:email", "user:follow"]),
    ("write:packages", &["read:packages"]),
    ("admin:gpg_key", &["write:gpg_key", "read:gpg_key"]),
    ("write:gpg_key", &["read:gpg_key"]),
    ("admin:public_key", &["write:public_key", "read:public_key"]),
    ("write:public_key", &["read:public_key"]),
    ("project", &["read:project"]),
];

fn scope_granted(granted: &BTreeSet<String>, need: &str) -> bool {
    granted.contains(need)
        || IMPLIES
            .iter()
            .any(|(g, covers)| granted.contains(*g) && covers.contains(&need))
}

fn parse_scopes(header: &str) -> BTreeSet<String> {
    header
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Jeton classique : outil par outil, les scopes qui manquent pour le niveau demandé.
fn audit_classic(reg: &[ToolMeta], level: Level, granted: &BTreeSet<String>) -> Value {
    let (mut missing, mut ok) = (Vec::new(), Vec::new());
    for t in reg.iter().filter(|t| t.level <= level) {
        let m: Vec<&str> = t
            .classic_scopes
            .iter()
            .copied()
            .filter(|s| !scope_granted(granted, s))
            .collect();
        if m.is_empty() {
            ok.push(json!(t.name))
        } else {
            missing.push(json!({"tool": t.name, "missing_scopes": m}))
        }
    }
    let all_missing: BTreeSet<&str> = up_to(reg, level)
        .scopes
        .into_iter()
        .filter(|s| !scope_granted(granted, s))
        .collect();
    json!({"granted_scopes": granted, "missing_scopes": all_missing, "tools_missing": missing, "tools_ok": ok})
}

/// Permission de dépôt -> chemin de sonde en lecture (relatif à /repos/{repo}).
fn probe_path(perm: &str) -> Option<&'static str> {
    Some(match perm {
        "metadata" => "",
        "contents" => "/contents",
        "issues" => "/issues",
        "pull_requests" => "/pulls",
        "actions" => "/actions/runs",
        "checks" => "/commits/HEAD/check-runs",
        "statuses" => "/commits/HEAD/statuses",
        "deployments" => "/deployments",
        "environments" => "/environments",
        "pages" => "/pages",
        "repository_hooks" => "/hooks",
        "security_events" => "/code-scanning/alerts",
        "secret_scanning_alerts" => "/secret-scanning/alerts",
        "vulnerability_alerts" => "/dependabot/alerts",
        _ => return None,
    })
}

/// Jeton fin / App : `granted` = permissions connues (sondées ou fournies par l'installation).
fn audit_perms(reg: &[ToolMeta], level: Level, granted: &BTreeMap<String, String>) -> Value {
    let mut missing = Vec::new();
    for t in reg.iter().filter(|t| t.level <= level) {
        let m: Vec<Value> = t
            .fine_perms
            .iter()
            .filter(|(p, a)| {
                !matches!(
                    (granted.get(*p).map(String::as_str), a),
                    (Some("write"), _) | (Some("read"), Access::Read)
                )
            })
            .map(|(p, a)| json!({"permission": p, "required": acc(*a), "granted": granted.get(*p)}))
            .collect();
        if !m.is_empty() {
            missing.push(json!({"tool": t.name, "missing_permissions": m}))
        }
    }
    json!({"granted_permissions": granted, "tools_missing": missing})
}

async fn account_audit(ctx: Ctx, args: Value) -> Result<Value> {
    let level = parse_level(arg_str(&args, "level").unwrap_or("lecture"))?;
    let gh = ctx.gh(&args)?;
    let reg = ctx.registry();
    let kind = gh.account.kind;
    let mut out = match kind {
        Kind::Classic => {
            // core::net::Raw rend les en-têtes en minuscules.
            let raw = gh.rest_raw("GET", "/user", &[], None).await?;
            match raw
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("x-oauth-scopes"))
            {
                Some((_, h)) => audit_classic(reg, level, &parse_scopes(h)),
                None => {
                    json!({"warning": "en-tête X-OAuth-Scopes absent (mode test ou jeton non classique) : scopes inconnus"})
                }
            }
        }
        Kind::Fine => {
            // Pas d'en-tête de scopes : on sonde la lecture sur `repo` ; l'écriture n'est pas sondable sans écrire.
            let (mut granted, mut unprobed) = (BTreeMap::new(), BTreeSet::new());
            let repo = arg_str(&args, "repo");
            for p in up_to(reg, level).perms.keys() {
                let (Some(repo), Some(sfx)) = (repo, probe_path(p)) else {
                    unprobed.insert(p.to_string());
                    continue;
                };
                match gh
                    .rest_raw(
                        "GET",
                        &format!("/repos/{repo}{sfx}"),
                        &[("per_page", "1".into())],
                        None,
                    )
                    .await
                {
                    Ok(_) => {
                        granted.insert(p.to_string(), "read".to_string());
                    }
                    Err(GhError::Http {
                        status: 401 | 403 | 404,
                        ..
                    }) => {}
                    Err(e) => return Err(e),
                }
            }
            let mut v = audit_perms(reg, level, &granted);
            v["note"] = json!("jeton fin : seule la lecture est sondée (paramètre repo=owner/nom) ; toute écriture requise est listée « manquante » car non vérifiable");
            v["not_probed"] = json!(unprobed);
            v
        }
        Kind::App => {
            let repos = gh
                .rest(
                    "GET",
                    "/installation/repositories",
                    &[("per_page", "100".into())],
                    None,
                )
                .await?;
            let granted: BTreeMap<String, String> = repos
                .get("permissions")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let mut v = audit_perms(reg, level, &granted);
            v["repositories"] =
                json!(repos
                    .get("repositories")
                    .and_then(Value::as_array)
                    .map(|a| a
                        .iter()
                        .filter_map(|r| r.get("full_name"))
                        .collect::<Vec<_>>()));
            v["note"] = json!("permissions d'installation lues si présentes dans la réponse (GitHub ne les expose pas avec le jeton d'installation) ; sinon les consulter sur la page de l'installation");
            v
        }
    };
    out["account"] = json!(gh.account_name());
    out["kind"] = json!(format!("{kind:?}").to_lowercase());
    out["level"] = json!(level_name(level));
    Ok(out)
}

// ---------- gh_setup_urls ----------

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn classic_url(n: &Needs, desc: &str) -> String {
    let scopes: Vec<String> = n.scopes.iter().map(|s| enc(s)).collect();
    format!(
        "https://github.com/settings/tokens/new?scopes={}&description={}",
        scopes.join(","),
        enc(desc)
    )
}

/// Hypothèse (docs GitHub « pre-filling fine-grained personal access token details ») : paramètres
/// `name`, `description`, `target_name`, `expires_in` (jours) et une clé par permission (`issues=read`).
fn fine_url(n: &Needs, name: &str, desc: &str, owner: Option<&str>) -> String {
    let mut u = format!(
        "https://github.com/settings/personal-access-tokens/new?name={}&description={}",
        enc(name),
        enc(desc)
    );
    if let Some(o) = owner {
        u += &format!("&target_name={}", enc(o));
    }
    u += "&expires_in=90";
    for (p, a) in &n.perms {
        u += &format!("&{p}={}", acc(*a));
    }
    u
}

fn manifest(n: &Needs, name: &str, owner: Option<&str>, webhook: Option<&str>) -> Value {
    let mut perms = perms_json(n.perms.iter().map(|(p, a)| (*p, *a)));
    perms.entry("metadata").or_insert(json!("read"));
    let mut m = json!({
        "name": name,
        "url": owner.map_or("https://github.com".to_string(), |o| format!("https://github.com/{o}")),
        // shortcut: redirect local fixe, à remplacer par le récepteur réel de la conversion du manifeste.
        "redirect_url": "http://localhost:3000/ghmcp/app-created",
        "public": false,
        "default_permissions": perms,
        "default_events": n.events,
    });
    if let Some(w) = webhook {
        m["hook_attributes"] = json!({"url": w});
    }
    m
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Le flux manifeste exige un POST de formulaire : page auto-soumise.
fn manifest_form(action: &str, manifest: &Value) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Créer la GitHub App</title>\n<form id=\"f\" method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"manifest\" value=\"{}\"><noscript><button>Créer la GitHub App</button></noscript></form>\n<script>document.getElementById('f').submit()</script>\n",
        html_escape(action),
        html_escape(&manifest.to_string())
    )
}

fn setup_urls_value(
    reg: &[ToolMeta],
    level: Level,
    desc: &str,
    owner: Option<&str>,
    webhook: Option<&str>,
) -> Value {
    let n = up_to(reg, level);
    let name = format!("ghmcp-{}", level_name(level));
    let man = manifest(&n, &name, owner, webhook);
    let app_url = match owner {
        Some(o) => format!(
            "https://github.com/organizations/{}/settings/apps/new",
            enc(o)
        ),
        None => "https://github.com/settings/apps/new".to_string(),
    };
    json!({
        "level": level_name(level),
        "classic_token_url": classic_url(&n, desc),
        "fine_token_url": fine_url(&n, &name, desc, owner),
        "github_app": {
            "manifest": man,
            "manifest_get_url": format!("{app_url}?manifest={}", enc(&man.to_string())),
            "manifest_form_html": manifest_form(&app_url, &man),
            "note": "le flux manifeste exige un POST de formulaire : ouvrir manifest_form_html dans un navigateur (la variante GET est indicative)"
        }
    })
}

async fn setup_urls(ctx: Ctx, args: Value) -> Result<Value> {
    let level = parse_level(ghmcp_core::req_str(&args, "level")?)?;
    Ok(setup_urls_value(
        ctx.registry(),
        level,
        arg_str(&args, "description").unwrap_or("ghmcp"),
        arg_str(&args, "owner"),
        arg_str(&args, "webhook_url"),
    ))
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    let ro = json!({"readOnlyHint": true});
    vec![
        Tool::new(
            "gh_permissions_schema",
            "For each tool (optional filter by name or level): fine-grained permission and classic scope required; plus the full cumulative schema per level (lecture, dev, cicd, securite, admin).",
            json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "Only this tool."},
                "level": {"type": "string", "enum": LEVEL_ENUM, "description": "Only tools of this level."}}}),
            permissions_schema,
        )
        .annotations(ro.clone())
        .level(Level::Lecture),
        Tool::new(
            "gh_account_audit",
            "Compare an account's real permissions (classic: X-OAuth-Scopes; fine: read probes on `repo`; App: installation) with a level's needs and list what is missing, tool by tool.",
            json!({"type": "object", "properties": {
                "level": {"type": "string", "enum": LEVEL_ENUM, "description": "Level to check (default lecture)."},
                "repo": {"type": "string", "description": "owner/name used to probe a fine-grained token."}}}),
            account_audit,
        )
        .annotations(ro.clone())
        .perm("metadata", Access::Read)
        .level(Level::Lecture),
        Tool::new(
            "gh_setup_urls",
            "Pre-filled creation URLs for a level: classic token, fine-grained token, and GitHub App manifest (JSON + auto-submitting HTML form).",
            json!({"type": "object", "required": ["level"], "properties": {
                "level": {"type": "string", "enum": LEVEL_ENUM},
                "description": {"type": "string"},
                "owner": {"type": "string", "description": "User or organization (target / App owner)."},
                "webhook_url": {"type": "string"}}}),
            setup_urls,
        )
        .annotations(ro)
        .level(Level::Lecture),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::Config;
    use std::sync::Arc;

    fn reg() -> Vec<ToolMeta> {
        let h = |_c: Ctx, _a: Value| async { Ok(json!(null)) };
        vec![
            Tool::new("gh_a", "a", json!({}), h)
                .scopes(&["repo"])
                .perm("contents", Access::Read)
                .level(Level::Lecture)
                .meta(),
            Tool::new("gh_b", "b", json!({}), h)
                .scopes(&["repo"])
                .perm("pull_requests", Access::Write)
                .events(&["pull_request"])
                .level(Level::Dev)
                .meta(),
            Tool::new("gh_c", "c", json!({}), h)
                .scopes(&["workflow"])
                .perm("workflows", Access::Write)
                .perm("actions", Access::Write)
                .events(&["workflow_run"])
                .level(Level::Cicd)
                .meta(),
            Tool::new("gh_d", "d", json!({}), h)
                .scopes(&["security_events"])
                .perm("security_events", Access::Read)
                .level(Level::Securite)
                .meta(),
        ]
    }

    fn ctx() -> Ctx {
        let cfg = "default_account = \"t\"\n[[accounts]]\nname = \"t\"\nkind = \"classic\"\ntoken_env = \"GHMCP_T11_NONE\"\n";
        let c = Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), ghmcp_core::bus());
        c.set_registry(reg());
        c
    }

    fn query(url: &str) -> (String, BTreeMap<String, String>) {
        let (base, q) = url.split_once('?').unwrap();
        let m = q
            .split('&')
            .map(|kv| kv.split_once('=').unwrap())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        (base.to_string(), m)
    }

    #[test]
    fn niveaux_cumulatifs() {
        let r = reg();
        assert!(
            up_to(&r, Level::Lecture).scopes.contains("repo")
                && !up_to(&r, Level::Lecture).scopes.contains("workflow")
        );
        let cicd = up_to(&r, Level::Cicd);
        assert!(
            cicd.scopes.contains("workflow")
                && cicd.scopes.contains("repo")
                && !cicd.scopes.contains("security_events")
        );
        assert_eq!(cicd.perms["pull_requests"], Access::Write);
    }

    #[tokio::test]
    async fn schema_outil_et_niveaux() {
        let v = permissions_schema(ctx(), json!({"name": "gh_c"}))
            .await
            .unwrap();
        assert_eq!(v["tools"].as_array().unwrap().len(), 1);
        assert_eq!(v["tools"][0]["fine_permissions"]["actions"], "write");
        assert!(v["levels"]["cicd"]["classic_scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("workflow")));
        assert_eq!(v["levels"].as_object().unwrap().len(), 5);
        let v = permissions_schema(ctx(), json!({"level": "dev"}))
            .await
            .unwrap();
        assert_eq!(v["tools"][0]["name"], "gh_b");
        assert!(permissions_schema(ctx(), json!({"level": "x"}))
            .await
            .is_err());
    }

    #[test]
    fn audit_detecte_scope_manquant() {
        let v = audit_classic(&reg(), Level::Cicd, &parse_scopes("repo, read:org"));
        assert_eq!(v["missing_scopes"], json!(["workflow"]));
        assert_eq!(v["tools_missing"][0]["tool"], "gh_c");
        assert_eq!(v["tools_ok"], json!(["gh_a", "gh_b"]));
        // repo couvre security_events : rien ne manque avec repo+workflow au niveau securite
        let v = audit_classic(&reg(), Level::Securite, &parse_scopes("repo,workflow"));
        assert_eq!(v["missing_scopes"], json!([]));
    }

    #[test]
    fn audit_permissions_fines() {
        let g: BTreeMap<String, String> = [
            ("contents".to_string(), "read".to_string()),
            ("pull_requests".to_string(), "read".to_string()),
        ]
        .into();
        let v = audit_perms(&reg(), Level::Dev, &g);
        assert_eq!(v["tools_missing"].as_array().unwrap().len(), 1);
        assert_eq!(
            v["tools_missing"][0]["missing_permissions"][0]["required"],
            "write"
        );
    }

    #[tokio::test]
    async fn audit_en_mode_test_repond() {
        let v = account_audit(ctx(), json!({"level": "dev"})).await.unwrap();
        assert_eq!(v["account"], "t");
        assert!(v["warning"].as_str().unwrap().contains("X-OAuth-Scopes"));
    }

    #[test]
    fn urls_classique_et_fine() {
        let v = setup_urls_value(&reg(), Level::Cicd, "mon jeton", Some("exemple-org"), None);
        let (base, q) = query(v["classic_token_url"].as_str().unwrap());
        assert_eq!(base, "https://github.com/settings/tokens/new");
        assert_eq!(q["scopes"], "repo,workflow");
        assert_eq!(q["description"], "mon%20jeton");
        let (base, q) = query(v["fine_token_url"].as_str().unwrap());
        assert_eq!(
            base,
            "https://github.com/settings/personal-access-tokens/new"
        );
        assert_eq!(
            (
                q["target_name"].as_str(),
                q["pull_requests"].as_str(),
                q["actions"].as_str(),
                q["expires_in"].as_str()
            ),
            ("exemple-org", "write", "write", "90")
        );
        assert!(!q.contains_key("security_events"));
    }

    #[test]
    fn manifeste_app_valide() {
        let v = setup_urls_value(
            &reg(),
            Level::Cicd,
            "d",
            Some("exemple-org"),
            Some("https://exemple.invalid/hook"),
        );
        let app = &v["github_app"];
        let m: Value = serde_json::from_str(&app["manifest"].to_string()).unwrap();
        assert_eq!(m["name"], "ghmcp-cicd");
        assert_eq!(m["public"], false);
        assert_eq!(m["hook_attributes"]["url"], "https://exemple.invalid/hook");
        assert_eq!(m["default_permissions"]["pull_requests"], "write");
        assert_eq!(m["default_permissions"]["metadata"], "read");
        assert_eq!(m["default_events"], json!(["pull_request", "workflow_run"]));
        assert!(app["manifest_get_url"].as_str().unwrap().starts_with(
            "https://github.com/organizations/exemple-org/settings/apps/new?manifest="
        ));
        let html = app["manifest_form_html"].as_str().unwrap();
        assert!(
            html.contains("method=\"post\"")
                && html.contains("name=\"manifest\"")
                && html.contains("submit()")
                && html.contains("&quot;ghmcp-cicd&quot;")
        );
    }

    #[test]
    fn trois_outils_en_lecture_seule() {
        let t = tools();
        assert_eq!(
            t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["gh_permissions_schema", "gh_account_audit", "gh_setup_urls"]
        );
        assert!(t.iter().all(|t| t.annotations["readOnlyHint"] == true));
    }
}
