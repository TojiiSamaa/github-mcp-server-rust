//! actions_get, actions_list, actions_run_trigger, get_job_logs (porté de actions.go).
use crate::{bad, int_arg, page_q};
use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, Level, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::official("actions_get", get)
            .scopes(&["repo"])
            .perm("actions", Access::Read),
        Tool::official("actions_list", list)
            .scopes(&["repo"])
            .perm("actions", Access::Read),
        Tool::official("actions_run_trigger", trigger)
            .scopes(&["repo"])
            .perm("actions", Access::Write)
            .events(&["workflow_run"])
            .level(Level::Dev),
        Tool::official("get_job_logs", job_logs)
            .scopes(&["repo"])
            .perm("actions", Access::Read),
    ]
}

fn rid(args: &Value, method: &str) -> Result<i64> {
    int_arg(args, "resource_id")?.ok_or_else(|| {
        bad(format!(
            "missing required parameter for method {method}: resource_id"
        ))
    })
}

async fn list(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, m) = (
        req_str(&args, "owner")?,
        req_str(&args, "repo")?,
        req_str(&args, "method")?,
    );
    let gh = ctx.gh(&args)?;
    let mut q = page_q(&args);
    let base = format!("/repos/{o}/{r}");
    match m {
        "list_workflows" => {
            gh.rest("GET", &format!("{base}/actions/workflows"), &q, None)
                .await
        }
        "list_workflow_runs" => {
            let f = args.get("workflow_runs_filter");
            for k in ["actor", "branch", "event", "status"] {
                if let Some(v) = f.and_then(|f| f.get(k)).and_then(Value::as_str) {
                    q.push((k, v.into()));
                }
            }
            let path = match arg_str(&args, "resource_id") {
                None | Some("") => format!("{base}/actions/runs"),
                Some(w) => format!("{base}/actions/workflows/{}/runs", urlencoding::encode(w)),
            };
            gh.rest("GET", &path, &q, None).await
        }
        "list_workflow_jobs" => {
            let id = rid(&args, m)?;
            if let Some(f) = args
                .get("workflow_jobs_filter")
                .and_then(|f| f.get("filter"))
                .and_then(Value::as_str)
            {
                q.push(("filter", f.into()));
            }
            gh.rest("GET", &format!("{base}/actions/runs/{id}/jobs"), &q, None)
                .await
        }
        "list_workflow_run_artifacts" => {
            let id = rid(&args, m)?;
            gh.rest(
                "GET",
                &format!("{base}/actions/runs/{id}/artifacts"),
                &q,
                None,
            )
            .await
        }
        _ => Err(bad(format!("unknown method: {m}"))),
    }
}

/// URL du 302 (en-tête location), sans la suivre : comme le Go (maxRedirects=1, redirection non suivie).
async fn redirect_url(gh: &ghmcp_core::net::Gh, path: &str) -> Result<String> {
    let opts = ghmcp_core::net::ReqOpts {
        no_redirect: true,
        ..Default::default()
    };
    let raw = gh.rest_raw_with("GET", path, &[], None, &opts).await?;
    raw.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.clone())
        .ok_or_else(|| {
            ghmcp_core::GhError::Other(format!(
                "pas de redirection (statut {}) pour {path}",
                raw.status
            ))
        })
}

async fn get(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, m) = (
        req_str(&args, "owner")?,
        req_str(&args, "repo")?,
        req_str(&args, "method")?,
    );
    let gh = ctx.gh(&args)?;
    let base = format!("/repos/{o}/{r}/actions");
    if m == "get_workflow" {
        let w = req_str(&args, "resource_id")?;
        return gh
            .rest(
                "GET",
                &format!("{base}/workflows/{}", urlencoding::encode(w)),
                &[],
                None,
            )
            .await;
    }
    let id = rid(&args, m)?;
    match m {
        "get_workflow_run" => {
            gh.rest("GET", &format!("{base}/runs/{id}"), &[], None)
                .await
        }
        "get_workflow_job" => {
            gh.rest("GET", &format!("{base}/jobs/{id}"), &[], None)
                .await
        }
        "get_workflow_run_usage" => {
            gh.rest("GET", &format!("{base}/runs/{id}/timing"), &[], None)
                .await
        }
        "get_workflow_run_logs_url" => {
            let url = redirect_url(&gh, &format!("{base}/runs/{id}/logs")).await?;
            Ok(
                json!({"logs_url": url, "message": "Workflow run logs are available for download",
                "note": "The logs_url provides a download link for the complete workflow run logs as a ZIP archive. You can download this archive to extract and examine individual job logs.",
                "warning": "This downloads ALL logs as a ZIP file which can be large and expensive. For debugging failed jobs, consider using get_job_logs with failed_only=true and run_id instead.",
                "optimization_tip": format!("Use: get_job_logs with parameters {{run_id: {id}, failed_only: true}} for more efficient failed job debugging")}),
            )
        }
        "download_workflow_run_artifact" => {
            let url = redirect_url(&gh, &format!("{base}/artifacts/{id}/zip")).await?;
            Ok(
                json!({"download_url": url, "message": "Artifact is available for download",
                "note": "The download_url provides a download link for the artifact as a ZIP archive. The link is temporary and expires after a short time.",
                "artifact_id": id}),
            )
        }
        _ => Err(bad(format!("unknown method: {m}"))),
    }
}

async fn trigger(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r, m) = (
        req_str(&args, "owner")?,
        req_str(&args, "repo")?,
        req_str(&args, "method")?,
    );
    let base = format!("/repos/{o}/{r}/actions");
    if m == "run_workflow" {
        let w = arg_str(&args, "workflow_id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad("workflow_id is required for run_workflow action"))?;
        let git_ref = arg_str(&args, "ref")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad("ref is required for run_workflow action"))?;
        let gh = ctx.gh(&args)?;
        let inputs = args.get("inputs").cloned().unwrap_or(json!({}));
        let body = json!({"ref": git_ref, "inputs": inputs});
        gh.rest(
            "POST",
            &format!("{base}/workflows/{}/dispatches", urlencoding::encode(w)),
            &[],
            Some(body),
        )
        .await?;
        let kind = if w.parse::<i64>().is_ok() {
            "workflow_id"
        } else {
            "workflow_file"
        };
        return Ok(
            json!({"message": "Workflow run has been queued", "workflow_type": kind, "workflow_id": w,
            "ref": git_ref, "inputs": inputs, "status": "204 No Content", "status_code": 204}),
        );
    }
    let id = arg_i64(&args, "run_id")
        .filter(|i| *i != 0)
        .ok_or_else(|| bad("missing required parameter: run_id"))?;
    let gh = ctx.gh(&args)?;
    let (verb, path, msg) = match m {
        "rerun_workflow_run" => (
            "POST",
            format!("{base}/runs/{id}/rerun"),
            "Workflow run has been queued for re-run",
        ),
        "rerun_failed_jobs" => (
            "POST",
            format!("{base}/runs/{id}/rerun-failed-jobs"),
            "Failed jobs have been queued for re-run",
        ),
        "cancel_workflow_run" => (
            "POST",
            format!("{base}/runs/{id}/cancel"),
            "Workflow run has been cancelled",
        ),
        "delete_workflow_run_logs" => (
            "DELETE",
            format!("{base}/runs/{id}/logs"),
            "Workflow run logs have been deleted",
        ),
        _ => return Err(bad(format!("unknown method: {m}"))),
    };
    gh.rest(verb, &path, &[], None).await?;
    Ok(json!({"message": msg, "run_id": id, "method": m}))
}

fn tail(text: &str, n: usize) -> (String, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    (
        lines[lines.len().saturating_sub(n)..].join("\n"),
        lines.len(),
    )
}

async fn job_logs(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let (job, run) = (
        arg_i64(&args, "job_id").unwrap_or(0),
        arg_i64(&args, "run_id").unwrap_or(0),
    );
    let failed_only = arg_bool(&args, "failed_only").unwrap_or(false);
    if failed_only && run == 0 {
        return Err(bad("run_id is required when failed_only is true"));
    }
    if !failed_only && job == 0 {
        return Err(bad("job_id is required when failed_only is false"));
    }
    let content = arg_bool(&args, "return_content").unwrap_or(false);
    let n = arg_i64(&args, "tail_lines")
        .filter(|n| *n > 0)
        .unwrap_or(500) as usize;
    let gh = ctx.gh(&args)?;
    let base = format!("/repos/{o}/{r}/actions");
    let one = |id: i64, name: Option<String>| {
        let (gh, path) = (gh.clone(), format!("{base}/jobs/{id}/logs"));
        async move {
            let raw = gh.rest_raw("GET", &path, &[], None).await?;
            Ok::<Value, ghmcp_core::GhError>(if content {
                let (text, total) = tail(&raw.text, n);
                json!({"job_id": id, "job_name": name, "logs_content": text, "original_length": total, "message": "Job logs content retrieved successfully"})
            } else {
                json!({"job_id": id, "job_name": name, "logs_url": format!("{}{path}", gh.account.api_url), "message": "Job logs are available for download"})
            })
        }
    };
    if !failed_only {
        return one(job, None).await;
    }
    let jobs = gh
        .rest(
            "GET",
            &format!("{base}/runs/{run}/jobs"),
            &[("filter", "latest".into())],
            None,
        )
        .await?;
    let all = jobs["jobs"].as_array().cloned().unwrap_or_default();
    let failed: Vec<&Value> = all
        .iter()
        .filter(|j| j["conclusion"] == "failure")
        .collect();
    if failed.is_empty() {
        return Ok(
            json!({"message": "No failed jobs found in this workflow run", "run_id": run, "total_jobs": all.len()}),
        );
    }
    let mut logs = vec![];
    for j in &failed {
        let id = j["id"].as_i64().unwrap_or(0);
        logs.push(
            one(id, j["name"].as_str().map(String::from))
                .await
                .unwrap_or_else(|e| json!({"error": {"job_id": id, "error": e.to_string()}})),
        );
    }
    Ok(
        json!({"message": format!("Retrieved logs for {} failed jobs", failed.len()), "run_id": run, "total_jobs": all.len(),
        "failed_jobs": failed.len(), "logs": logs, "return_format": {"content": content, "urls": !content}}),
    )
}
