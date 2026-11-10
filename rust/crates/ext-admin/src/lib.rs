//! ghmcp-ext-admin : outils étendus gh_ d'administration (réglages de dépôt, protections, rulesets,
//! collaborateurs, invitations, organisations, membres, équipes). Contrat : en-tête de crates/core/src/lib.rs.
//!
//! Un outil = une ressource + un paramètre `action`. Chaque outil a une fonction `plan` qui traduit les
//! arguments en un appel REST (`Call`) ; `run` fait le reste (compte, confirmation, appel, pagination).

use ghmcp_core::{arg_bool, arg_i64, req_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Map, Value};

/// Appel REST décrit par un `plan`.
struct Call {
    method: &'static str,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    confirm: Option<String>,
    list: bool,
}

type Plan = fn(&Value) -> Result<Call>;

fn call(method: &'static str, path: String) -> Call {
    Call {
        method,
        path,
        query: vec![],
        body: None,
        confirm: None,
        list: false,
    }
}
fn get(path: String) -> Call {
    call("GET", path)
}
fn list(path: String) -> Call {
    Call {
        list: true,
        ..get(path)
    }
}
impl Call {
    /// Copie en paramètres de requête les clés présentes dans `a`.
    fn q(mut self, a: &Value, keys: &[&'static str]) -> Self {
        for k in keys {
            match a.get(*k) {
                Some(Value::String(s)) => self.query.push((k, s.clone())),
                Some(v @ (Value::Bool(_) | Value::Number(_))) => {
                    self.query.push((k, v.to_string()))
                }
                _ => {}
            }
        }
        self
    }
    fn body(mut self, a: &Value, keys: &[&str]) -> Self {
        self.body = Some(pick(a, keys));
        self
    }
    fn ask(mut self, msg: String) -> Self {
        self.confirm = Some(msg);
        self
    }
}

/// Objet JSON des seules clés présentes (non nulles) dans `a`.
fn pick(a: &Value, keys: &[&str]) -> Value {
    let m: Map<String, Value> = keys
        .iter()
        .filter_map(|k| {
            a.get(*k)
                .filter(|v| !v.is_null())
                .map(|v| (k.to_string(), v.clone()))
        })
        .collect();
    Value::Object(m)
}

fn e(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}
/// Nom de branche : les `/` restent des séparateurs.
fn br(s: &str) -> String {
    s.split('/').map(e).collect::<Vec<_>>().join("/")
}
/// Argument texte requis, encodé pour un segment d'URL.
fn s(a: &Value, k: &str) -> Result<String> {
    Ok(e(req_str(a, k)?))
}
/// `/repos/{owner}/{repo}`
fn repo(a: &Value) -> Result<String> {
    Ok(format!("/repos/{}/{}", s(a, "owner")?, s(a, "repo")?))
}
/// `/orgs/{org}`
fn org(a: &Value) -> Result<String> {
    Ok(format!("/orgs/{}", s(a, "org")?))
}
/// `/orgs/{org}/teams/{team_slug}`
fn team(a: &Value) -> Result<String> {
    Ok(format!("{}/teams/{}", org(a)?, s(a, "team_slug")?))
}
fn id(a: &Value, k: &str) -> Result<String> {
    arg_i64(a, k)
        .map(|n| n.to_string())
        .ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))
}
fn bad<T>(tool: &str, action: &str) -> Result<T> {
    Err(GhError::InvalidArgs(format!(
        "{tool}: action inconnue: {action}"
    )))
}

async fn run(ctx: Ctx, args: Value, plan: Plan) -> Result<Value> {
    let gh = ctx.gh(&args)?; // compte résolu avant la confirmation
    let c = plan(&args)?;
    if let Some(m) = &c.confirm {
        ctx.confirm(&args, m).await?;
    }
    if c.list {
        Ok(Value::Array(gh.paginate(&c.path, &c.query, None, 5).await?))
    } else {
        gh.rest(c.method, &c.path, &c.query, c.body).await
    }
}

fn ty(name: &str) -> Value {
    match name {
        "private"
        | "has_issues"
        | "has_projects"
        | "has_wiki"
        | "allow_squash_merge"
        | "allow_merge_commit"
        | "allow_rebase_merge"
        | "allow_auto_merge"
        | "delete_branch_on_merge"
        | "allow_update_branch"
        | "archived"
        | "web_commit_signoff_required"
        | "enforce_admins"
        | "required_linear_history"
        | "allow_force_pushes"
        | "allow_deletions"
        | "required_conversation_resolution"
        | "block_creations"
        | "lock_branch"
        | "allow_fork_syncing"
        | "strict"
        | "dismiss_stale_reviews"
        | "require_code_owner_reviews"
        | "require_last_push_approval"
        | "members_can_create_repositories"
        | "has_organization_projects"
        | "has_repository_projects" => {
            json!({"type": "boolean"})
        }
        "invitation_id"
        | "ruleset_id"
        | "invitee_id"
        | "parent_team_id"
        | "required_approving_review_count" => json!({"type": "integer"}),
        "required_status_checks"
        | "required_pull_request_reviews"
        | "restrictions"
        | "conditions"
        | "dismissal_restrictions"
        | "bypass_pull_request_allowances" => json!({"type": ["object", "null"]}),
        "names" | "bypass_actors" | "rules" | "team_ids" | "contexts" | "checks" => {
            json!({"type": "array"})
        }
        _ => json!({"type": "string"}),
    }
}

/// Schéma commun : `action` (enum) + paramètres listés (types dans `ty`). Les requis par action sont validés dans `plan`.
fn schema(actions: &[&str], names: &[&str]) -> Value {
    let mut p = Map::new();
    p.insert("action".into(), json!({"type": "string", "enum": actions}));
    for n in names {
        p.insert(n.to_string(), ty(n));
    }
    json!({"type": "object", "properties": p, "required": ["action"]})
}

/// Construit un outil admin. `destructive` = au moins une action confirmée (`approval`).
fn tool(
    name: &str,
    desc: &str,
    actions: &[&str],
    names: &[&str],
    plan: Plan,
    destructive: bool,
) -> Tool {
    let t = Tool::new(name, desc, schema(actions, names), move |c, a| {
        run(c, a, plan)
    })
    .level(Level::Admin)
    .annotations(json!({"readOnlyHint": false, "destructiveHint": destructive}));
    if destructive {
        t.approval()
    } else {
        t
    }
}

// ---------- dépôt ----------

const SETTINGS: &[&str] = &[
    "name",
    "description",
    "homepage",
    "private",
    "visibility",
    "has_issues",
    "has_projects",
    "has_wiki",
    "default_branch",
    "allow_squash_merge",
    "allow_merge_commit",
    "allow_rebase_merge",
    "allow_auto_merge",
    "delete_branch_on_merge",
    "allow_update_branch",
    "archived",
    "squash_merge_commit_title",
    "squash_merge_commit_message",
    "merge_commit_title",
    "merge_commit_message",
    "web_commit_signoff_required",
];

fn plan_repo_settings(a: &Value) -> Result<Call> {
    match req_str(a, "action")? {
        "get" => Ok(get(repo(a)?)),
        "update" => {
            let r = format!("{}/{}", req_str(a, "owner")?, req_str(a, "repo")?);
            let c = call("PATCH", repo(a)?).body(a, SETTINGS);
            // Visibilité et archivage : effet immédiat sur les tiers, donc confirmés.
            let risky = a.get("visibility").is_some()
                || a.get("private").is_some()
                || arg_bool(a, "archived") == Some(true);
            Ok(if risky {
                c.ask(format!("Changer la visibilité ou archiver {r} ?"))
            } else {
                c
            })
        }
        x => bad("gh_repo_settings", x),
    }
}

fn plan_repo_topics(a: &Value) -> Result<Call> {
    match req_str(a, "action")? {
        "get" => Ok(get(format!("{}/topics", repo(a)?))),
        "set" => Ok(call("PUT", format!("{}/topics", repo(a)?)).body(a, &["names"])),
        x => bad("gh_repo_topics", x),
    }
}

fn plan_repo_transfer(a: &Value) -> Result<Call> {
    let new_owner = req_str(a, "new_owner")?;
    let r = format!("{}/{}", req_str(a, "owner")?, req_str(a, "repo")?);
    Ok(call("POST", format!("{}/transfer", repo(a)?))
        .body(a, &["new_owner", "new_name", "team_ids"])
        .ask(format!("Transférer {r} vers {new_owner} ?")))
}

fn plan_protection(a: &Value) -> Result<Call> {
    let p = format!(
        "{}/branches/{}/protection",
        repo(a)?,
        br(req_str(a, "branch")?)
    );
    Ok(match req_str(a, "action")? {
        "get" => get(p),
        "set" => {
            // PUT exige ces 4 clés (nullables) ; le reste est optionnel.
            let mut b = json!({"required_status_checks": null, "enforce_admins": false, "required_pull_request_reviews": null, "restrictions": null});
            let extra = pick(
                a,
                &[
                    "required_status_checks",
                    "enforce_admins",
                    "required_pull_request_reviews",
                    "restrictions",
                    "required_linear_history",
                    "allow_force_pushes",
                    "allow_deletions",
                    "required_conversation_resolution",
                    "block_creations",
                    "lock_branch",
                    "allow_fork_syncing",
                ],
            );
            if let (Some(bm), Value::Object(em)) = (b.as_object_mut(), extra) {
                bm.extend(em);
            }
            Call {
                body: Some(b),
                ..call("PUT", p)
            }
        }
        "delete" => call("DELETE", p).ask(format!(
            "Supprimer la protection de la branche {} ?",
            req_str(a, "branch")?
        )),
        "get_status_checks" => get(format!("{p}/required_status_checks")),
        "set_status_checks" => call("PATCH", format!("{p}/required_status_checks"))
            .body(a, &["strict", "contexts", "checks"]),
        "get_reviews" => get(format!("{p}/required_pull_request_reviews")),
        "set_reviews" => call("PATCH", format!("{p}/required_pull_request_reviews")).body(
            a,
            &[
                "required_approving_review_count",
                "dismiss_stale_reviews",
                "require_code_owner_reviews",
                "require_last_push_approval",
                "dismissal_restrictions",
                "bypass_pull_request_allowances",
            ],
        ),
        x => return bad("gh_branch_protection", x),
    })
}

fn plan_ruleset(a: &Value) -> Result<Call> {
    let base = match req_str(a, "scope")? {
        "repo" => format!("{}/rulesets", repo(a)?),
        "org" => format!("{}/rulesets", org(a)?),
        x => {
            return Err(GhError::InvalidArgs(format!(
                "scope doit valoir repo ou org: {x}"
            )))
        }
    };
    let fields = [
        "name",
        "target",
        "enforcement",
        "bypass_actors",
        "conditions",
        "rules",
    ];
    Ok(match req_str(a, "action")? {
        "list" => list(base),
        "get" => get(format!("{base}/{}", id(a, "ruleset_id")?)),
        "create" => {
            req_str(a, "name")?;
            req_str(a, "enforcement")?;
            call("POST", base).body(a, &fields)
        }
        "update" => call("PUT", format!("{base}/{}", id(a, "ruleset_id")?)).body(a, &fields),
        "delete" => {
            let i = id(a, "ruleset_id")?;
            call("DELETE", format!("{base}/{i}")).ask(format!("Supprimer le ruleset {i} ?"))
        }
        x => return bad("gh_ruleset", x),
    })
}

fn plan_collaborator(a: &Value) -> Result<Call> {
    let base = format!("{}/collaborators", repo(a)?);
    Ok(match req_str(a, "action")? {
        "list" => list(base).q(a, &["affiliation", "permission"]),
        "permission" => get(format!("{base}/{}/permission", s(a, "username")?)),
        "add" => call("PUT", format!("{base}/{}", s(a, "username")?)).body(a, &["permission"]),
        "remove" => {
            let u = req_str(a, "username")?;
            call("DELETE", format!("{base}/{}", e(u))).ask(format!(
                "Retirer {u} des collaborateurs de {}/{} ?",
                req_str(a, "owner")?,
                req_str(a, "repo")?
            ))
        }
        x => return bad("gh_collaborator", x),
    })
}

fn plan_repo_invitation(a: &Value) -> Result<Call> {
    let base = format!("{}/invitations", repo(a)?);
    Ok(match req_str(a, "action")? {
        "list" => list(base),
        "update" => {
            call("PATCH", format!("{base}/{}", id(a, "invitation_id")?)).body(a, &["permissions"])
        }
        "cancel" => {
            let i = id(a, "invitation_id")?;
            call("DELETE", format!("{base}/{i}")).ask(format!("Annuler l'invitation {i} ?"))
        }
        x => return bad("gh_repo_invitation", x),
    })
}

/// Invitations REÇUES par l'utilisateur du compte.
fn plan_my_invitation(a: &Value) -> Result<Call> {
    Ok(match req_str(a, "action")? {
        "list" => list("/user/repository_invitations".into()),
        "accept" => call(
            "PATCH",
            format!("/user/repository_invitations/{}", id(a, "invitation_id")?),
        ),
        "decline" => call(
            "DELETE",
            format!("/user/repository_invitations/{}", id(a, "invitation_id")?),
        ),
        x => return bad("gh_my_invitation", x),
    })
}

fn plan_org_invitation(a: &Value) -> Result<Call> {
    Ok(match req_str(a, "action")? {
        "list" => list(format!("{}/invitations", org(a)?)),
        "list_failed" => list(format!("{}/failed_invitations", org(a)?)),
        "invite" => {
            if a.get("invitee_id").is_none() {
                req_str(a, "email")?;
            }
            call("POST", format!("{}/invitations", org(a)?))
                .body(a, &["invitee_id", "email", "role", "team_ids"])
        }
        "cancel" => {
            let i = id(a, "invitation_id")?;
            call("DELETE", format!("{}/invitations/{i}", org(a)?))
                .ask(format!("Annuler l'invitation d'organisation {i} ?"))
        }
        "list_received" => list("/user/memberships/orgs".into()).q(a, &["state"]),
        "accept" => Call {
            body: Some(json!({"state": "active"})),
            ..call("PATCH", format!("/user/memberships/orgs/{}", s(a, "org")?))
        },
        x => return bad("gh_org_invitation", x),
    })
}

fn plan_org(a: &Value) -> Result<Call> {
    Ok(match req_str(a, "action")? {
        "list_mine" => list("/user/orgs".into()),
        "get" => get(org(a)?),
        "update" => call("PATCH", org(a)?).body(a, ORG_FIELDS),
        x => return bad("gh_org", x),
    })
}

const ORG_FIELDS: &[&str] = &[
    "billing_email",
    "company",
    "email",
    "location",
    "name",
    "description",
    "blog",
    "default_repository_permission",
    "members_can_create_repositories",
    "has_organization_projects",
    "has_repository_projects",
];

fn plan_org_member(a: &Value) -> Result<Call> {
    let o = org(a)?;
    Ok(match req_str(a, "action")? {
        "list" => list(format!("{o}/members")).q(a, &["role", "filter"]),
        "list_outside" => list(format!("{o}/outside_collaborators")),
        "get" => get(format!("{o}/memberships/{}", s(a, "username")?)),
        "set_role" => {
            call("PUT", format!("{o}/memberships/{}", s(a, "username")?)).body(a, &["role"])
        }
        "remove" => {
            let u = req_str(a, "username")?;
            call("DELETE", format!("{o}/members/{}", e(u))).ask(format!(
                "Retirer {u} de l'organisation {} ?",
                req_str(a, "org")?
            ))
        }
        x => return bad("gh_org_member", x),
    })
}

fn plan_team(a: &Value) -> Result<Call> {
    let fields = [
        "name",
        "description",
        "privacy",
        "notification_setting",
        "permission",
        "parent_team_id",
    ];
    Ok(match req_str(a, "action")? {
        "list" => list(format!("{}/teams", org(a)?)),
        "get" => get(team(a)?),
        "create" => {
            req_str(a, "name")?;
            call("POST", format!("{}/teams", org(a)?)).body(a, &fields)
        }
        "update" => call("PATCH", team(a)?).body(a, &fields),
        "delete" => call("DELETE", team(a)?)
            .ask(format!("Supprimer l'équipe {} ?", req_str(a, "team_slug")?)),
        x => return bad("gh_team", x),
    })
}

fn plan_team_member(a: &Value) -> Result<Call> {
    let t = team(a)?;
    Ok(match req_str(a, "action")? {
        "list" => list(format!("{t}/members")).q(a, &["role"]),
        "get" => get(format!("{t}/memberships/{}", s(a, "username")?)),
        "add" => call("PUT", format!("{t}/memberships/{}", s(a, "username")?)).body(a, &["role"]),
        "remove" => {
            let u = req_str(a, "username")?;
            call("DELETE", format!("{t}/memberships/{}", e(u))).ask(format!(
                "Retirer {u} de l'équipe {} ?",
                req_str(a, "team_slug")?
            ))
        }
        x => return bad("gh_team_member", x),
    })
}

fn plan_team_repo(a: &Value) -> Result<Call> {
    let t = team(a)?;
    let r = format!(
        "{t}/repos/{}/{}",
        s(a, "owner").unwrap_or_default(),
        s(a, "repo").unwrap_or_default()
    );
    Ok(match req_str(a, "action")? {
        "list" => list(format!("{t}/repos")),
        "add" => call(
            "PUT",
            format!("{t}/repos/{}/{}", s(a, "owner")?, s(a, "repo")?),
        )
        .body(a, &["permission"]),
        "remove" => call(
            "DELETE",
            format!("{t}/repos/{}/{}", s(a, "owner")?, s(a, "repo")?),
        )
        .ask(format!(
            "Retirer le dépôt {r} de l'équipe {} ?",
            req_str(a, "team_slug")?
        )),
        x => return bad("gh_team_repo", x),
    })
}

pub fn tools() -> Vec<Tool> {
    // Scopes classiques et permissions fines (écriture) appliqués à un outil.
    let with = |t: Tool, scopes: &[&'static str], perms: &[&'static str]| {
        perms
            .iter()
            .fold(t.scopes(scopes), |t, p| t.perm(p, Access::Write))
    };
    let repo_fields = ["owner", "repo"];
    let cat = |a: &[&'static str], b: &[&'static str]| [a, b].concat();
    vec![
        with(tool("gh_repo_settings", "Get or update repository settings (visibility, merge options, auto-merge, delete branch on merge, archive). Visibility/archive changes require approval.",
            &["get", "update"], &cat(&repo_fields, SETTINGS), plan_repo_settings, true), &["repo"], &["administration"]),
        with(tool("gh_repo_topics", "Get or replace the topics of a repository.", &["get", "set"], &["owner", "repo", "names"], plan_repo_topics, false), &["repo"], &["administration"]),
        with(tool("gh_repo_transfer", "Transfer a repository to another owner (hard to undo). Requires human approval.",
            &["transfer"], &["owner", "repo", "new_owner", "new_name", "team_ids"], plan_repo_transfer, true), &["repo"], &["administration"]),
        with(tool("gh_branch_protection", "Branch protection: get/set/delete, required status checks and required reviews. Delete requires approval.",
            &["get", "set", "delete", "get_status_checks", "set_status_checks", "get_reviews", "set_reviews"],
            &["owner", "repo", "branch", "required_status_checks", "enforce_admins", "required_pull_request_reviews", "restrictions", "required_linear_history", "allow_force_pushes", "allow_deletions", "required_conversation_resolution", "block_creations", "lock_branch", "allow_fork_syncing", "strict", "contexts", "checks", "required_approving_review_count", "dismiss_stale_reviews", "require_code_owner_reviews", "require_last_push_approval", "dismissal_restrictions", "bypass_pull_request_allowances"],
            plan_protection, true), &["repo"], &["administration"]),
        with(tool("gh_ruleset", "Repository or organization rulesets (scope=repo|org): list/get/create/update/delete. Delete requires approval.",
            &["list", "get", "create", "update", "delete"], &["scope", "owner", "repo", "org", "ruleset_id", "name", "target", "enforcement", "bypass_actors", "conditions", "rules"],
            plan_ruleset, true), &["repo", "admin:org"], &["administration", "organization_administration"]),
        with(tool("gh_collaborator", "Repository collaborators: list, check permission, add with a permission, remove (requires approval).",
            &["list", "permission", "add", "remove"], &["owner", "repo", "username", "permission", "affiliation"], plan_collaborator, true), &["repo"], &["administration"]),
        with(tool("gh_repo_invitation", "Pending invitations of a repository: list, update permissions, cancel (requires approval).",
            &["list", "update", "cancel"], &["owner", "repo", "invitation_id", "permissions"], plan_repo_invitation, true), &["repo"], &["administration"]),
        // Invitations reçues : endpoints /user/*, pas de permission fine dédiée, jeton classique `repo`.
        tool("gh_my_invitation", "Repository invitations RECEIVED by the authenticated user: list, accept, decline.",
            &["list", "accept", "decline"], &["invitation_id"], plan_my_invitation, false).scopes(&["repo"]),
        with(tool("gh_org_invitation", "Organization invitations: list sent/failed, invite by user id or email, cancel (requires approval); list received memberships and accept one.",
            &["list", "list_failed", "invite", "cancel", "list_received", "accept"], &["org", "invitee_id", "email", "role", "team_ids", "invitation_id", "state"],
            plan_org_invitation, true), &["admin:org"], &["members"]),
        with(tool("gh_org", "Organizations: list mine, get, update settings.", &["list_mine", "get", "update"], &cat(&["org"], ORG_FIELDS), plan_org, false),
            &["admin:org"], &["organization_administration"]),
        with(tool("gh_org_member", "Organization members: list, outside collaborators, get membership, set role, remove (requires approval).",
            &["list", "list_outside", "get", "set_role", "remove"], &["org", "username", "role", "filter"], plan_org_member, true), &["admin:org"], &["members"]),
        with(tool("gh_team", "Organization teams: list/get/create/update/delete (delete requires approval).",
            &["list", "get", "create", "update", "delete"], &["org", "team_slug", "name", "description", "privacy", "notification_setting", "permission", "parent_team_id"],
            plan_team, true), &["admin:org"], &["members"]),
        with(tool("gh_team_member", "Team members: list, get, add with role, remove (requires approval).",
            &["list", "get", "add", "remove"], &["org", "team_slug", "username", "role"], plan_team_member, true), &["admin:org"], &["members"]),
        with(tool("gh_team_repo", "Team repositories: list, add with permission, remove (requires approval).",
            &["list", "add", "remove"], &["org", "team_slug", "owner", "repo", "permission"], plan_team_repo, true), &["admin:org"], &["members"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};
    use std::sync::Arc;

    const CFG: &str = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_ADMIN\"\n";

    fn ctx() -> Ctx {
        Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus())
    }

    /// (outil, arguments valides, l'action est confirmée)
    fn cas() -> Vec<(&'static str, Value, bool)> {
        let or = |v: Value| {
            let mut m = v.as_object().unwrap().clone();
            m.insert("owner".into(), json!("o"));
            m.insert("repo".into(), json!("r"));
            Value::Object(m)
        };
        let t = json!({"org": "acme-test", "team_slug": "devs"});
        let tm = |extra: Value| {
            let mut m = t.as_object().unwrap().clone();
            m.extend(extra.as_object().unwrap().clone());
            Value::Object(m)
        };
        vec![
            ("gh_repo_settings", or(json!({"action": "get"})), false),
            (
                "gh_repo_settings",
                or(
                    json!({"action": "update", "allow_auto_merge": true, "delete_branch_on_merge": true}),
                ),
                false,
            ),
            (
                "gh_repo_settings",
                or(json!({"action": "update", "visibility": "private"})),
                true,
            ),
            (
                "gh_repo_topics",
                or(json!({"action": "set", "names": ["rust", "mcp"]})),
                false,
            ),
            (
                "gh_repo_transfer",
                or(json!({"action": "transfer", "new_owner": "acme-test"})),
                true,
            ),
            (
                "gh_branch_protection",
                or(json!({"action": "get", "branch": "feature/x"})),
                false,
            ),
            (
                "gh_branch_protection",
                or(
                    json!({"action": "set", "branch": "main", "required_pull_request_reviews": {"required_approving_review_count": 1}}),
                ),
                false,
            ),
            (
                "gh_branch_protection",
                or(
                    json!({"action": "set_status_checks", "branch": "main", "strict": true, "contexts": ["ci"]}),
                ),
                false,
            ),
            (
                "gh_branch_protection",
                or(json!({"action": "delete", "branch": "main"})),
                true,
            ),
            (
                "gh_ruleset",
                or(json!({"action": "list", "scope": "repo"})),
                false,
            ),
            (
                "gh_ruleset",
                json!({"action": "create", "scope": "org", "org": "acme-test", "name": "r1", "enforcement": "active"}),
                false,
            ),
            (
                "gh_ruleset",
                json!({"action": "delete", "scope": "org", "org": "acme-test", "ruleset_id": 7}),
                true,
            ),
            (
                "gh_collaborator",
                or(json!({"action": "add", "username": "bob-test", "permission": "push"})),
                false,
            ),
            (
                "gh_collaborator",
                or(json!({"action": "remove", "username": "bob-test"})),
                true,
            ),
            ("gh_repo_invitation", or(json!({"action": "list"})), false),
            (
                "gh_repo_invitation",
                or(json!({"action": "cancel", "invitation_id": 3})),
                true,
            ),
            ("gh_my_invitation", json!({"action": "list"}), false),
            (
                "gh_my_invitation",
                json!({"action": "accept", "invitation_id": 3}),
                false,
            ),
            (
                "gh_my_invitation",
                json!({"action": "decline", "invitation_id": 3}),
                false,
            ),
            (
                "gh_org_invitation",
                json!({"action": "invite", "org": "acme-test", "email": "bob@example.invalid", "role": "direct_member"}),
                false,
            ),
            (
                "gh_org_invitation",
                json!({"action": "cancel", "org": "acme-test", "invitation_id": 4}),
                true,
            ),
            (
                "gh_org_invitation",
                json!({"action": "list_received"}),
                false,
            ),
            (
                "gh_org_invitation",
                json!({"action": "accept", "org": "acme-test"}),
                false,
            ),
            ("gh_org", json!({"action": "list_mine"}), false),
            (
                "gh_org",
                json!({"action": "update", "org": "acme-test", "description": "fictif"}),
                false,
            ),
            (
                "gh_org_member",
                json!({"action": "set_role", "org": "acme-test", "username": "bob-test", "role": "admin"}),
                false,
            ),
            (
                "gh_org_member",
                json!({"action": "remove", "org": "acme-test", "username": "bob-test"}),
                true,
            ),
            (
                "gh_team",
                tm(json!({"action": "create", "name": "Devs"})),
                false,
            ),
            ("gh_team", tm(json!({"action": "delete"})), true),
            (
                "gh_team_member",
                tm(json!({"action": "add", "username": "bob-test", "role": "member"})),
                false,
            ),
            (
                "gh_team_member",
                tm(json!({"action": "remove", "username": "bob-test"})),
                true,
            ),
            (
                "gh_team_repo",
                or(tm(json!({"action": "add", "permission": "push"}))),
                false,
            ),
            ("gh_team_repo", or(tm(json!({"action": "remove"}))), true),
        ]
    }

    #[test]
    fn noms_et_metadonnees() {
        let all = tools();
        let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
        for n in [
            "gh_my_invitation",
            "gh_collaborator",
            "gh_branch_protection",
            "gh_ruleset",
            "gh_org",
            "gh_team",
            "gh_org_invitation",
            "gh_repo_invitation",
            "gh_repo_settings",
        ] {
            assert!(names.contains(&n), "manque {n}");
        }
        assert_eq!(all.len(), 14);
        for t in &all {
            assert!(t.name.starts_with("gh_"));
            assert!(!t.classic_scopes.is_empty(), "{} sans scope", t.name);
            assert!(t.level >= Level::Dev);
        }
        // Chaque outil dont un cas est confirmé porte needs_approval.
        for (n, _, conf) in cas() {
            if conf {
                assert!(
                    all.iter().find(|t| t.name == n).unwrap().needs_approval,
                    "{n} sans approval"
                );
            }
        }
    }

    /// Chaque outil répond en mode test (non initialisé = fermé) ; les actions confirmées sont refusées (défaut deny).
    #[tokio::test]
    async fn chaque_outil_repond_ou_refuse_en_mode_test() {
        let all = tools();
        for (n, a, conf) in cas() {
            let t = all.iter().find(|t| t.name == n).unwrap();
            let r = (t.handler)(ctx(), a.clone()).await;
            if conf {
                assert_eq!(r.unwrap_err().code(), "DENIED", "{n} {a}");
            } else {
                assert!(
                    !r.unwrap_or_else(|e| panic!("{n} {a}: {e}")).is_null(),
                    "{n} vide"
                );
            }
        }
    }

    #[tokio::test]
    async fn parametres_invalides_refuses_avant_confirmation() {
        let all = tools();
        let t = |n: &str| all.iter().find(|t| t.name == n).unwrap();
        let r = (t("gh_collaborator").handler)(
            ctx(),
            json!({"action": "remove", "owner": "o", "repo": "r"}),
        )
        .await;
        assert_eq!(r.unwrap_err().code(), "INVALID_ARGS"); // username manquant : pas de demande de confirmation
        let r = (t("gh_team").handler)(ctx(), json!({"action": "zzz", "org": "a"})).await;
        assert_eq!(r.unwrap_err().code(), "INVALID_ARGS");
        let r = (t("gh_ruleset").handler)(ctx(), json!({"action": "list", "scope": "x"})).await;
        assert_eq!(r.unwrap_err().code(), "INVALID_ARGS");
        let r = (t("gh_org").handler)(
            ctx(),
            json!({"action": "get", "org": "a", "account": "mallory"}),
        )
        .await;
        assert_eq!(r.unwrap_err().code(), "UNKNOWN_ACCOUNT");
    }

    #[test]
    fn chemins_et_corps() {
        let c = plan_protection(&json!({"action": "set", "owner": "o", "repo": "r", "branch": "release/1.x", "enforce_admins": true})).unwrap();
        assert_eq!(
            (c.method, c.path.as_str()),
            ("PUT", "/repos/o/r/branches/release/1.x/protection")
        );
        let b = c.body.unwrap();
        assert_eq!(b["enforce_admins"], true);
        assert!(b["restrictions"].is_null() && b.get("restrictions").is_some());
        let c = plan_org_invitation(&json!({"action": "accept", "org": "acme-test"})).unwrap();
        assert_eq!(
            (c.method, c.path.as_str(), c.body),
            (
                "PATCH",
                "/user/memberships/orgs/acme-test",
                Some(json!({"state": "active"}))
            )
        );
        let c = plan_my_invitation(&json!({"action": "decline", "invitation_id": "9"})).unwrap();
        assert_eq!(
            (c.method, c.path.as_str()),
            ("DELETE", "/user/repository_invitations/9")
        );
    }
}
