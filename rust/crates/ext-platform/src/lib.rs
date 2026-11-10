//! ghmcp-ext-platform : outils étendus gh_ — paquets, environnements, déploiements, Pages, réglages de sécurité.
//! Contrat : voir l en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//!
//! Les outils sont décrits par une table de `Spec` (REST docs.github.com/rest) interprétée par un seul
//! handler : `{clé}` du chemin = argument obligatoire (URL-encodé), `q` = query, `b` = champs du body.
use ghmcp_core::{arg_bool, arg_str, req_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy)]
struct Spec {
    n: &'static str,
    d: &'static str,
    m: &'static str,
    p: &'static str,
    q: &'static [&'static str],
    b: &'static [&'static str],
    req_b: &'static [&'static str],
    lv: Level,
    sc: &'static [&'static str],
    pm: &'static str,
    confirm: bool,
    /// `enabled` (bool, obligatoire) choisit PUT (true) ou DELETE (false).
    toggle: bool,
}

const D: Spec = Spec {
    n: "",
    d: "",
    m: "GET",
    p: "",
    q: &[],
    b: &[],
    req_b: &[],
    lv: Level::Cicd,
    sc: &["repo"],
    pm: "",
    confirm: false,
    toggle: false,
};
const PK_R: &[&str] = &["read:packages"];
const PK_W: &[&str] = &["write:packages"];
const PK_D: &[&str] = &["delete:packages"];
const SEC: &[&str] = &["security_events"];
const PG: &[&str] = &["page", "perPage"];

const ENV: &str = "/repos/{owner}/{repo}/environments/{environment_name}";
const DEP: &str = "/repos/{owner}/{repo}/deployments";

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str()) // table construite une fois au démarrage
}

fn specs() -> Vec<Spec> {
    let mut v = vec![];
    // Paquets : même table pour le compte utilisateur (`username`) et l'organisation (`org`).
    for (who, base) in [("user", "/users/{username}"), ("org", "/orgs/{org}")] {
        let pk = |s: &str| leak(format!("{base}/packages{s}"));
        let name = |s: &str| leak(format!("gh_{who}_package{s}"));
        let pv = "/{package_type}/{package_name}";
        v.extend([
            Spec { n: leak(format!("gh_{who}_packages_list")), d: leak(format!("List packages of a {who} by package_type (npm, maven, rubygems, docker, nuget, container).")), p: pk(""), q: &["package_type", "visibility", "page", "perPage"], sc: PK_R, pm: "packages", ..D },
            Spec { n: name("_get"), d: leak(format!("Get one package of a {who}.")), p: pk(pv), sc: PK_R, pm: "packages", ..D },
            Spec { n: name("_versions_list"), d: leak(format!("List the versions of a package of a {who}.")), p: pk(&format!("{pv}/versions")), q: &["state", "page", "perPage"], sc: PK_R, pm: "packages", ..D },
            Spec { n: name("_version_get"), d: leak(format!("Get one package version of a {who}.")), p: pk(&format!("{pv}/versions/{{package_version_id}}")), sc: PK_R, pm: "packages", ..D },
            Spec { n: name("_delete"), d: leak(format!("Delete a whole package of a {who} (restorable for 30 days). Requires human approval.")), m: "DELETE", p: pk(pv), lv: Level::Admin, sc: PK_D, pm: "packages", confirm: true, ..D },
            Spec { n: name("_version_delete"), d: leak(format!("Delete one package version of a {who}. Requires human approval.")), m: "DELETE", p: pk(&format!("{pv}/versions/{{package_version_id}}")), lv: Level::Admin, sc: PK_D, pm: "packages", confirm: true, ..D },
            Spec { n: name("_restore"), d: leak(format!("Restore a deleted package of a {who}.")), m: "POST", p: pk(&format!("{pv}/restore")), q: &["token"], lv: Level::Admin, sc: PK_W, pm: "packages", ..D },
            Spec { n: name("_version_restore"), d: leak(format!("Restore a deleted package version of a {who}.")), m: "POST", p: pk(&format!("{pv}/versions/{{package_version_id}}/restore")), lv: Level::Admin, sc: PK_W, pm: "packages", ..D },
        ]);
    }
    v.extend([
        // Environnements
        Spec { n: "gh_environments_list", d: "List the environments of a repository.", p: "/repos/{owner}/{repo}/environments", q: PG, pm: "environments", ..D },
        Spec { n: "gh_environment_get", d: "Get one environment (protection rules included).", p: ENV, pm: "environments", ..D },
        Spec { n: "gh_environment_set", d: "Create or update an environment (wait_timer, reviewers, deployment_branch_policy, prevent_self_review).", m: "PUT", p: ENV, b: &["wait_timer", "prevent_self_review", "reviewers", "deployment_branch_policy"], lv: Level::Admin, pm: "environments", ..D },
        Spec { n: "gh_environment_delete", d: "Delete an environment. Requires human approval.", m: "DELETE", p: ENV, lv: Level::Admin, pm: "environments", confirm: true, ..D },
        Spec { n: "gh_environment_protection_rules_list", d: "List the custom deployment protection rules of an environment.", p: "/repos/{owner}/{repo}/environments/{environment_name}/deployment_protection_rules", pm: "environments", ..D },
        Spec { n: "gh_environment_protection_rule_create", d: "Enable a custom deployment protection rule (GitHub App integration_id) on an environment.", m: "POST", p: "/repos/{owner}/{repo}/environments/{environment_name}/deployment_protection_rules", b: &["integration_id"], req_b: &["integration_id"], lv: Level::Admin, pm: "environments", ..D },
        Spec { n: "gh_environment_protection_rule_delete", d: "Disable a custom deployment protection rule. Requires human approval.", m: "DELETE", p: "/repos/{owner}/{repo}/environments/{environment_name}/deployment_protection_rules/{protection_rule_id}", lv: Level::Admin, pm: "environments", confirm: true, ..D },
        Spec { n: "gh_environment_branch_policies_list", d: "List the deployment branch policies of an environment.", p: "/repos/{owner}/{repo}/environments/{environment_name}/deployment-branch-policies", q: PG, pm: "environments", ..D },
        Spec { n: "gh_environment_branch_policy_create", d: "Add a deployment branch/tag policy (name, type=branch|tag) to an environment.", m: "POST", p: "/repos/{owner}/{repo}/environments/{environment_name}/deployment-branch-policies", b: &["name", "type"], req_b: &["name"], lv: Level::Admin, pm: "environments", ..D },
        // Déploiements
        Spec { n: "gh_deployments_list", d: "List the deployments of a repository (filter sha, ref, task, environment).", p: DEP, q: &["sha", "ref", "task", "environment", "page", "perPage"], pm: "deployments", ..D },
        Spec { n: "gh_deployment_create", d: "Create a deployment for a ref.", m: "POST", p: DEP, b: &["ref", "task", "auto_merge", "required_contexts", "payload", "environment", "description", "transient_environment", "production_environment"], req_b: &["ref"], pm: "deployments", ..D },
        Spec { n: "gh_deployment_delete", d: "Delete an inactive deployment. Requires human approval.", m: "DELETE", p: "/repos/{owner}/{repo}/deployments/{deployment_id}", lv: Level::Admin, pm: "deployments", confirm: true, ..D },
        Spec { n: "gh_deployment_statuses_list", d: "List the statuses of a deployment.", p: "/repos/{owner}/{repo}/deployments/{deployment_id}/statuses", q: PG, pm: "deployments", ..D },
        Spec { n: "gh_deployment_status_create", d: "Create a deployment status (state: error, failure, inactive, in_progress, queued, pending, success).", m: "POST", p: "/repos/{owner}/{repo}/deployments/{deployment_id}/statuses", b: &["state", "log_url", "description", "environment", "environment_url", "auto_inactive"], req_b: &["state"], pm: "deployments", ..D },
        // Pages
        Spec { n: "gh_pages_get", d: "Get the GitHub Pages site of a repository.", p: "/repos/{owner}/{repo}/pages", pm: "pages", ..D },
        Spec { n: "gh_pages_create", d: "Create a GitHub Pages site (source {branch, path}, build_type legacy|workflow).", m: "POST", p: "/repos/{owner}/{repo}/pages", b: &["source", "build_type"], lv: Level::Admin, pm: "pages", ..D },
        Spec { n: "gh_pages_update", d: "Update the Pages source, cname, https_enforced or build_type.", m: "PUT", p: "/repos/{owner}/{repo}/pages", b: &["source", "cname", "https_enforced", "build_type"], lv: Level::Admin, pm: "pages", ..D },
        Spec { n: "gh_pages_build_request", d: "Request a Pages build.", m: "POST", p: "/repos/{owner}/{repo}/pages/builds", pm: "pages", ..D },
        Spec { n: "gh_pages_build_latest", d: "Get the latest Pages build.", p: "/repos/{owner}/{repo}/pages/builds/latest", pm: "pages", ..D },
        // Réglages de sécurité
        Spec { n: "gh_vulnerability_alerts_get", d: "Tell whether Dependabot alerts (vulnerability alerts) are enabled (404 = disabled).", p: "/repos/{owner}/{repo}/vulnerability-alerts", lv: Level::Securite, pm: "vulnerability_alerts", ..D },
        Spec { n: "gh_vulnerability_alerts_set", d: "Enable or disable Dependabot alerts (`enabled`).", m: "PUT", p: "/repos/{owner}/{repo}/vulnerability-alerts", toggle: true, lv: Level::Securite, pm: "vulnerability_alerts", ..D },
        Spec { n: "gh_dependabot_security_updates_get", d: "Get the Dependabot security updates state.", p: "/repos/{owner}/{repo}/automated-security-fixes", lv: Level::Securite, pm: "vulnerability_alerts", ..D },
        Spec { n: "gh_dependabot_security_updates_set", d: "Enable or disable Dependabot security updates (`enabled`).", m: "PUT", p: "/repos/{owner}/{repo}/automated-security-fixes", toggle: true, lv: Level::Securite, pm: "vulnerability_alerts", ..D },
        Spec { n: "gh_code_scanning_default_setup_get", d: "Get the code scanning default setup.", p: "/repos/{owner}/{repo}/code-scanning/default-setup", sc: SEC, lv: Level::Securite, pm: "code_scanning_alerts", ..D },
        Spec { n: "gh_code_scanning_default_setup_set", d: "Configure the code scanning default setup (state configured|not-configured, languages, query_suite).", m: "PATCH", p: "/repos/{owner}/{repo}/code-scanning/default-setup", b: &["state", "languages", "query_suite", "runner_type", "runner_label"], sc: SEC, lv: Level::Securite, pm: "code_scanning_alerts", ..D },
        Spec { n: "gh_dependabot_alert_update", d: "Dismiss or reopen a Dependabot alert (state open|dismissed, dismissed_reason, dismissed_comment).", m: "PATCH", p: "/repos/{owner}/{repo}/dependabot/alerts/{alert_number}", b: &["state", "dismissed_reason", "dismissed_comment"], req_b: &["state"], sc: SEC, lv: Level::Securite, pm: "dependabot_alerts", ..D },
        Spec { n: "gh_secret_scanning_alert_update", d: "Resolve or reopen a secret scanning alert (state open|resolved, resolution, resolution_comment).", m: "PATCH", p: "/repos/{owner}/{repo}/secret-scanning/alerts/{alert_number}", b: &["state", "resolution", "resolution_comment"], req_b: &["state"], sc: SEC, lv: Level::Securite, pm: "secret_scanning_alerts", ..D },
        Spec { n: "gh_code_scanning_alert_update", d: "Dismiss or reopen a code scanning alert (state open|dismissed, dismissed_reason, dismissed_comment).", m: "PATCH", p: "/repos/{owner}/{repo}/code-scanning/alerts/{alert_number}", b: &["state", "dismissed_reason", "dismissed_comment"], req_b: &["state"], sc: SEC, lv: Level::Securite, pm: "code_scanning_alerts", ..D },
    ]);
    v
}

/// Type JSON Schema d'un paramètre, déduit de son nom.
fn ty(k: &str) -> Value {
    match k {
        "page" | "perPage" | "package_version_id" | "deployment_id" | "protection_rule_id"
        | "alert_number" | "integration_id" | "wait_timer" => json!({"type": "integer"}),
        "auto_merge"
        | "transient_environment"
        | "production_environment"
        | "auto_inactive"
        | "prevent_self_review"
        | "https_enforced"
        | "enabled" => json!({"type": "boolean"}),
        "source" | "payload" | "deployment_branch_policy" => json!({"type": "object"}),
        "reviewers" | "required_contexts" | "languages" => json!({"type": "array", "items": {}}),
        _ => json!({"type": "string"}),
    }
}

/// Clés `{x}` d'un chemin.
fn path_keys(p: &str) -> Vec<&str> {
    p.split('{')
        .skip(1)
        .filter_map(|s| s.split('}').next())
        .collect()
}

fn schema(s: &Spec) -> Value {
    let pk = path_keys(s.p);
    let mut props = Map::new();
    for k in pk.iter().chain(s.q).chain(s.b) {
        props.insert((*k).into(), ty(k));
    }
    let mut req: Vec<&str> = pk;
    req.extend(s.req_b);
    if s.toggle {
        props.insert("enabled".into(), ty("enabled"));
        req.push("enabled");
    }
    json!({"type": "object", "properties": props, "required": req})
}

fn fill(path: &str, args: &Value) -> Result<String> {
    let mut out = path.to_string();
    for k in path_keys(path) {
        let v = match args.get(k) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(GhError::InvalidArgs(format!(
                    "paramètre requis manquant: {k}"
                )))
            }
        };
        out = out.replace(&format!("{{{k}}}"), &urlencoding::encode(&v));
    }
    Ok(out)
}

async fn run(ctx: Ctx, args: Value, s: Spec) -> Result<Value> {
    let path = fill(s.p, &args)?;
    let method = if s.toggle {
        let on = arg_bool(&args, "enabled")
            .ok_or_else(|| GhError::InvalidArgs("paramètre requis manquant: enabled".into()))?;
        if on {
            "PUT"
        } else {
            "DELETE"
        }
    } else {
        s.m
    };
    let mut q: Vec<(&str, String)> = vec![];
    for k in s.q {
        let name = if *k == "perPage" { "per_page" } else { k };
        match args.get(*k) {
            Some(Value::String(v)) if !v.is_empty() => q.push((name, v.clone())),
            Some(Value::Number(n)) => q.push((name, n.to_string())),
            _ => {}
        }
    }
    let mut body = Map::new();
    for k in s.b {
        if let Some(v) = args.get(*k).filter(|v| !v.is_null()) {
            body.insert((*k).into(), v.clone());
        }
    }
    for k in s.req_b {
        if !body.contains_key(*k) {
            return Err(GhError::InvalidArgs(format!(
                "paramètre requis manquant: {k}"
            )));
        }
    }
    let gh = ctx.gh(&args)?; // compte résolu avant la confirmation
    if s.confirm {
        ctx.confirm(
            &args,
            &format!(
                "{} {path} ?",
                s.d.trim_end_matches(" Requires human approval.")
            ),
        )
        .await?;
    }
    let body = (!body.is_empty()).then(|| Value::Object(body));
    gh.rest(method, &path, &q, body).await
}

/// `security_and_analysis` du dépôt : chaque champ vaut "enabled" ou "disabled".
const SA: &[&str] = &[
    "advanced_security",
    "secret_scanning",
    "secret_scanning_push_protection",
    "secret_scanning_non_provider_patterns",
    "dependabot_security_updates",
];

async fn security_analysis_set(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let mut sa = Map::new();
    for k in SA {
        if let Some(st) = arg_str(&args, k) {
            if st != "enabled" && st != "disabled" {
                return Err(GhError::InvalidArgs(format!(
                    "{k}: 'enabled' ou 'disabled' attendu"
                )));
            }
            sa.insert((*k).into(), json!({"status": st}));
        }
    }
    if sa.is_empty() {
        return Err(GhError::InvalidArgs(format!(
            "au moins un champ parmi: {}",
            SA.join(", ")
        )));
    }
    let gh = ctx.gh(&args)?;
    gh.rest(
        "PATCH",
        &format!(
            "/repos/{}/{}",
            urlencoding::encode(o),
            urlencoding::encode(r)
        ),
        &[],
        Some(json!({"security_and_analysis": sa})),
    )
    .await
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    let mut out: Vec<Tool> = specs()
        .into_iter()
        .map(|s| {
            let mut t = Tool::new(s.n, s.d, schema(&s), move |c, a| run(c, a, s))
                .annotations(json!({"readOnlyHint": s.m == "GET", "destructiveHint": s.m == "DELETE" || s.toggle}))
                .scopes(s.sc)
                .perm(s.pm, if s.m == "GET" { Access::Read } else { Access::Write })
                .level(s.lv);
            if s.confirm {
                t = t.approval();
            }
            t
        })
        .collect();
    let mut props = Map::new();
    props.insert("owner".into(), ty("owner"));
    props.insert("repo".into(), ty("repo"));
    for k in SA {
        props.insert(
            (*k).into(),
            json!({"type": "string", "enum": ["enabled", "disabled"]}),
        );
    }
    out.push(
        Tool::new(
            "gh_repo_security_analysis_set",
            "Enable or disable repository security features via security_and_analysis (secret scanning, push protection, advanced security, Dependabot security updates).",
            json!({"type": "object", "properties": props, "required": ["owner", "repo"]}),
            security_analysis_set,
        )
        .annotations(json!({"readOnlyHint": false, "destructiveHint": false}))
        .scopes(&["repo", "security_events"])
        .perm("administration", Access::Write)
        .level(Level::Admin),
    );
    out
}
