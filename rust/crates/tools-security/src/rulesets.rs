//! create_repository_ruleset, repository_ruleset_read, custom_properties_read, custom_properties_write.
use crate::{bad, enc, opt, page_q};
use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, Result, Tool};
use serde_json::{json, Map, Value};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::official("create_repository_ruleset", create)
            .scopes(&["repo", "admin:org", "admin:enterprise"])
            .perm("administration", Access::Write),
        Tool::official("repository_ruleset_read", read)
            .scopes(&["repo", "read:org", "read:enterprise"])
            .perm("administration", Access::Read),
        Tool::official("custom_properties_read", props_read)
            .scopes(&["repo", "read:org", "read:enterprise"])
            .perm("repository_custom_properties", Access::Read),
        Tool::official("custom_properties_write", props_write)
            .scopes(&["repo", "admin:org", "admin:enterprise"])
            .perm("repository_custom_properties", Access::Write),
    ]
}

/// Base REST du niveau : (`/repos/o/r`, `/orgs/x` ou `/enterprises/x`).
fn base(args: &Value) -> Result<(String, &str)> {
    let level = req_str(args, "level")?;
    let b = match level {
        "repository" => format!(
            "/repos/{}/{}",
            req_str(args, "owner")?,
            req_str(args, "repo")?
        ),
        "organization" => format!("/orgs/{}", req_str(args, "org")?),
        "enterprise" => format!("/enterprises/{}", req_str(args, "enterprise")?),
        _ => {
            return Err(bad(format!(
                "unknown level: {level:?} (expected 'repository', 'organization', or 'enterprise')"
            )))
        }
    };
    Ok((b, level))
}

fn id_arg(args: &Value, k: &str) -> Result<i64> {
    arg_i64(args, k).ok_or_else(|| bad(format!("missing required parameter: {k}")))
}

async fn read(ctx: Ctx, args: Value) -> Result<Value> {
    let (b, level) = base(&args)?;
    let method = req_str(&args, "method")?;
    let unsupported = |sup: &str| {
        bad(format!(
            "method {method:?} is not supported for level {level:?}; supported methods: {sup}"
        ))
    };
    let mut q = page_q(&args);
    let gh = ctx.gh(&args)?;
    let path = match (method, level) {
        ("get", _) => {
            if level == "repository" {
                // includes_parents vaut true par défaut
                q.push((
                    "includes_parents",
                    arg_bool(&args, "includes_parents")
                        .unwrap_or(true)
                        .to_string(),
                ));
            }
            format!("{b}/rulesets/{}", id_arg(&args, "ruleset_id")?)
        }
        ("list", _) => {
            if level == "repository" {
                if let Some(v) = arg_bool(&args, "includes_parents") {
                    q.push(("includes_parents", v.to_string()));
                }
            }
            format!("{b}/rulesets")
        }
        ("get_rules_for_branch", "repository") => {
            format!("{b}/rules/branches/{}", enc(req_str(&args, "branch")?))
        }
        ("list_rule_suites", "repository" | "organization") => {
            for k in [
                "ref",
                "time_period",
                "actor_name",
                "rule_suite_result",
                "evaluate_status",
            ] {
                opt(&mut q, &args, k, k);
            }
            if level == "organization" {
                opt(&mut q, &args, "repository_name", "repository_name");
            }
            format!("{b}/rulesets/rule-suites")
        }
        ("get_rule_suite", "repository" | "organization") => format!(
            "{b}/rulesets/rule-suites/{}",
            id_arg(&args, "rule_suite_id")?
        ),
        _ => {
            return Err(match level {
                "organization" => unsupported("get, list, list_rule_suites, get_rule_suite"),
                "enterprise" => unsupported("get, list"),
                _ => bad(format!("unknown method: {method:?}")),
            })
        }
    };
    gh.rest("GET", &path, &q, None).await
}

/// Construit le corps du ruleset en validant ce que valide le Go (types de règles uniques, bypass_mode).
fn build(args: &Value) -> Result<Value> {
    let mut body = Map::new();
    body.insert("name".into(), json!(req_str(args, "name")?));
    body.insert("enforcement".into(), json!(req_str(args, "enforcement")?));
    body.insert(
        "target".into(),
        json!(arg_str(args, "target")
            .filter(|t| !t.is_empty())
            .unwrap_or("branch")),
    );
    let rules = args
        .get("rules")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("missing required parameter: rules"))?;
    let mut seen = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        let o = r
            .as_object()
            .ok_or_else(|| bad(format!("rules[{i}] must be an object")))?;
        if let Some(k) = o.keys().find(|k| *k != "type" && *k != "parameters") {
            return Err(bad(format!(
                "rules[{i}]: unsupported or unrecognized key: {k:?}"
            )));
        }
        let t = o
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| bad(format!("rules[{i}].type is required and must be a string")))?;
        if seen.contains(&t) {
            return Err(bad(format!(
                "duplicate rule type: {t:?} (a ruleset may only have one rule of each type)"
            )));
        }
        seen.push(t);
        if o.get("parameters").is_some_and(|p| !p.is_object()) {
            return Err(bad(format!("rule {t:?}: parameters must be an object")));
        }
    }
    body.insert("rules".into(), json!(rules));
    if let Some(c) = args.get("conditions").filter(|c| !c.is_null()) {
        body.insert("conditions".into(), c.clone());
    }
    if let Some(b) = args.get("bypass_actors").and_then(Value::as_array) {
        for (i, a) in b.iter().enumerate() {
            let m = a
                .get("bypass_mode")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    bad(format!(
                        "bypass_actors[{i}].bypass_mode is required and must be a string"
                    ))
                })?;
            if !["always", "pull_request", "exempt"].contains(&m) {
                return Err(bad(format!("bypass_actors[{i}].bypass_mode must be one of \"always\", \"pull_request\", or \"exempt\"")));
            }
        }
        body.insert("bypass_actors".into(), json!(b));
    }
    Ok(Value::Object(body))
}

async fn create(ctx: Ctx, args: Value) -> Result<Value> {
    let (b, _) = base(&args)?;
    let body = build(&args)?;
    ctx.gh(&args)?
        .rest("POST", &format!("{b}/rulesets"), &[], Some(body))
        .await
}

async fn props_read(ctx: Ctx, args: Value) -> Result<Value> {
    let (b, level) = base(&args)?;
    let path = if level == "repository" {
        format!("{b}/properties/values")
    } else {
        format!("{b}/properties/schema")
    };
    ctx.gh(&args)?.rest("GET", &path, &[], None).await
}

/// Champs d une définition que l appelant peut changer ; ceux omis gardent la valeur actuelle.
const DEF_FIELDS: [&str; 7] = [
    "value_type",
    "required",
    "default_value",
    "description",
    "allowed_values",
    "values_editable_by",
    "require_explicit_values",
];

fn merge(current: &Value, requested: &[Value], source: &str) -> Result<Vec<Value>> {
    let mut out: Vec<Value> = current.as_array().cloned().unwrap_or_default();
    for (i, r) in requested.iter().enumerate() {
        let name = r
            .get("property_name")
            .and_then(Value::as_str)
            .ok_or_else(|| bad(format!("properties[{i}].property_name is required")))?;
        match out
            .iter_mut()
            .find(|d| d.get("property_name").and_then(Value::as_str) == Some(name))
        {
            Some(d) => {
                if let Some(src) = d
                    .get("source_type")
                    .and_then(Value::as_str)
                    .filter(|s| *s != source)
                {
                    return Err(bad(format!("property {name:?} is inherited from {src} and cannot be updated at the {source} level")));
                }
                for f in DEF_FIELDS.iter().filter(|f| r.get(**f).is_some()) {
                    d[*f] = r[*f].clone();
                }
            }
            None => {
                if r.get("value_type").is_none() {
                    return Err(bad(format!(
                        "properties[{i}].value_type is required for new property {name:?}"
                    )));
                }
                out.push(r.clone());
            }
        }
    }
    Ok(out)
}

async fn props_write(ctx: Ctx, args: Value) -> Result<Value> {
    let (b, level) = base(&args)?;
    let props = args
        .get("properties")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| bad("missing required parameter: properties"))?;
    let mut names = Vec::new();
    for (i, p) in props.iter().enumerate() {
        let n = p
            .get("property_name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| bad(format!("properties[{i}].property_name is required")))?;
        if names.contains(&n) {
            return Err(bad(format!(
                "properties[{i}].property_name duplicates {n:?}"
            )));
        }
        names.push(n);
    }
    let gh = ctx.gh(&args)?;
    if level == "repository" {
        if let Some(i) = props.iter().position(|p| p.get("value").is_none()) {
            return Err(bad(format!("properties[{i}].value is required")));
        }
        gh.rest(
            "PATCH",
            &format!("{b}/properties/values"),
            &[],
            Some(json!({"properties": props})),
        )
        .await?;
        return Ok(json!(
            "Repository custom property values updated successfully"
        ));
    }
    let path = format!("{b}/properties/schema");
    let current = gh.rest("GET", &path, &[], None).await?;
    let merged = merge(&current, props, level)?;
    gh.rest("PATCH", &path, &[], Some(json!({"properties": merged})))
        .await
}
