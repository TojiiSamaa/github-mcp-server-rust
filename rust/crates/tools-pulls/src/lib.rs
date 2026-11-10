//! ghmcp-tools-pulls : outils officiels pull requests et reviews (pullrequests.go, pullrequests_granular.go,
//! comment_minimize.go de github/github-mcp-server, MIT). Contrat : en-tête AGENTS-CONTRAT de ghmcp-core.
//!
//! Écarts connus : `list_pull_requests`/`search_pull_requests` ne filtrent pas `fields` (réponse REST complète).

use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, GhError, Result, Tool};
use serde_json::{json, Map, Value};

type Out = Result<Value>;

fn int(a: &Value, k: &str) -> Result<i64> {
    arg_i64(a, k).ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))
}
fn repo_of(a: &Value) -> Result<(String, String)> {
    Ok((req_str(a, "owner")?.into(), req_str(a, "repo")?.into()))
}
fn pull_path(a: &Value) -> Result<String> {
    let (o, r) = repo_of(a)?;
    Ok(format!("/repos/{o}/{r}/pulls/{}", int(a, "pullNumber")?))
}
fn is_echo(v: &Value) -> bool {
    v.get("test_mode").and_then(Value::as_bool).unwrap_or(false)
}
fn text(m: &str) -> Out {
    Ok(json!(m))
}
fn page_q(a: &Value) -> Vec<(&'static str, String)> {
    let mut q = vec![];
    if let Some(p) = arg_i64(a, "page") {
        q.push(("page", p.to_string()));
    }
    if let Some(p) = arg_i64(a, "perPage") {
        q.push(("per_page", p.to_string()));
    }
    q
}
fn str_list(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
/// "org/team" = équipe (slug), sinon utilisateur (splitPullRequestReviewers du Go).
fn split_reviewers(r: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut users, mut teams) = (vec![], vec![]);
    for x in r {
        match x.split_once('/') {
            Some((o, t)) if !o.is_empty() && !t.is_empty() && !t.contains('/') => {
                teams.push(t.to_string())
            }
            _ => users.push(x.clone()),
        }
    }
    (users, teams)
}
fn minimal(pr: &Value) -> Value {
    json!({"id": pr.get("id").map(|i| i.to_string()).unwrap_or_default(),
           "url": pr.get("html_url").cloned().unwrap_or(Value::Null)})
}

async fn request_reviewers(ctx: &Ctx, a: &Value, reviewers: &[String]) -> Out {
    let (users, teams) = split_reviewers(reviewers);
    ctx.gh(a)?
        .rest(
            "POST",
            &format!("{}/requested_reviewers", pull_path(a)?),
            &[],
            Some(json!({"reviewers": users, "team_reviewers": teams})),
        )
        .await
}

// ---------- GraphQL partagé ----------

async fn pr_node_id(ctx: &Ctx, a: &Value) -> Result<Value> {
    let (o, r) = repo_of(a)?;
    let q = "query($owner:String!,$repo:String!,$prNum:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$prNum){id isDraft}}}";
    ctx.gh(a)?
        .graphql(
            q,
            json!({"owner": o, "repo": r, "prNum": int(a, "pullNumber")?}),
        )
        .await
}

/// Id de la review PENDING du compte courant (viewer), avec pagination. Mode test : Err(écho).
async fn pending_review(ctx: &Ctx, a: &Value) -> Result<std::result::Result<String, Value>> {
    let (o, r) = repo_of(a)?;
    let gh = ctx.gh(a)?;
    let viewer = gh.graphql("query{viewer{login}}", json!({})).await?;
    if is_echo(&viewer) {
        return Ok(Err(viewer));
    }
    let me = viewer["viewer"]["login"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let q = "query($after:String,$owner:String!,$name:String!,$prNum:Int!){repository(owner:$owner,name:$name){pullRequest(number:$prNum){reviews(first:100,after:$after,states:[PENDING]){nodes{id author{login}} pageInfo{hasNextPage endCursor}}}}}";
    let mut after = Value::Null;
    loop {
        let d = gh
            .graphql(
                q,
                json!({"after": after, "owner": o, "name": r, "prNum": int(a, "pullNumber")?}),
            )
            .await?;
        let reviews = &d["repository"]["pullRequest"]["reviews"];
        if let Some(n) = reviews["nodes"].as_array().and_then(|n| {
            n.iter()
                .find(|n| n["author"]["login"].as_str() == Some(&me))
        }) {
            return Ok(Ok(n["id"].as_str().unwrap_or_default().to_string()));
        }
        if !reviews["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false)
        {
            return Err(GhError::Other(
                "No pending review found for the viewer".into(),
            ));
        }
        after = reviews["pageInfo"]["endCursor"].clone();
    }
}

async fn review_create(ctx: Ctx, a: Value) -> Out {
    let d = pr_node_id(&ctx, &a).await?;
    if is_echo(&d) {
        return Ok(d);
    }
    let event = arg_str(&a, "event").filter(|e| !e.is_empty());
    let mut input = json!({"pullRequestId": d["repository"]["pullRequest"]["id"]});
    if let Some(c) = arg_str(&a, "commitID").filter(|c| !c.is_empty()) {
        input["commitOID"] = json!(c);
    }
    if let Some(e) = event {
        input["event"] = json!(e);
        input["body"] = json!(arg_str(&a, "body").unwrap_or(""));
    }
    ctx.gh(&a)?.graphql("mutation($input:AddPullRequestReviewInput!){addPullRequestReview(input:$input){pullRequestReview{id}}}", json!({"input": input})).await?;
    text(if event.is_none() {
        "pending pull request created"
    } else {
        "pull request review submitted successfully"
    })
}

async fn review_submit(ctx: Ctx, a: Value) -> Out {
    let id = match pending_review(&ctx, &a).await? {
        Ok(id) => id,
        Err(echo) => return Ok(echo),
    };
    let input = json!({"pullRequestReviewId": id, "event": req_str(&a, "event")?, "body": arg_str(&a, "body").unwrap_or("")});
    ctx.gh(&a)?.graphql("mutation($input:SubmitPullRequestReviewInput!){submitPullRequestReview(input:$input){pullRequestReview{id}}}", json!({"input": input})).await?;
    text("pending pull request review successfully submitted")
}

async fn review_delete(ctx: Ctx, a: Value) -> Out {
    let id = match pending_review(&ctx, &a).await? {
        Ok(id) => id,
        Err(echo) => return Ok(echo),
    };
    ctx.gh(&a)?.graphql("mutation($input:DeletePullRequestReviewInput!){deletePullRequestReview(input:$input){pullRequestReview{id}}}", json!({"input": {"pullRequestReviewId": id}})).await?;
    text("pending pull request review successfully deleted")
}

async fn thread_resolve(ctx: &Ctx, a: &Value, id: &str, resolve: bool) -> Out {
    if id.is_empty() {
        return Err(GhError::InvalidArgs(
            "threadId is required for resolve_thread and unresolve_thread methods".into(),
        ));
    }
    let (m, f) = if resolve {
        ("resolveReviewThread", "ResolveReviewThreadInput")
    } else {
        ("unresolveReviewThread", "UnresolveReviewThreadInput")
    };
    let d = ctx
        .gh(a)?
        .graphql(
            &format!("mutation($input:{f}!){{{m}(input:$input){{thread{{id isResolved}}}}}}"),
            json!({"input": {"threadId": id}}),
        )
        .await?;
    if is_echo(&d) {
        return Ok(d);
    }
    text(if resolve {
        "review thread resolved successfully"
    } else {
        "review thread unresolved successfully"
    })
}

async fn comment_pending(ctx: Ctx, a: Value) -> Out {
    let id = match pending_review(&ctx, &a).await? {
        Ok(id) => id,
        Err(echo) => return Ok(echo),
    };
    let mut input = json!({"pullRequestReviewId": id, "path": req_str(&a, "path")?, "body": req_str(&a, "body")?, "subjectType": req_str(&a, "subjectType")?});
    for k in ["line", "startLine"] {
        if let Some(n) = arg_i64(&a, k) {
            input[k] = json!(n);
        }
    }
    for k in ["side", "startSide"] {
        if let Some(s) = arg_str(&a, k) {
            input[k] = json!(s);
        }
    }
    let d = ctx.gh(&a)?.graphql("mutation($input:AddPullRequestReviewThreadInput!){addPullRequestReviewThread(input:$input){thread{id}}}", json!({"input": input})).await?;
    if d["addPullRequestReviewThread"]["thread"]["id"].is_null() {
        return Err(GhError::Other("Failed to add comment to pending review. Possible reasons: the line number doesn't exist in the pull request diff, the file path is incorrect, or the side (LEFT/RIGHT) is invalid for the specified line".into()));
    }
    text("pull request review comment successfully added to pending review")
}

async fn draft_state(ctx: &Ctx, a: &Value, draft: bool) -> Out {
    let d = pr_node_id(ctx, a).await?;
    if is_echo(&d) {
        return Ok(d);
    }
    let pr = &d["repository"]["pullRequest"];
    if pr["isDraft"].as_bool() == Some(draft) {
        return Ok(json!({"draft": draft, "changed": false}));
    }
    let (m, f) = if draft {
        (
            "convertPullRequestToDraft",
            "ConvertPullRequestToDraftInput",
        )
    } else {
        (
            "markPullRequestReadyForReview",
            "MarkPullRequestReadyForReviewInput",
        )
    };
    ctx.gh(a)?
        .graphql(
            &format!("mutation($input:{f}!){{{m}(input:$input){{pullRequest{{id isDraft}}}}}}"),
            json!({"input": {"pullRequestId": pr["id"]}}),
        )
        .await?;
    Ok(json!({"draft": draft, "changed": true}))
}

// ---------- pull_request_read ----------

async fn pull_request_read(ctx: Ctx, a: Value) -> Out {
    let gh = ctx.gh(&a)?;
    let (o, r) = repo_of(&a)?;
    let p = pull_path(&a)?;
    let q = page_q(&a);
    let method = req_str(&a, "method")?;
    match method {
        "get" => gh.rest("GET", &p, &[], None).await,
        "get_files" => gh.rest("GET", &format!("{p}/files"), &q, None).await,
        "get_commits" => gh.rest("GET", &format!("{p}/commits"), &q, None).await,
        "get_reviews" => gh.rest("GET", &format!("{p}/reviews"), &q, None).await,
        "get_comments" => {
            gh.rest(
                "GET",
                &format!("/repos/{o}/{r}/issues/{}/comments", int(&a, "pullNumber")?),
                &q,
                None,
            )
            .await
        }
        "get_diff" => {
            let opts = ghmcp_core::net::ReqOpts {
                accept: Some("application/vnd.github.diff".into()),
                ..Default::default()
            };
            Ok(json!(
                gh.rest_raw_with("GET", &p, &[], None, &opts).await?.text
            ))
        }
        "get_status" | "get_check_runs" => {
            let pr = gh.rest("GET", &p, &[], None).await?;
            let sha = pr["head"]["sha"]
                .as_str()
                .map(String::from)
                .or_else(|| ctx.test_mode().then(|| "HEAD".to_string()));
            let Some(sha) = sha else {
                return Err(GhError::Other("pull request sans head.sha".into()));
            };
            if method == "get_status" {
                gh.rest(
                    "GET",
                    &format!("/repos/{o}/{r}/commits/{sha}/status"),
                    &[],
                    None,
                )
                .await
            } else {
                gh.rest(
                    "GET",
                    &format!("/repos/{o}/{r}/commits/{sha}/check-runs"),
                    &q,
                    None,
                )
                .await
            }
        }
        "get_review_comments" => {
            let query = "query($owner:String!,$repo:String!,$prNum:Int!,$after:String){repository(owner:$owner,name:$repo){pullRequest(number:$prNum){reviewThreads(first:100,after:$after){nodes{id isResolved isOutdated isCollapsed comments(first:100){totalCount nodes{id body path line author{login} createdAt url}}} pageInfo{hasNextPage endCursor}}}}}";
            let vars = json!({"owner": o, "repo": r, "prNum": int(&a, "pullNumber")?, "after": arg_str(&a, "after")});
            gh.graphql(query, vars).await
        }
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}

// ---------- écritures REST ----------

async fn create_pull_request(ctx: Ctx, a: Value) -> Out {
    let (o, r) = repo_of(&a)?;
    let mut b = Map::new();
    for k in ["title", "head", "base"] {
        b.insert(k.into(), json!(req_str(&a, k)?));
    }
    if let Some(v) = arg_str(&a, "body") {
        b.insert("body".into(), json!(v));
    }
    for k in ["draft", "maintainer_can_modify"] {
        if let Some(v) = arg_bool(&a, k) {
            b.insert(k.into(), json!(v));
        }
    }
    let gh = ctx.gh(&a)?;
    let pr = gh
        .rest(
            "POST",
            &format!("/repos/{o}/{r}/pulls"),
            &[],
            Some(Value::Object(b)),
        )
        .await?;
    let reviewers = str_list(&a, "reviewers");
    if !reviewers.is_empty() {
        let (users, teams) = split_reviewers(&reviewers);
        let n = pr["number"].as_i64().unwrap_or(0);
        gh.rest(
            "POST",
            &format!("/repos/{o}/{r}/pulls/{n}/requested_reviewers"),
            &[],
            Some(json!({"reviewers": users, "team_reviewers": teams})),
        )
        .await?;
    }
    Ok(minimal(&pr))
}

async fn list_pull_requests(ctx: Ctx, a: Value) -> Out {
    let (o, r) = repo_of(&a)?;
    let mut q = page_q(&a);
    for k in ["state", "head", "base", "sort", "direction"] {
        if let Some(v) = arg_str(&a, k) {
            q.push((k, v.into()));
        }
    }
    ctx.gh(&a)?
        .rest("GET", &format!("/repos/{o}/{r}/pulls"), &q, None)
        .await
}

async fn merge_pull_request(ctx: Ctx, a: Value) -> Out {
    let mut b = Map::new();
    for (k, g) in [
        ("commit_title", "commit_title"),
        ("commit_message", "commit_message"),
        ("merge_method", "merge_method"),
        ("expectedHeadSha", "sha"),
    ] {
        if let Some(v) = arg_str(&a, k).filter(|v| !v.is_empty()) {
            b.insert(g.into(), json!(v));
        }
    }
    ctx.gh(&a)?
        .rest(
            "PUT",
            &format!("{}/merge", pull_path(&a)?),
            &[],
            Some(Value::Object(b)),
        )
        .await
}

async fn update_pull_request_branch(ctx: Ctx, a: Value) -> Out {
    let body = arg_str(&a, "expectedHeadSha")
        .filter(|s| !s.is_empty())
        .map(|s| json!({"expected_head_sha": s}));
    ctx.gh(&a)?
        .rest(
            "PUT",
            &format!("{}/update-branch", pull_path(&a)?),
            &[],
            body,
        )
        .await
}

async fn search_pull_requests(ctx: Ctx, a: Value) -> Out {
    let mut query = format!("is:pr {}", req_str(&a, "query")?);
    match (arg_str(&a, "owner"), arg_str(&a, "repo")) {
        (Some(o), Some(r)) => query.push_str(&format!(" repo:{o}/{r}")),
        (Some(o), None) => query.push_str(&format!(" user:{o}")),
        _ => {}
    }
    let mut q = page_q(&a);
    q.push(("q", query));
    for k in ["sort", "order"] {
        if let Some(v) = arg_str(&a, k) {
            q.push((k, v.into()));
        }
    }
    ctx.gh(&a)?.rest("GET", "/search/issues", &q, None).await
}

async fn update_pull_request(ctx: Ctx, a: Value) -> Out {
    let p = pull_path(&a)?;
    let gh = ctx.gh(&a)?;
    let mut b = Map::new();
    for k in ["title", "body", "state", "base"] {
        if let Some(v) = arg_str(&a, k) {
            b.insert(k.into(), json!(v));
        }
    }
    if let Some(v) = arg_bool(&a, "maintainer_can_modify") {
        b.insert("maintainer_can_modify".into(), json!(v));
    }
    let draft = arg_bool(&a, "draft");
    let reviewers = str_list(&a, "reviewers");
    if b.is_empty() && draft.is_none() && reviewers.is_empty() {
        return Err(GhError::InvalidArgs(
            "No update parameters provided.".into(),
        ));
    }
    if !b.is_empty() {
        gh.rest("PATCH", &p, &[], Some(Value::Object(b))).await?;
    }
    if let Some(d) = draft {
        draft_state(&ctx, &a, d).await?;
    }
    if !reviewers.is_empty() {
        request_reviewers(&ctx, &a, &reviewers).await?;
    }
    Ok(minimal(&gh.rest("GET", &p, &[], None).await?))
}

async fn patch_fields(ctx: Ctx, a: Value, keys: &[&str]) -> Out {
    let mut b = Map::new();
    for k in keys {
        b.insert((*k).into(), json!(req_str(&a, k)?));
    }
    Ok(minimal(
        &ctx.gh(&a)?
            .rest("PATCH", &pull_path(&a)?, &[], Some(Value::Object(b)))
            .await?,
    ))
}
async fn update_title(ctx: Ctx, a: Value) -> Out {
    patch_fields(ctx, a, &["title"]).await
}
async fn update_body(ctx: Ctx, a: Value) -> Out {
    patch_fields(ctx, a, &["body"]).await
}
async fn update_state(ctx: Ctx, a: Value) -> Out {
    patch_fields(ctx, a, &["state"]).await
}
async fn update_draft(ctx: Ctx, a: Value) -> Out {
    let d = arg_bool(&a, "draft")
        .ok_or_else(|| GhError::InvalidArgs("paramètre requis manquant: draft".into()))?;
    draft_state(&ctx, &a, d).await
}
async fn request_reviewers_tool(ctx: Ctx, a: Value) -> Out {
    let rv = str_list(&a, "reviewers");
    if rv.is_empty() {
        return Err(GhError::InvalidArgs("reviewers must not be empty".into()));
    }
    Ok(minimal(&request_reviewers(&ctx, &a, &rv).await?))
}

async fn reply_to_comment(ctx: Ctx, a: Value) -> Out {
    let (o, r) = repo_of(&a)?;
    let cid = int(&a, "commentId")?;
    if cid < 1 {
        return Err(GhError::InvalidArgs(
            "commentId must be greater than 0".into(),
        ));
    }
    let (body, reaction) = (arg_str(&a, "body"), arg_str(&a, "reaction"));
    if body.is_none() && reaction.is_none() {
        return Err(GhError::InvalidArgs(
            "at least one of body or reaction is required".into(),
        ));
    }
    if body == Some("") || reaction == Some("") {
        return Err(GhError::InvalidArgs(
            "body and reaction cannot be empty when provided".into(),
        ));
    }
    let gh = ctx.gh(&a)?;
    let mut out = Map::new();
    if let Some(c) = reaction {
        let v = gh
            .rest(
                "POST",
                &format!("/repos/{o}/{r}/pulls/comments/{cid}/reactions"),
                &[],
                Some(json!({"content": c})),
            )
            .await?;
        out.insert("reaction".into(), json!({"id": v["id"].to_string()}));
    }
    if let Some(b) = body {
        let n = int(&a, "pullNumber")?;
        let v = gh
            .rest(
                "POST",
                &format!("/repos/{o}/{r}/pulls/{n}/comments"),
                &[],
                Some(json!({"body": b, "in_reply_to": cid})),
            )
            .await?;
        out.insert("comment".into(), minimal(&v));
    }
    Ok(Value::Object(out))
}

async fn review_write(ctx: Ctx, a: Value) -> Out {
    let thread = arg_str(&a, "threadId").unwrap_or("").to_string();
    match req_str(&a, "method")? {
        "create" => review_create(ctx, a).await,
        "submit_pending" => review_submit(ctx, a).await,
        "delete_pending" => review_delete(ctx, a).await,
        "resolve_thread" => thread_resolve(&ctx, &a, &thread, true).await,
        "unresolve_thread" => thread_resolve(&ctx, &a, &thread, false).await,
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}
async fn resolve_thread(ctx: Ctx, a: Value) -> Out {
    thread_resolve(&ctx, &a, req_str(&a, "threadID")?, true).await
}
async fn unresolve_thread(ctx: Ctx, a: Value) -> Out {
    thread_resolve(&ctx, &a, req_str(&a, "threadID")?, false).await
}

async fn add_reaction(ctx: Ctx, a: Value) -> Out {
    let (o, r) = repo_of(&a)?;
    let v = ctx
        .gh(&a)?
        .rest(
            "POST",
            &format!(
                "/repos/{o}/{r}/pulls/comments/{}/reactions",
                int(&a, "comment_id")?
            ),
            &[],
            Some(json!({"content": req_str(&a, "content")?})),
        )
        .await?;
    Ok(json!({"id": v["id"].to_string()}))
}
async fn remove_reaction(ctx: Ctx, a: Value) -> Out {
    let (o, r) = repo_of(&a)?;
    ctx.gh(&a)?
        .rest(
            "DELETE",
            &format!(
                "/repos/{o}/{r}/pulls/comments/{}/reactions/{}",
                int(&a, "comment_id")?,
                int(&a, "reaction_id")?
            ),
            &[],
            None,
        )
        .await?;
    text("reaction removed")
}

// ---------- masquage (comment_minimize.go) ----------

const CLASSIFIERS: [&str; 7] = [
    "SPAM",
    "ABUSE",
    "OFF_TOPIC",
    "OUTDATED",
    "DUPLICATE",
    "RESOLVED",
    "LOW_QUALITY",
];

async fn visibility(ctx: Ctx, a: Value, node_path: String, hide: bool) -> Out {
    let gh = ctx.gh(&a)?;
    let classifier = if hide {
        let c = req_str(&a, "classifier")?.to_uppercase();
        if !CLASSIFIERS.contains(&c.as_str()) {
            return Err(GhError::InvalidArgs(format!(
                "invalid classifier {c:?}: must be one of {CLASSIFIERS:?}"
            )));
        }
        Some(c)
    } else {
        None
    };
    let obj = gh.rest("GET", &node_path, &[], None).await?;
    let node = match obj["node_id"].as_str() {
        Some(n) => n.to_string(),
        None if ctx.test_mode() => "TEST_NODE_ID".into(),
        None => return Err(GhError::Other("response has no node ID".into())),
    };
    if hide {
        let d = gh.graphql("mutation($input:MinimizeCommentInput!){minimizeComment(input:$input){minimizedComment{isMinimized minimizedReason}}}", json!({"input": {"subjectId": node, "classifier": classifier}})).await?;
        let m = &d["minimizeComment"]["minimizedComment"];
        Ok(
            json!({"node_id": node, "is_minimized": m["isMinimized"].as_bool().unwrap_or(false), "minimized_reason": m["minimizedReason"]}),
        )
    } else {
        let d = gh.graphql("mutation($input:UnminimizeCommentInput!){unminimizeComment(input:$input){unminimizedComment{isMinimized}}}", json!({"input": {"subjectId": node}})).await?;
        Ok(
            json!({"node_id": node, "is_minimized": d["unminimizeComment"]["unminimizedComment"]["isMinimized"].as_bool().unwrap_or(false)}),
        )
    }
}
fn issue_comment_path(a: &Value) -> Result<String> {
    let (o, r) = repo_of(a)?;
    Ok(format!(
        "/repos/{o}/{r}/issues/comments/{}",
        int(a, "comment_id")?
    ))
}
fn review_comment_path(a: &Value) -> Result<String> {
    let (o, r) = repo_of(a)?;
    Ok(format!(
        "/repos/{o}/{r}/pulls/comments/{}",
        int(a, "comment_id")?
    ))
}
fn review_path(a: &Value) -> Result<String> {
    Ok(format!(
        "{}/reviews/{}",
        pull_path(a)?,
        int(a, "review_id")?
    ))
}
async fn hide_issue_comment(ctx: Ctx, a: Value) -> Out {
    let p = issue_comment_path(&a)?;
    visibility(ctx, a, p, true).await
}
async fn unhide_issue_comment(ctx: Ctx, a: Value) -> Out {
    let p = issue_comment_path(&a)?;
    visibility(ctx, a, p, false).await
}
async fn hide_review_comment(ctx: Ctx, a: Value) -> Out {
    let p = review_comment_path(&a)?;
    visibility(ctx, a, p, true).await
}
async fn unhide_review_comment(ctx: Ctx, a: Value) -> Out {
    let p = review_comment_path(&a)?;
    visibility(ctx, a, p, false).await
}
async fn hide_review(ctx: Ctx, a: Value) -> Out {
    let p = review_path(&a)?;
    visibility(ctx, a, p, true).await
}
async fn unhide_review(ctx: Ctx, a: Value) -> Out {
    let p = review_path(&a)?;
    visibility(ctx, a, p, false).await
}

fn w(t: Tool) -> Tool {
    t.scopes(&["repo"]).perm("pull_requests", Access::Write)
}
fn rd(t: Tool) -> Tool {
    t.scopes(&["repo"]).perm("pull_requests", Access::Read)
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    vec![
        rd(Tool::official("pull_request_read", pull_request_read))
            .perm("checks", Access::Read)
            .perm("statuses", Access::Read),
        rd(Tool::official("list_pull_requests", list_pull_requests)),
        rd(Tool::official("search_pull_requests", search_pull_requests)),
        w(Tool::official("create_pull_request", create_pull_request)),
        w(Tool::official("update_pull_request", update_pull_request)),
        w(Tool::official("merge_pull_request", merge_pull_request)).perm("contents", Access::Write),
        w(Tool::official(
            "update_pull_request_branch",
            update_pull_request_branch,
        ))
        .perm("contents", Access::Write),
        w(Tool::official(
            "add_reply_to_pull_request_comment",
            reply_to_comment,
        )),
        w(Tool::official(
            "add_comment_to_pending_review",
            comment_pending,
        )),
        w(Tool::official("pull_request_review_write", review_write)),
        w(Tool::official(
            "add_pull_request_review_comment",
            comment_pending,
        )),
        w(Tool::official(
            "add_pull_request_review_comment_reaction",
            add_reaction,
        )),
        w(Tool::official(
            "remove_pull_request_review_comment_reaction",
            remove_reaction,
        )),
        w(Tool::official("create_pull_request_review", review_create)),
        w(Tool::official(
            "delete_pending_pull_request_review",
            review_delete,
        )),
        w(Tool::official(
            "submit_pending_pull_request_review",
            review_submit,
        )),
        w(Tool::official(
            "request_pull_request_reviewers",
            request_reviewers_tool,
        )),
        w(Tool::official("resolve_review_thread", resolve_thread)),
        w(Tool::official("unresolve_review_thread", unresolve_thread)),
        w(Tool::official("update_pull_request_title", update_title)),
        w(Tool::official("update_pull_request_body", update_body)),
        w(Tool::official("update_pull_request_state", update_state)),
        w(Tool::official(
            "update_pull_request_draft_state",
            update_draft,
        )),
        w(Tool::official("hide_issue_comment", hide_issue_comment)).perm("issues", Access::Write),
        w(Tool::official("unhide_issue_comment", unhide_issue_comment))
            .perm("issues", Access::Write),
        w(Tool::official("hide_pull_request_review", hide_review)),
        w(Tool::official("unhide_pull_request_review", unhide_review)),
        w(Tool::official(
            "hide_pull_request_review_comment",
            hide_review_comment,
        )),
        w(Tool::official(
            "unhide_pull_request_review_comment",
            unhide_review_comment,
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};
    use std::sync::Arc;

    const NAMES: [&str; 29] = [
        "add_comment_to_pending_review",
        "add_reply_to_pull_request_comment",
        "create_pull_request",
        "list_pull_requests",
        "merge_pull_request",
        "pull_request_read",
        "pull_request_review_write",
        "search_pull_requests",
        "update_pull_request",
        "update_pull_request_branch",
        "add_pull_request_review_comment",
        "add_pull_request_review_comment_reaction",
        "create_pull_request_review",
        "delete_pending_pull_request_review",
        "remove_pull_request_review_comment_reaction",
        "request_pull_request_reviewers",
        "resolve_review_thread",
        "submit_pending_pull_request_review",
        "unresolve_review_thread",
        "update_pull_request_body",
        "update_pull_request_draft_state",
        "update_pull_request_state",
        "update_pull_request_title",
        "hide_issue_comment",
        "unhide_issue_comment",
        "hide_pull_request_review",
        "unhide_pull_request_review",
        "hide_pull_request_review_comment",
        "unhide_pull_request_review_comment",
    ];

    #[test]
    fn noms_presents() {
        let got: Vec<String> = tools().into_iter().map(|t| t.name).collect();
        assert_eq!(got.len(), 29);
        for n in NAMES {
            assert!(got.iter().any(|g| g == n), "manquant: {n}");
        }
    }

    #[test]
    fn equipes_et_utilisateurs() {
        let (u, t) = split_reviewers(&["alice-test".into(), "org-test/team-test".into()]);
        assert_eq!(
            (u, t),
            (
                vec!["alice-test".to_string()],
                vec!["team-test".to_string()]
            )
        );
    }

    /// Un seul test async : le mode test est un état global du processus (net::init).
    #[tokio::test]
    async fn chaque_outil_repond_en_mode_test() {
        ghmcp_core::net::init(true, ghmcp_core::Approver::Approve);
        let cfg = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_X\"\n";
        let ctx = Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
        let base = json!({"owner": "o-test", "repo": "r-test", "pullNumber": 7, "comment_id": 11, "review_id": 5, "commentId": 11,
            "reaction_id": 3, "content": "+1", "body": "texte-test", "title": "t-test", "state": "closed", "draft": true,
            "head": "h-test", "base": "b-test", "path": "a.txt", "subjectType": "FILE", "event": "COMMENT", "classifier": "spam",
            "threadID": "PRRT_test", "threadId": "PRRT_test", "reviewers": ["alice-test"], "query": "bug", "method": "get"});
        let all = tools();
        for t in &all {
            let mut a = base.clone();
            if t.name == "pull_request_review_write" {
                a["method"] = json!("create");
            }
            let r = (t.handler)(ctx.clone(), a).await;
            assert!(r.is_ok(), "{} : {:?}", t.name, r.err());
        }
        let read = all.iter().find(|t| t.name == "pull_request_read").unwrap();
        for m in [
            "get",
            "get_files",
            "get_commits",
            "get_reviews",
            "get_comments",
            "get_diff",
            "get_status",
            "get_check_runs",
            "get_review_comments",
        ] {
            let mut a = base.clone();
            a["method"] = json!(m);
            let r = (read.handler)(ctx.clone(), a).await;
            assert!(r.is_ok(), "pull_request_read/{m} : {:?}", r.err());
        }
        let mut a = base.clone();
        a["classifier"] = json!("nope");
        assert!(hide_issue_comment(ctx.clone(), a).await.is_err());
        let r = hide_issue_comment(ctx, base).await.unwrap();
        assert_eq!(r["node_id"], "TEST_NODE_ID");
    }
}
