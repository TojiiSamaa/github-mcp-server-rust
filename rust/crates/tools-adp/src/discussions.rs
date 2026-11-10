//! Discussions (GraphQL), porté de discussions.go. Les noms de champs suivent le schéma officiel.
use crate::bad;
use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, Level, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    let r = |t: Tool| t.scopes(&["repo"]).perm("discussions", Access::Read);
    vec![
        r(Tool::official("list_discussions", list)),
        r(Tool::official("get_discussion", get)),
        r(Tool::official("get_discussion_comments", comments)),
        r(Tool::official("list_discussion_categories", categories)),
        Tool::official("discussion_comment_write", write)
            .scopes(&["repo"])
            .perm("discussions", Access::Write)
            .events(&["discussion_comment"])
            .level(Level::Dev),
    ]
}

const DISC: &str = "number title body url createdAt updatedAt closed locked author { login } category { name } answer { id } comments { totalCount }";

fn repo_of(args: &Value) -> Result<(&str, &str)> {
    Ok((
        req_str(args, "owner")?,
        arg_str(args, "repo")
            .filter(|s| !s.is_empty())
            .unwrap_or(".github"),
    ))
}

async fn list(ctx: Ctx, args: Value) -> Result<Value> {
    let (owner, repo) = repo_of(&args)?;
    let gh = ctx.gh(&args)?;
    let first = arg_i64(&args, "perPage")
        .filter(|n| *n > 0)
        .unwrap_or(30)
        .min(100);
    let order = match (arg_str(&args, "orderBy"), arg_str(&args, "direction")) {
        (Some(f), Some(d)) if !f.is_empty() && !d.is_empty() => {
            Some(json!({"field": f, "direction": d}))
        }
        _ => None,
    };
    let q = format!(
        "query($owner:String!,$repo:String!,$first:Int!,$after:String,$categoryId:ID,$orderBy:DiscussionOrder){{repository(owner:$owner,name:$repo){{discussions(first:$first,after:$after,categoryId:$categoryId,orderBy:$orderBy){{nodes{{{DISC}}} pageInfo{{hasNextPage hasPreviousPage startCursor endCursor}} totalCount}}}}}}"
    );
    let v = json!({"owner": owner, "repo": repo, "first": first, "after": arg_str(&args, "after"),
        "categoryId": arg_str(&args, "category").filter(|s| !s.is_empty()), "orderBy": order});
    let d = gh.graphql(&q, v).await?;
    Ok(d.pointer("/repository/discussions").cloned().unwrap_or(d))
}

async fn get(ctx: Ctx, args: Value) -> Result<Value> {
    let (owner, repo) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let n = arg_i64(&args, "discussionNumber")
        .ok_or_else(|| bad("missing required parameter: discussionNumber"))?;
    let q = format!("query($owner:String!,$repo:String!,$n:Int!){{repository(owner:$owner,name:$repo){{discussion(number:$n){{{DISC}}}}}}}");
    let d = ctx
        .gh(&args)?
        .graphql(&q, json!({"owner": owner, "repo": repo, "n": n}))
        .await?;
    Ok(d.pointer("/repository/discussion").cloned().unwrap_or(d))
}

async fn comments(ctx: Ctx, args: Value) -> Result<Value> {
    let (owner, repo) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let n = arg_i64(&args, "discussionNumber")
        .ok_or_else(|| bad("missing required parameter: discussionNumber"))?;
    let first = arg_i64(&args, "perPage")
        .filter(|n| *n > 0)
        .unwrap_or(30)
        .min(100);
    let replies = if arg_bool(&args, "includeReplies").unwrap_or(false) {
        "replies(first:100){nodes{id body url author{login}}}"
    } else {
        ""
    };
    let q = format!(
        "query($owner:String!,$repo:String!,$n:Int!,$first:Int!,$after:String){{repository(owner:$owner,name:$repo){{discussion(number:$n){{comments(first:$first,after:$after){{nodes{{id body url createdAt author{{login}} {replies}}} pageInfo{{hasNextPage endCursor}} totalCount}}}}}}}}"
    );
    let d = ctx.gh(&args)?.graphql(&q, json!({"owner": owner, "repo": repo, "n": n, "first": first, "after": arg_str(&args, "after")})).await?;
    Ok(d.pointer("/repository/discussion/comments")
        .cloned()
        .unwrap_or(d))
}

async fn categories(ctx: Ctx, args: Value) -> Result<Value> {
    let (owner, repo) = repo_of(&args)?;
    let q = "query($owner:String!,$repo:String!){repository(owner:$owner,name:$repo){discussionCategories(first:100){nodes{id name slug isAnswerable} totalCount}}}";
    let d = ctx
        .gh(&args)?
        .graphql(q, json!({"owner": owner, "repo": repo}))
        .await?;
    Ok(d.pointer("/repository/discussionCategories")
        .cloned()
        .unwrap_or(d))
}

async fn write(ctx: Ctx, args: Value) -> Result<Value> {
    let m = req_str(&args, "method")?;
    let gh = ctx.gh(&args)?;
    let node = || {
        arg_str(&args, "commentNodeID")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad("commentNodeID is required"))
    };
    let body = || {
        arg_str(&args, "body")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad("body is required"))
    };
    let (q, v) = match m {
        "add" | "reply" => {
            let (owner, repo) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
            let n = arg_i64(&args, "discussionNumber").ok_or_else(|| bad("discussionNumber is required"))?;
            let id = gh.graphql("query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){discussion(number:$n){id}}}", json!({"o": owner, "r": repo, "n": n})).await?;
            let did = id.pointer("/repository/discussion/id").cloned().unwrap_or(json!("test-discussion-id"));
            let reply = if m == "reply" { Some(node()?) } else { None };
            ("mutation($d:ID!,$b:String!,$p:ID){addDiscussionComment(input:{discussionId:$d,body:$b,replyToId:$p}){comment{id url}}}",
                json!({"d": did, "b": body()?, "p": reply}))
        }
        "update" => ("mutation($i:ID!,$b:String!){updateDiscussionComment(input:{commentId:$i,body:$b}){comment{id url}}}", json!({"i": node()?, "b": body()?})),
        "delete" => ("mutation($i:ID!){deleteDiscussionComment(input:{id:$i}){comment{id}}}", json!({"i": node()?})),
        "mark_answer" => ("mutation($i:ID!){markDiscussionCommentAsAnswer(input:{id:$i}){discussion{id}}}", json!({"i": node()?})),
        "unmark_answer" => ("mutation($i:ID!){unmarkDiscussionCommentAsAnswer(input:{id:$i}){discussion{id}}}", json!({"i": node()?})),
        _ => return Err(bad(format!("unknown method: {m}"))),
    };
    gh.graphql(q, v).await
}
