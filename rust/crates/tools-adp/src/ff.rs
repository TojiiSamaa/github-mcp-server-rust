//! Outils sous drapeau : issue_dependency_*, issue_artifact_*, find_duplicate (porté de issue_*.go, find_duplicate.go).
use crate::{bad, page_q};
use ghmcp_core::{arg_i64, arg_str, req_str, Access, Ctx, Level, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::official("issue_dependency_read", dep_read)
            .scopes(&["repo"])
            .perm("issues", Access::Read),
        Tool::official("issue_dependency_write", dep_write)
            .scopes(&["repo"])
            .perm("issues", Access::Write)
            .level(Level::Dev),
        Tool::official("issue_artifact_read", art_read)
            .scopes(&["repo"])
            .perm("issues", Access::Read),
        Tool::official("issue_artifact_write", art_write)
            .scopes(&["repo"])
            .perm("issues", Access::Write)
            .level(Level::Dev),
        Tool::official("find_duplicate", dup)
            .scopes(&["repo"])
            .perm("issues", Access::Read),
    ]
}

fn seg<'a>(args: &'a Value, k: &str) -> Result<&'a str> {
    let s = req_str(args, k)?;
    if s.trim().is_empty() || s == "." || s == ".." || s.contains(['/', '\\']) {
        return Err(bad(
            "owner and repo must be nonempty repository path segments",
        ));
    }
    Ok(s)
}

fn issue_no(args: &Value, k: &str) -> Result<i64> {
    arg_i64(args, k)
        .filter(|n| *n > 0)
        .ok_or_else(|| bad(format!("{k} must be positive")))
}

async fn dep_read(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, n) = (
        seg(&args, "owner")?,
        seg(&args, "repo")?,
        issue_no(&args, "issue_number")?,
    );
    let kind = match req_str(&args, "method")? {
        "get_blocked_by" => "blocked_by",
        "get_blocking" => "blocking",
        m => return Err(bad(format!("unknown method: {m}"))),
    };
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/issues/{n}/dependencies/{kind}"),
            &page_q(&args),
            None,
        )
        .await
}

async fn dep_write(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, n) = (
        seg(&args, "owner")?,
        seg(&args, "repo")?,
        issue_no(&args, "issue_number")?,
    );
    let rel = issue_no(&args, "related_issue_number")?;
    let (ro, rr) = (
        arg_str(&args, "related_owner").unwrap_or(o),
        arg_str(&args, "related_repo").unwrap_or(r),
    );
    let method = req_str(&args, "method")?;
    let kind = req_str(&args, "type")?;
    if !matches!(method, "add" | "remove") {
        return Err(bad(format!("unknown method: {method}")));
    }
    // Le côté « bloqué » porte la relation : blocked_by = sujet bloqué, blocking = lié bloqué.
    let ((bo, br, bn), (xo, xr, xn)) = match kind {
        "blocked_by" => ((o, r, n), (ro, rr, rel)),
        "blocking" => ((ro, rr, rel), (o, r, n)),
        _ => return Err(bad(format!("unknown type: {kind}"))),
    };
    let gh = ctx.gh(&args)?;
    let blocker = gh
        .rest("GET", &format!("/repos/{xo}/{xr}/issues/{xn}"), &[], None)
        .await?;
    let id = blocker["id"].as_i64().unwrap_or(1);
    let base = format!("/repos/{bo}/{br}/issues/{bn}/dependencies/blocked_by");
    if method == "add" {
        gh.rest("POST", &base, &[], Some(json!({"issue_id": id})))
            .await
    } else {
        gh.rest("DELETE", &format!("{base}/{id}"), &[], None).await
    }
}

fn art_path(args: &Value, number: Option<i64>) -> Result<String> {
    let (o, r, n) = (
        seg(args, "owner")?,
        seg(args, "repo")?,
        issue_no(args, "issue_number")?,
    );
    Ok(match number {
        Some(a) => format!("/repos/{o}/{r}/issues/{n}/artifacts/{a}"),
        None => format!("/repos/{o}/{r}/issues/{n}/artifacts"),
    })
}

async fn art_read(ctx: Ctx, args: Value) -> Result<Value> {
    match req_str(&args, "method")? {
        "list" => {
            let mut q = page_q(&args);
            if let Some(t) = arg_str(&args, "type").filter(|s| !s.is_empty()) {
                q.push(("type", t.into()));
            }
            ctx.gh(&args)?
                .rest("GET", &art_path(&args, None)?, &q, None)
                .await
        }
        "get" => {
            ctx.gh(&args)?
                .rest(
                    "GET",
                    &art_path(&args, Some(issue_no(&args, "artifact_number")?))?,
                    &[],
                    None,
                )
                .await
        }
        _ => Err(bad("method must be list or get")),
    }
}

async fn art_write(ctx: Ctx, args: Value) -> Result<Value> {
    let m = req_str(&args, "method")?;
    let mut b = json!({});
    for k in ["name", "type", "body"] {
        if let Some(v) = arg_str(&args, k) {
            b[k] = json!(v);
        }
    }
    match m {
        "create" => {
            ctx.gh(&args)?
                .rest("POST", &art_path(&args, None)?, &[], Some(b))
                .await
        }
        "update" => {
            ctx.gh(&args)?
                .rest(
                    "PATCH",
                    &art_path(&args, Some(issue_no(&args, "artifact_number")?))?,
                    &[],
                    Some(b),
                )
                .await
        }
        "delete" => {
            ctx.gh(&args)?
                .rest(
                    "DELETE",
                    &art_path(&args, Some(issue_no(&args, "artifact_number")?))?,
                    &[],
                    None,
                )
                .await?;
            Ok(json!({"deleted": true, "artifact_number": issue_no(&args, "artifact_number")?}))
        }
        _ => Err(bad("method must be create, update or delete")),
    }
}

async fn dup(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, n) = (
        seg(&args, "owner")?,
        seg(&args, "repo")?,
        issue_no(&args, "issue_number")?,
    );
    let mut q = page_q(&args);
    if let Some(t) = args.get("confidence_threshold").and_then(Value::as_f64) {
        q.push(("confidence_threshold", t.to_string()));
    }
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/issues/{n}/semantically_similar"),
            &q,
            None,
        )
        .await
}
