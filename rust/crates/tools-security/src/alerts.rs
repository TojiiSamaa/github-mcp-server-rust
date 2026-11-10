//! Code/secret scanning, dependabot, advisories globales et de dépôt/org, code quality.
use crate::{bad, enc, opt, page_q, req_num};
use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, Level, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    let sec = ["security_events"];
    vec![
        Tool::official("get_code_scanning_alert", cs_get)
            .scopes(&sec)
            .perm("code_scanning_alerts", Access::Read),
        Tool::official("list_code_scanning_alerts", cs_list)
            .scopes(&sec)
            .perm("code_scanning_alerts", Access::Read)
            .level(Level::Securite),
        Tool::official("get_secret_scanning_alert", ss_get)
            .scopes(&sec)
            .perm("secret_scanning_alerts", Access::Read),
        Tool::official("list_secret_scanning_alerts", ss_list)
            .scopes(&sec)
            .perm("secret_scanning_alerts", Access::Read)
            .level(Level::Securite),
        Tool::official("get_dependabot_alert", db_get)
            .scopes(&sec)
            .perm("dependabot_alerts", Access::Read),
        Tool::official("list_dependabot_alerts", db_list)
            .scopes(&sec)
            .perm("dependabot_alerts", Access::Read)
            .level(Level::Securite),
        Tool::official("get_global_security_advisory", adv_get)
            .scopes(&sec)
            .perm("metadata", Access::Read),
        Tool::official("list_global_security_advisories", adv_list)
            .scopes(&sec)
            .perm("metadata", Access::Read),
        Tool::official("list_org_repository_security_advisories", adv_org)
            .scopes(&sec)
            .perm("organization_administration", Access::Read),
        Tool::official("list_repository_security_advisories", adv_repo)
            .scopes(&sec)
            .perm("repository_advisories", Access::Read),
        Tool::official("get_code_quality_finding", cq_get)
            .scopes(&["repo"])
            .perm("code_scanning_alerts", Access::Read),
    ]
}

fn or(args: &Value) -> Result<(&str, &str)> {
    Ok((req_str(args, "owner")?, req_str(args, "repo")?))
}

async fn one(ctx: Ctx, args: Value, kind: &str, key: &str) -> Result<Value> {
    let (o, r) = or(&args)?;
    let n = req_num(&args, key)?;
    ctx.gh(&args)?
        .rest("GET", &format!("/repos/{o}/{r}/{kind}/{n}"), &[], None)
        .await
}

async fn cs_get(ctx: Ctx, args: Value) -> Result<Value> {
    one(ctx, args, "code-scanning/alerts", "alertNumber").await
}
async fn ss_get(ctx: Ctx, args: Value) -> Result<Value> {
    one(ctx, args, "secret-scanning/alerts", "alertNumber").await
}
async fn db_get(ctx: Ctx, args: Value) -> Result<Value> {
    one(ctx, args, "dependabot/alerts", "alertNumber").await
}
async fn cq_get(ctx: Ctx, args: Value) -> Result<Value> {
    one(ctx, args, "code-quality/findings", "findingNumber").await
}

async fn cs_list(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = or(&args)?;
    let mut q = page_q(&args);
    q.push((
        "state",
        arg_str(&args, "state")
            .filter(|s| !s.is_empty())
            .unwrap_or("open")
            .into(),
    ));
    opt(&mut q, &args, "ref", "ref");
    opt(&mut q, &args, "severity", "severity");
    opt(&mut q, &args, "tool_name", "tool_name");
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/code-scanning/alerts"),
            &q,
            None,
        )
        .await
}

async fn ss_list(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = or(&args)?;
    let mut q = page_q(&args);
    opt(&mut q, &args, "state", "state");
    opt(&mut q, &args, "secret_type", "secret_type");
    opt(&mut q, &args, "resolution", "resolution");
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/secret-scanning/alerts"),
            &q,
            None,
        )
        .await
}

/// Dependabot : pagination par curseur ; rend `{alerts, pageInfo}` comme le Go.
async fn db_list(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = or(&args)?;
    let mut q = vec![];
    opt(&mut q, &args, "state", "state");
    opt(&mut q, &args, "severity", "severity");
    opt(&mut q, &args, "after", "after");
    if let Some(p) = arg_i64(&args, "perPage").filter(|p| *p > 0) {
        q.push(("per_page", p.to_string()));
    }
    let raw = ctx
        .gh(&args)?
        .rest_raw(
            "GET",
            &format!("/repos/{o}/{r}/dependabot/alerts"),
            &q,
            None,
        )
        .await?;
    let alerts: Value =
        serde_json::from_str(&raw.text).map_err(|e| bad(format!("réponse non JSON: {e}")))?;
    let link = raw
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("link"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    // En-tête Link : <https://api/...?after=CURSEUR>; rel="next"
    let after = link
        .split(',')
        .find(|p| p.contains("rel=\"next\""))
        .and_then(|p| p.split("after=").nth(1))
        .map(|s| s.split(['&', '>']).next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty());
    Ok(
        json!({"alerts": alerts, "pageInfo": {"hasNextPage": after.is_some(), "endCursor": after.unwrap_or_default()}}),
    )
}

async fn adv_get(ctx: Ctx, args: Value) -> Result<Value> {
    let id = req_str(&args, "ghsaId")?;
    ctx.gh(&args)?
        .rest("GET", &format!("/advisories/{}", enc(id)), &[], None)
        .await
}

async fn adv_list(ctx: Ctx, args: Value) -> Result<Value> {
    let mut q = vec![];
    for (from, to) in [
        ("ghsaId", "ghsa_id"),
        ("type", "type"),
        ("cveId", "cve_id"),
        ("ecosystem", "ecosystem"),
        ("severity", "severity"),
        ("affects", "affects"),
        ("published", "published"),
        ("updated", "updated"),
        ("modified", "modified"),
    ] {
        opt(&mut q, &args, from, to);
    }
    if let Some(c) = args
        .get("cwes")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
    {
        q.push((
            "cwes",
            c.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    if arg_bool(&args, "isWithdrawn") == Some(true) {
        q.push(("is_withdrawn", "true".into()));
    }
    ctx.gh(&args)?.rest("GET", "/advisories", &q, None).await
}

fn adv_q(args: &Value) -> Vec<(&'static str, String)> {
    let mut q = vec![];
    for k in ["direction", "sort", "state"] {
        opt(&mut q, args, k, k);
    }
    q
}

async fn adv_repo(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = or(&args)?;
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/security-advisories"),
            &adv_q(&args),
            None,
        )
        .await
}

async fn adv_org(ctx: Ctx, args: Value) -> Result<Value> {
    let org = req_str(&args, "org")?;
    ctx.gh(&args)?
        .rest(
            "GET",
            &format!("/orgs/{org}/security-advisories"),
            &adv_q(&args),
            None,
        )
        .await
}
