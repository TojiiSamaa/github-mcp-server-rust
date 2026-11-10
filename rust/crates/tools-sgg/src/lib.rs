//! ghmcp-tools-sgg : outils officiels search, git, gists, contexte, notifications (19 outils).
//! Contrat : voir l'en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//! Porté de github-mcp-server/pkg/github/{search,git,gists,context_tools,notifications}.go.
//! Écart connu : search_code ne demande pas `text_matches` (le client REST du cœur n'accepte pas d'en-tête Accept).

use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Map, Value};

const READ: Access = Access::Read;

/// Garde seulement `keys` de chaque objet (équivalent des « Minimal* » du Go).
fn pick(v: &Value, keys: &[&str]) -> Value {
    let o: Map<String, Value> = keys
        .iter()
        .filter_map(|k| v.get(*k).map(|x| (k.to_string(), x.clone())))
        .collect();
    Value::Object(o)
}

fn pick_all(v: &Value, keys: &[&str]) -> Value {
    Value::Array(
        v.as_array()
            .map(|a| a.iter().map(|x| pick(x, keys)).collect())
            .unwrap_or_default(),
    )
}

/// page/perPage du schéma officiel -> page/per_page de l'API (défauts du Go : 1 / 30).
fn page_q(args: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("page", arg_i64(args, "page").unwrap_or(1).to_string()),
        (
            "per_page",
            arg_i64(args, "perPage").unwrap_or(30).to_string(),
        ),
    ]
}

fn rfc3339(args: &Value, k: &str) -> Result<Option<String>> {
    match arg_str(args, k).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|_| Some(s.to_string()))
            .map_err(|e| {
                GhError::InvalidArgs(format!(
                    "invalid {k} time format, should be RFC3339/ISO8601: {e}"
                ))
            }),
    }
}

fn opt<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    arg_str(args, k).filter(|s| !s.is_empty())
}

// ---------- search ----------

/// sort/order/pagination -> GET /search/<kind>.
async fn search(ctx: &Ctx, args: &Value, kind: &str, query: String) -> Result<Value> {
    let mut q = vec![("q", query)];
    for k in ["sort", "order"] {
        if let Some(s) = opt(args, k) {
            q.push((k, s.to_string()));
        }
    }
    q.extend(page_q(args));
    ctx.gh(args)?
        .rest("GET", &format!("/search/{kind}"), &q, None)
        .await
}

fn wrap(r: &Value, items: Value) -> Value {
    json!({"total_count": r["total_count"], "incomplete_results": r["incomplete_results"], "items": items})
}

fn query_of(args: &Value) -> Result<String> {
    opt(args, "query")
        .map(String::from)
        .ok_or_else(|| GhError::InvalidArgs("missing required parameter: query".into()))
}

async fn search_repositories(ctx: Ctx, args: Value) -> Result<Value> {
    let r = search(&ctx, &args, "repositories", query_of(&args)?).await?;
    if arg_bool(&args, "minimal_output") == Some(false) {
        return Ok(r);
    }
    let keys = [
        "id",
        "name",
        "full_name",
        "description",
        "html_url",
        "language",
        "stargazers_count",
        "forks_count",
        "open_issues_count",
        "updated_at",
        "created_at",
        "pushed_at",
        "private",
        "fork",
        "archived",
        "default_branch",
    ];
    Ok(wrap(&r, pick_all(&r["items"], &keys)))
}

async fn search_code(ctx: Ctx, args: Value) -> Result<Value> {
    let r = search(&ctx, &args, "code", query_of(&args)?).await?;
    let items: Vec<Value> = r["items"]
        .as_array()
        .map(|a| a.iter().map(|c| json!({"name": c["name"], "path": c["path"], "sha": c["sha"], "repository": c["repository"]["full_name"]})).collect())
        .unwrap_or_default();
    let fields: Vec<&str> = args
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let items = if fields.is_empty() {
        Value::Array(items)
    } else {
        pick_all(&Value::Array(items), &fields)
    };
    Ok(wrap(&r, items))
}

async fn search_commits(ctx: Ctx, args: Value) -> Result<Value> {
    let r = search(&ctx, &args, "commits", query_of(&args)?).await?;
    let items: Vec<Value> = r["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| json!({"sha": c["sha"], "html_url": c["html_url"], "commit": {"message": c["commit"]["message"], "author": c["commit"]["author"]}, "author": pick(&c["author"], &["login", "id", "html_url"]), "repository": c["repository"]["full_name"]}))
                .collect()
        })
        .unwrap_or_default();
    Ok(wrap(&r, Value::Array(items)))
}

/// users / orgs : même endpoint, filtre `type:` ajouté sauf si déjà présent.
async fn accounts(ctx: Ctx, args: Value, kind: &str) -> Result<Value> {
    let query = query_of(&args)?;
    let q = if has_type_filter(&query) {
        query
    } else {
        format!("type:{kind} {query}")
    };
    let r = search(&ctx, &args, "users", q).await?;
    let items: Vec<Value> = r["items"]
        .as_array()
        .map(|a| a.iter().filter(|u| u["login"].is_string()).map(|u| json!({"login": u["login"], "id": u["id"], "profile_url": u["html_url"], "avatar_url": u["avatar_url"]})).collect())
        .unwrap_or_default();
    Ok(wrap(&r, Value::Array(items)))
}

/// `type:\S+` en début de chaîne ou après un caractère non-mot (hasFilter du Go).
fn has_type_filter(q: &str) -> bool {
    q.match_indices("type:").any(|(i, _)| {
        let before_ok = q[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        before_ok
            && q[i + 5..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace())
    })
}

async fn search_users(ctx: Ctx, args: Value) -> Result<Value> {
    accounts(ctx, args, "user").await
}
async fn search_orgs(ctx: Ctx, args: Value) -> Result<Value> {
    accounts(ctx, args, "org").await
}

// ---------- git ----------

async fn get_repository_tree(ctx: Ctx, args: Value) -> Result<Value> {
    let (owner, repo) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let gh = ctx.gh(&args)?;
    let mut sha = opt(&args, "tree_sha").unwrap_or("").to_string();
    if sha.is_empty() {
        let info = gh
            .rest("GET", &format!("/repos/{owner}/{repo}"), &[], None)
            .await?;
        sha = info["default_branch"]
            .as_str()
            .unwrap_or("HEAD")
            .to_string();
    }
    let recursive = arg_bool(&args, "recursive").unwrap_or(false);
    let q: Vec<(&str, String)> = if recursive {
        vec![("recursive", "1".into())]
    } else {
        vec![]
    };
    let tree = gh
        .rest(
            "GET",
            &format!("/repos/{owner}/{repo}/git/trees/{sha}"),
            &q,
            None,
        )
        .await?;
    let prefix = opt(&args, "path_filter").unwrap_or("");
    let entries: Vec<Value> = tree["tree"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|e| e["path"].as_str().unwrap_or("").starts_with(prefix))
                .map(|e| pick(e, &["path", "type", "mode", "sha", "size", "url"]))
                .collect()
        })
        .unwrap_or_default();
    Ok(
        json!({"sha": tree["sha"], "truncated": tree["truncated"], "count": entries.len(), "tree": entries, "tree_sha": sha, "owner": owner, "repo": repo, "recursive": recursive}),
    )
}

// ---------- gists ----------

async fn list_gists(ctx: Ctx, args: Value) -> Result<Value> {
    let mut q = page_q(&args);
    if let Some(s) = rfc3339(&args, "since")? {
        q.push(("since", s));
    }
    let path = match opt(&args, "username") {
        Some(u) => format!("/users/{u}/gists"),
        None => "/gists".into(),
    };
    ctx.gh(&args)?.rest("GET", &path, &q, None).await
}

async fn get_gist(ctx: Ctx, args: Value) -> Result<Value> {
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/gists/{}", req_str(&args, "gist_id")?),
            &[],
            None,
        )
        .await
}

fn gist_min(r: Value) -> Value {
    json!({"id": r["id"], "url": r["html_url"]})
}

async fn create_gist(ctx: Ctx, args: Value) -> Result<Value> {
    let (filename, content) = (req_str(&args, "filename")?, req_str(&args, "content")?);
    let body = json!({"description": arg_str(&args, "description").unwrap_or(""), "public": arg_bool(&args, "public").unwrap_or(false), "files": {filename: {"content": content}}});
    ctx.gh(&args)?
        .rest("POST", "/gists", &[], Some(body))
        .await
        .map(gist_min)
}

async fn update_gist(ctx: Ctx, args: Value) -> Result<Value> {
    let (id, filename, content) = (
        req_str(&args, "gist_id")?,
        req_str(&args, "filename")?,
        req_str(&args, "content")?,
    );
    let mut body = json!({"files": {filename: {"filename": filename, "content": content}}});
    if let Some(d) = arg_str(&args, "description") {
        body["description"] = json!(d);
    }
    ctx.gh(&args)?
        .rest("PATCH", &format!("/gists/{id}"), &[], Some(body))
        .await
        .map(gist_min)
}

// ---------- contexte ----------

async fn get_me(ctx: Ctx, args: Value) -> Result<Value> {
    let u = ctx.gh(&args)?.rest("GET", "/user", &[], None).await?;
    let mut out = pick(&u, &["login", "id"]);
    out["profile_url"] = u["html_url"].clone();
    out["avatar_url"] = u["avatar_url"].clone();
    out["details"] = pick(
        &u,
        &[
            "name",
            "company",
            "blog",
            "location",
            "email",
            "hireable",
            "bio",
            "twitter_username",
            "public_repos",
            "public_gists",
            "followers",
            "following",
            "created_at",
            "updated_at",
            "private_gists",
            "total_private_repos",
            "owned_private_repos",
        ],
    );
    Ok(out)
}

async fn get_teams(ctx: Ctx, args: Value) -> Result<Value> {
    let gh = ctx.gh(&args)?;
    let login = match opt(&args, "user") {
        Some(u) => u.to_string(),
        None => gh.rest("GET", "/user", &[], None).await?["login"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    };
    let q = "query($login: String!) { user(login: $login) { organizations(first: 100) { nodes { login teams(first: 100, userLogins: [$login]) { nodes { name slug description } } } } } }";
    let d = gh.graphql(q, json!({"login": login})).await?;
    let orgs: Vec<Value> = d["user"]["organizations"]["nodes"]
        .as_array()
        .map(|a| a.iter().map(|o| json!({"org": o["login"], "teams": o["teams"]["nodes"].as_array().cloned().unwrap_or_default()})).collect())
        .unwrap_or_default();
    Ok(Value::Array(orgs))
}

async fn get_team_members(ctx: Ctx, args: Value) -> Result<Value> {
    let (org, slug) = (req_str(&args, "org")?, req_str(&args, "team_slug")?);
    let q = "query($org: String!, $teamSlug: String!) { organization(login: $org) { team(slug: $teamSlug) { members(first: 100) { nodes { login } } } } }";
    let d = ctx
        .gh(&args)?
        .graphql(q, json!({"org": org, "teamSlug": slug}))
        .await?;
    let m: Vec<Value> = d["organization"]["team"]["members"]["nodes"]
        .as_array()
        .map(|a| a.iter().map(|n| n["login"].clone()).collect())
        .unwrap_or_default();
    Ok(Value::Array(m))
}

// ---------- notifications ----------

/// owner+repo présents -> notifications du dépôt, sinon celles du compte.
fn notif_path(args: &Value) -> String {
    match (opt(args, "owner"), opt(args, "repo")) {
        (Some(o), Some(r)) => format!("/repos/{o}/{r}/notifications"),
        _ => "/notifications".into(),
    }
}

async fn list_notifications(ctx: Ctx, args: Value) -> Result<Value> {
    let mut q = page_q(&args);
    match arg_str(&args, "filter") {
        Some("include_read_notifications") => q.push(("all", "true".into())),
        Some("only_participating") => q.push(("participating", "true".into())),
        _ => {}
    }
    for k in ["since", "before"] {
        if let Some(s) = rfc3339(&args, k)? {
            q.push((k, s));
        }
    }
    ctx.gh(&args)?
        .rest("GET", &notif_path(&args), &q, None)
        .await
}

async fn dismiss_notification(ctx: Ctx, args: Value) -> Result<Value> {
    let (id, state) = (req_str(&args, "threadID")?, req_str(&args, "state")?);
    let method = match state {
        "done" => "DELETE",
        "read" => "PATCH",
        _ => {
            return Err(GhError::InvalidArgs(
                "Invalid state. Must be one of: read, done.".into(),
            ))
        }
    };
    ctx.gh(&args)?
        .rest(method, &format!("/notifications/threads/{id}"), &[], None)
        .await?;
    Ok(json!({"message": format!("Notification marked as {state}")}))
}

async fn mark_all_notifications_read(ctx: Ctx, args: Value) -> Result<Value> {
    let last = rfc3339(&args, "lastReadAt")?.unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    ctx.gh(&args)?
        .rest(
            "PUT",
            &notif_path(&args),
            &[],
            Some(json!({"last_read_at": last})),
        )
        .await?;
    Ok(json!({"message": "All notifications marked as read"}))
}

async fn get_notification_details(ctx: Ctx, args: Value) -> Result<Value> {
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!(
                "/notifications/threads/{}",
                req_str(&args, "notificationID")?
            ),
            &[],
            None,
        )
        .await
}

/// ignore / watch / delete sur une ressource d'abonnement (fil ou dépôt).
async fn subscription(ctx: &Ctx, args: &Value, path: &str, what: &str) -> Result<Value> {
    let gh = ctx.gh(args)?;
    match req_str(args, "action")? {
        "ignore" => {
            gh.rest("PUT", path, &[], Some(json!({"ignored": true})))
                .await
        }
        "watch" => {
            gh.rest(
                "PUT",
                path,
                &[],
                Some(json!({"ignored": false, "subscribed": true})),
            )
            .await
        }
        "delete" => {
            gh.rest("DELETE", path, &[], None).await?;
            Ok(json!({"message": format!("{what} subscription deleted")}))
        }
        _ => Err(GhError::InvalidArgs(
            "Invalid action. Must be one of: ignore, watch, delete.".into(),
        )),
    }
}

async fn manage_notification_subscription(ctx: Ctx, args: Value) -> Result<Value> {
    let p = format!(
        "/notifications/threads/{}/subscription",
        req_str(&args, "notificationID")?
    );
    subscription(&ctx, &args, &p, "Notification").await
}

async fn manage_repository_notification_subscription(ctx: Ctx, args: Value) -> Result<Value> {
    let p = format!(
        "/repos/{}/{}/subscription",
        req_str(&args, "owner")?,
        req_str(&args, "repo")?
    );
    subscription(&ctx, &args, &p, "Repository").await
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    // Droits repris des déclarations Go : PublicRead(Repo) -> repo ; ReadOrg -> read:org ; Gist -> gist ; Notifications -> notifications.
    let repo = |t: Tool| t.scopes(&["repo"]).perm("metadata", READ);
    let org = |t: Tool| t.scopes(&["read:org"]).perm("members", READ);
    let notif = |t: Tool, w: bool| {
        t.scopes(&["notifications"])
            .level(if w { Level::Dev } else { Level::Lecture })
    };
    vec![
        repo(Tool::official("search_repositories", search_repositories)),
        repo(Tool::official("search_code", search_code)),
        repo(Tool::official("search_commits", search_commits)),
        repo(Tool::official("search_users", search_users)),
        org(Tool::official("search_orgs", search_orgs)),
        repo(Tool::official("get_repository_tree", get_repository_tree)).perm("contents", READ),
        Tool::official("list_gists", list_gists).scopes(&[]),
        Tool::official("get_gist", get_gist).scopes(&[]),
        Tool::official("create_gist", create_gist)
            .scopes(&["gist"])
            .level(Level::Dev),
        Tool::official("update_gist", update_gist)
            .scopes(&["gist"])
            .level(Level::Dev),
        Tool::official("get_me", get_me)
            .scopes(&[])
            .perm("metadata", READ),
        org(Tool::official("get_teams", get_teams)),
        org(Tool::official("get_team_members", get_team_members)),
        notif(
            Tool::official("list_notifications", list_notifications),
            false,
        ),
        notif(
            Tool::official("get_notification_details", get_notification_details),
            false,
        ),
        notif(
            Tool::official("dismiss_notification", dismiss_notification),
            true,
        ),
        notif(
            Tool::official("mark_all_notifications_read", mark_all_notifications_read),
            true,
        ),
        notif(
            Tool::official(
                "manage_notification_subscription",
                manage_notification_subscription,
            ),
            true,
        ),
        notif(
            Tool::official(
                "manage_repository_notification_subscription",
                manage_repository_notification_subscription,
            ),
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};
    use std::sync::Arc;

    const NAMES: [&str; 19] = [
        "search_code",
        "search_commits",
        "search_orgs",
        "search_repositories",
        "search_users",
        "get_repository_tree",
        "create_gist",
        "get_gist",
        "list_gists",
        "update_gist",
        "get_me",
        "get_team_members",
        "get_teams",
        "dismiss_notification",
        "get_notification_details",
        "list_notifications",
        "manage_notification_subscription",
        "manage_repository_notification_subscription",
        "mark_all_notifications_read",
    ];

    fn ctx() -> Ctx {
        let cfg = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_SGG_TEST_TOKEN\"\n";
        Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus())
    }

    fn args(name: &str) -> Value {
        match name {
            "search_code"
            | "search_commits"
            | "search_orgs"
            | "search_repositories"
            | "search_users" => json!({"query": "demo"}),
            "get_repository_tree" => {
                json!({"owner": "octo-test", "repo": "demo", "tree_sha": "main", "path_filter": "src/"})
            }
            "create_gist" => json!({"filename": "a.txt", "content": "x"}),
            "update_gist" => {
                json!({"gist_id": "g1", "filename": "a.txt", "content": "y", "description": "d"})
            }
            "get_gist" => json!({"gist_id": "g1"}),
            "get_team_members" => json!({"org": "org-test", "team_slug": "core"}),
            "dismiss_notification" => json!({"threadID": "1", "state": "done"}),
            "get_notification_details" => json!({"notificationID": "1"}),
            "manage_notification_subscription" => json!({"notificationID": "1", "action": "watch"}),
            "manage_repository_notification_subscription" => {
                json!({"owner": "octo-test", "repo": "demo", "action": "ignore"})
            }
            _ => json!({}),
        }
    }

    #[test]
    fn dix_neuf_noms_presents() {
        let mut got: Vec<String> = tools().into_iter().map(|t| t.name).collect();
        got.sort();
        let mut want: Vec<String> = NAMES.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn schema_publie_egal_au_snap() {
        for t in tools() {
            let snap = ghmcp_core::schema::snap(&t.name).unwrap();
            let keys = |v: &Value| -> std::collections::BTreeSet<String> {
                v.pointer("/inputSchema/properties")
                    .and_then(Value::as_object)
                    .map(|o| o.keys().filter(|k| *k != "account").cloned().collect())
                    .unwrap_or_default()
            };
            let req = |v: &Value| {
                v.pointer("/inputSchema/required")
                    .cloned()
                    .unwrap_or(json!([]))
            };
            let published = json!({"inputSchema": t.published_schema()});
            assert_eq!(keys(&published), keys(&snap), "{}", t.name);
            assert_eq!(req(&published), req(&snap), "{}", t.name);
        }
    }

    #[tokio::test]
    async fn chaque_outil_repond_en_mode_test() {
        for t in tools() {
            let r = (t.handler)(ctx(), args(&t.name)).await;
            assert!(r.is_ok(), "{} a échoué: {:?}", t.name, r.err());
        }
    }

    #[tokio::test]
    async fn validations_comme_le_go() {
        let t = tools();
        let call = |n: &str, a: Value| {
            let h = t.iter().find(|x| x.name == n).unwrap().handler.clone();
            async move { h(ctx(), a).await }
        };
        assert!(call("search_users", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("query"));
        assert!(call(
            "dismiss_notification",
            json!({"threadID": "1", "state": "nope"})
        )
        .await
        .is_err());
        assert!(call("list_notifications", json!({"since": "pas-une-date"}))
            .await
            .is_err());
        assert!(
            has_type_filter("type:org x")
                && has_type_filter("(type:user)")
                && !has_type_filter("prototype:x")
                && !has_type_filter("type: x")
        );
    }
}
