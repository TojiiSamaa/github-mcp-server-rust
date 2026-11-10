//! ghmcp-tools-issues : outils officiels issues (+ sous-issues, réactions, champs, labels).
//! Contrat : voir l'en-tête AGENTS-CONTRAT de crates/core/src/lib.rs. Portage de issues.go,
//! issues_granular.go, issue_fields.go, labels.go (REST là où l'API le permet, GraphQL pour les champs).

use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, GhError, Result, Tool};
use serde_json::{json, Value};

type Q = Vec<(&'static str, String)>;

fn int(a: &Value, k: &str) -> Result<i64> {
    arg_i64(a, k).ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))
}
fn base(a: &Value) -> Result<String> {
    Ok(format!(
        "/repos/{}/{}",
        req_str(a, "owner")?,
        req_str(a, "repo")?
    ))
}
fn issue_path(a: &Value) -> Result<String> {
    Ok(format!("{}/issues/{}", base(a)?, int(a, "issue_number")?))
}
fn page(a: &Value, q: &mut Q) {
    if let Some(p) = arg_i64(a, "page") {
        q.push(("page", p.to_string()));
    }
    if let Some(p) = arg_i64(a, "perPage") {
        q.push(("per_page", p.to_string()));
    }
}
const REACTIONS: [&str; 8] = [
    "+1", "-1", "laugh", "confused", "heart", "hooray", "rocket", "eyes",
];
fn reaction(v: &str) -> Result<()> {
    REACTIONS.contains(&v).then_some(()).ok_or_else(|| {
        GhError::InvalidArgs(
            "reaction must be one of +1, -1, laugh, confused, heart, hooray, rocket, eyes".into(),
        )
    })
}
/// PATCH d'une issue avec les seuls champs donnés (base des outils granulaires).
async fn patch(ctx: Ctx, a: Value, body: Value) -> Result<Value> {
    ctx.gh(&a)?
        .rest("PATCH", &issue_path(&a)?, &[], Some(body))
        .await
}
fn arr(a: &Value, k: &str) -> Result<Value> {
    a.get(k)
        .filter(|v| v.is_array())
        .cloned()
        .ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))
}
fn check_duplicate(a: &Value) -> Result<()> {
    if arg_i64(a, "duplicate_of").is_some() && arg_str(a, "state_reason") != Some("duplicate") {
        return Err(GhError::InvalidArgs(
            "duplicate_of can only be used when state_reason is 'duplicate'".into(),
        ));
    }
    Ok(())
}

// ---- issues.go ----
async fn issue_read(ctx: Ctx, a: Value) -> Result<Value> {
    let p = issue_path(&a)?;
    let gh = ctx.gh(&a)?;
    let mut q: Q = vec![];
    page(&a, &mut q);
    match req_str(&a, "method")? {
        "get" => {
            let mut issue = gh.rest("GET", &p, &[], None).await?;
            if let Some(id) = issue["node_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
            {
                // issues.go:2471 fetchIssueReadEnrichment ; erreur ignorée comme le Go (issues.go:944)
                if let Ok(r) = gh.graphql(&issue_read_enrich(), json!({"ids": [id]})).await {
                    apply_enrichment(&mut issue, &r["nodes"][0]);
                }
            }
            Ok(issue)
        }
        "get_comments" => gh.rest("GET", &format!("{p}/comments"), &q, None).await,
        "get_sub_issues" => gh.rest("GET", &format!("{p}/sub_issues"), &q, None).await,
        "get_parent" => gh.rest("GET", &format!("{p}/parent"), &[], None).await,
        "get_labels" => gh.rest("GET", &format!("{p}/labels"), &q, None).await,
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}

async fn issue_write(ctx: Ctx, a: Value) -> Result<Value> {
    let gh = ctx.gh(&a)?;
    let b = base(&a)?;
    let mut body = json!({});
    for k in ["title", "body", "assignees", "labels", "type"] {
        if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
            body[k] = v.clone();
        }
    }
    if let Some(m) = arg_i64(&a, "milestone").filter(|m| *m > 0) {
        body["milestone"] = json!(m);
    }
    match req_str(&a, "method")? {
        "create" => {
            if arg_str(&a, "title").is_none() {
                return Err(GhError::InvalidArgs(
                    "missing required parameter: title".into(),
                ));
            }
            let created = gh
                .rest("POST", &format!("{b}/issues"), &[], Some(body))
                .await?;
            if let Some(parent) = arg_i64(&a, "parent_issue_number").filter(|n| *n > 0) {
                let po = arg_str(&a, "parent_owner").unwrap_or(req_str(&a, "owner")?);
                let pr = arg_str(&a, "parent_repo").unwrap_or(req_str(&a, "repo")?);
                let id = created.get("id").cloned().unwrap_or(json!(0));
                gh.rest(
                    "POST",
                    &format!("/repos/{po}/{pr}/issues/{parent}/sub_issues"),
                    &[],
                    Some(json!({"sub_issue_id": id})),
                )
                .await?;
            }
            Ok(created)
        }
        "update" => {
            let n = arg_i64(&a, "issue_number").ok_or_else(|| {
                GhError::InvalidArgs("issue_number is required for update method".into())
            })?;
            for k in ["state", "state_reason"] {
                if let Some(s) = arg_str(&a, k) {
                    body[k] = json!(s);
                }
            }
            check_duplicate(&a)?;
            gh.rest("PATCH", &format!("{b}/issues/{n}"), &[], Some(body))
                .await
        }
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}

async fn list_issue_types(ctx: Ctx, a: Value) -> Result<Value> {
    let o = req_str(&a, "owner")?;
    // issues.go:1338 : repo fourni -> types du dépôt, sinon ceux de l'org
    let p = match arg_str(&a, "repo").filter(|r| !r.is_empty()) {
        Some(r) => format!("/repos/{o}/{r}/issue-types"),
        None => format!("/orgs/{o}/issue-types"),
    };
    ctx.gh(&a)?.rest("GET", &p, &[], None).await
}

// issues.go:3735 ListIssues : liste GraphQL (curseur `after`), field_filters résolus via issueFieldsRepoQuery (issue_fields.go:176).
const LIST_ISSUE_FRAG: &str = "number title body state databaseId author { login } createdAt updatedAt labels(first:100){ nodes { name id description } } assignees(first:100){ nodes { login } } comments { totalCount }";
fn list_issues_query(labels: bool, since: bool, field_values: bool) -> String {
    let (lv, la) = if labels {
        (",$labels:[String!]", ", labels:$labels")
    } else {
        ("", "")
    };
    let sv = if since { ",$since:DateTime" } else { "" };
    let fv = if field_values {
        ",$issueFieldValues:[IssueFieldValueFilter!]"
    } else {
        ""
    };
    let filter = match (since, field_values) {
        (true, true) => ", filterBy:{since:$since, issueFieldValues:$issueFieldValues}",
        (false, true) => ", filterBy:{issueFieldValues:$issueFieldValues}",
        (true, false) => ", filterBy:{since:$since}",
        (false, false) => "",
    };
    let (frag, fref) = if field_values {
        (format!("{LIST_ISSUE_FRAG} {FV_FRAG}"), FIELD_REF)
    } else {
        (LIST_ISSUE_FRAG.to_string(), "")
    };
    format!("query($owner:String!,$repo:String!,$states:[IssueState!],$orderBy:IssueOrderField!,$direction:OrderDirection!,$first:Int!,$after:String{lv}{sv}{fv}){{ repository(owner:$owner,name:$repo){{ isPrivate issues(first:$first, after:$after{la}, states:$states, orderBy:{{field:$orderBy, direction:$direction}}{filter}){{ nodes {{ {frag} }} pageInfo {{ hasNextPage hasPreviousPage startCursor endCursor }} totalCount }} }} }} {fref}")
}

/// minimal_types.go:979 convertToMinimalIssuesResponse, depuis `data` d'une réponse GraphQL.
fn minimal_issues(d: &Value) -> Value {
    let q = &d["repository"]["issues"];
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let issues: Vec<Value> = q["nodes"].as_array().into_iter().flatten().map(|n| {
        let mut m = json!({
            "number": n["number"].as_i64().unwrap_or(0), "title": s(&n["title"]), "state": s(&n["state"]),
            "user": {"login": s(&n["author"]["login"])},
            "assignees": n["assignees"]["nodes"].as_array().into_iter().flatten().map(|x| s(&x["login"])).collect::<Vec<_>>(),
            "created_at": s(&n["createdAt"]), "updated_at": s(&n["updatedAt"]),
        });
        // champs `omitempty` du Go : absents quand vides
        if !s(&n["body"]).is_empty() {
            m["body"] = n["body"].clone();
        }
        let labels: Vec<String> = n["labels"]["nodes"].as_array().into_iter().flatten().map(|x| s(&x["name"])).collect();
        if !labels.is_empty() {
            m["labels"] = json!(labels);
        }
        if let Some(c) = n["comments"]["totalCount"].as_i64().filter(|c| *c > 0) {
            m["comments"] = json!(c);
        }
        let fv = field_values(n);
        if fv.as_array().is_some_and(|a| !a.is_empty()) {
            m["field_values"] = fv;
        }
        m
    }).collect();
    let p = &q["pageInfo"];
    json!({"issues": issues, "totalCount": q["totalCount"].as_i64().unwrap_or(0),
        "pageInfo": {"hasNextPage": p["hasNextPage"].as_bool().unwrap_or(false), "hasPreviousPage": p["hasPreviousPage"].as_bool().unwrap_or(false), "startCursor": s(&p["startCursor"]), "endCursor": s(&p["endCursor"])}})
}

/// issues.go:4041 resolveFieldFilters : `fields` = `data` de la requête issueFields (FIELDS_FRAG).
fn resolve_field_filters(raw: &[(String, String)], fields: &Value) -> Result<Vec<Value>> {
    let nodes: Vec<&Value> = fields["repository"]["issueFields"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let name = |f: &Value| f["name"].as_str().unwrap_or("").to_string();
    let known = nodes.iter().map(|f| name(f)).collect::<Vec<_>>().join(", ");
    let err = |m: String| GhError::InvalidArgs(format!("field_filters: {m}"));
    raw.iter()
        .map(|(fname, val)| {
            let f = nodes
                .iter()
                .find(|f| name(f).eq_ignore_ascii_case(fname))
                .ok_or_else(|| err(format!("unknown field {fname:?}. Known fields: {known}")))?;
            let n = name(f);
            let (slot, v) = match f["__typename"].as_str().unwrap_or("") {
                "IssueFieldSingleSelect" => {
                    let opts: Vec<String> = f["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(name)
                        .collect();
                    let m = opts
                        .iter()
                        .find(|o| o.eq_ignore_ascii_case(val))
                        .ok_or_else(|| {
                            err(format!(
                                "{val:?} is not a valid option for {n:?}. Valid options: {}",
                                opts.join(", ")
                            ))
                        })?;
                    ("singleSelectOptionValue", json!(m))
                }
                "IssueFieldText" => ("textValue", json!(val)),
                "IssueFieldDate" => {
                    let ok = val.len() == 10
                        && val.bytes().enumerate().all(|(i, b)| {
                            if i == 4 || i == 7 {
                                b == b'-'
                            } else {
                                b.is_ascii_digit()
                            }
                        });
                    if !ok {
                        return Err(err(format!(
                            "{val:?} is not a valid date for {n:?} (expected YYYY-MM-DD)"
                        )));
                    }
                    ("dateValue", json!(val))
                }
                "IssueFieldNumber" => (
                    "numberValue",
                    json!(val.parse::<f64>().map_err(|e| err(format!(
                        "{val:?} is not a valid number for {n:?}: {e}"
                    )))?),
                ),
                t => return Err(err(format!("field {n:?} has unsupported data_type {t:?}"))),
            };
            Ok(json!({"fieldName": n, slot: v}))
        })
        .collect()
}

/// issues.go:775 isUnsupportedListIssuesIssueFieldsError : schéma sans issueFieldValues -> requête de repli.
fn unsupported_field_values(e: &GhError) -> bool {
    let m = e.to_string();
    m.contains("IssueFieldValueFilter")
        || (m.contains("issueFieldValues")
            && (m.contains("doesn't exist on type")
                || m.contains("doesn't accept argument")
                || (m.contains("Argument 'filterBy'") && m.contains("invalid value"))))
}

async fn list_issues(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = (req_str(&a, "owner")?, req_str(&a, "repo")?);
    if a.get("page").is_some_and(|p| !p.is_null()) {
        return Err(GhError::InvalidArgs("This tool uses cursor-based pagination. Use the 'after' parameter with the 'endCursor' value from the previous response instead of 'page'.".into()));
    }
    let mut raw = vec![];
    for f in a
        .get("field_filters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (n, v) = (
            f["field_name"].as_str().unwrap_or(""),
            f["value"].as_str().unwrap_or(""),
        );
        if n.is_empty() {
            return Err(GhError::InvalidArgs(
                "field_filters entry: missing required parameter: field_name".into(),
            ));
        }
        if v.is_empty() {
            return Err(GhError::InvalidArgs(format!(
                "field_filters entry {n:?}: missing required parameter: value"
            )));
        }
        raw.push((n.to_string(), v.to_string()));
    }
    let up = |k: &str| arg_str(&a, k).unwrap_or("").to_uppercase();
    let states = match up("state").as_str() {
        s @ ("OPEN" | "CLOSED") => json!([s]),
        _ => json!(["OPEN", "CLOSED"]),
    };
    let order = match up("orderBy").as_str() {
        s @ ("CREATED_AT" | "UPDATED_AT" | "COMMENTS") => s.to_string(),
        _ => "CREATED_AT".into(),
    };
    let dir = match up("direction").as_str() {
        s @ ("ASC" | "DESC") => s.to_string(),
        _ => "DESC".into(),
    };
    let labels: Vec<&str> = a
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    // parseISOTimestamp (issues.go:4105) : RFC3339 ou YYYY-MM-DD
    let since = arg_str(&a, "since").filter(|s| !s.is_empty()).map(|s| {
        if s.len() == 10 {
            format!("{s}T00:00:00Z")
        } else {
            s.to_string()
        }
    });
    let gh = ctx.gh(&a)?;
    let hdr = [(
        "GraphQL-Features".to_string(),
        "issue_fields, repo_issue_fields".to_string(),
    )];
    let mut filters = vec![];
    if !raw.is_empty() {
        let d = gh.graphql_with(&format!("query($owner:String!,$name:String!){{ repository(owner:$owner,name:$name){{ issueFields(first:100){{ {FIELDS_FRAG} }} }} }}"), json!({"owner": o, "name": r}), &hdr).await?;
        if d.get("test_mode").is_some() {
            return Ok(d); // mode test : pas de définitions de champs simulées
        }
        filters = resolve_field_filters(&raw, &d)?;
    }
    let mut vars = json!({"owner": o, "repo": r, "states": states, "orderBy": order, "direction": dir,
        "first": arg_i64(&a, "perPage").unwrap_or(30).clamp(1, 100), "after": arg_str(&a, "after"), "issueFieldValues": filters});
    if !labels.is_empty() {
        vars["labels"] = json!(labels);
    }
    let has_since = since.is_some();
    if let Some(s) = since {
        vars["since"] = json!(s);
    }
    let d = match gh
        .graphql_with(
            &list_issues_query(!labels.is_empty(), has_since, true),
            vars.clone(),
            &hdr,
        )
        .await
    {
        Ok(d) => d,
        Err(e) if raw.is_empty() && unsupported_field_values(&e) => {
            if let Some(m) = vars.as_object_mut() {
                m.remove("issueFieldValues");
            }
            gh.graphql(
                &list_issues_query(!labels.is_empty(), has_since, false),
                vars,
            )
            .await?
        }
        Err(e) => return Err(e),
    };
    if d.get("test_mode").is_some() {
        return Ok(d);
    }
    Ok(minimal_issues(&d))
}

async fn search_issues(ctx: Ctx, a: Value) -> Result<Value> {
    let mut query = format!("is:issue {}", req_str(&a, "query")?);
    match (arg_str(&a, "owner"), arg_str(&a, "repo")) {
        (Some(o), Some(r)) => query.push_str(&format!(" repo:{o}/{r}")),
        (Some(o), None) => query.push_str(&format!(" user:{o}")),
        _ => {}
    }
    let mut q: Q = vec![("q", query)];
    for k in ["sort", "order"] {
        if let Some(s) = arg_str(&a, k) {
            q.push((if k == "sort" { "sort" } else { "order" }, s.into()));
        }
    }
    page(&a, &mut q);
    let gh = ctx.gh(&a)?;
    let mut res = gh.rest("GET", "/search/issues", &q, None).await?;
    // issues.go:2359 fetchIssueFieldValuesByNodeID : un nodes(ids:) pour les champs de toutes les issues
    let ids: Vec<String> = res["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["node_id"].as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if !ids.is_empty() {
        let r = gh.graphql(&format!("query($ids:[ID!]!){{ nodes(ids:$ids){{ ... on Issue {{ id {FV_FRAG} }} }} }} {FIELD_REF}"), json!({"ids": ids})).await?;
        let by_id: std::collections::HashMap<&str, Value> = r["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| Some((n["id"].as_str()?, field_values(n))))
            .collect();
        for it in res["items"].as_array_mut().into_iter().flatten() {
            if let Some(v) = it["node_id"].as_str().and_then(|id| by_id.get(id)).cloned() {
                it["field_values"] = v;
            }
        }
    }
    Ok(res)
}

// ---- enrichissement GraphQL du Go (issues.go:2343-2540, minimal_types.go:953) ----
const FV_FRAG: &str = "issueFieldValues(first:25){ nodes { __typename ... on IssueFieldDateValue { field { ...F } value } ... on IssueFieldNumberValue { field { ...F } valueNumber: value } ... on IssueFieldSingleSelectValue { field { ...F } value } ... on IssueFieldTextValue { field { ...F } value } } }";
const FIELD_REF: &str = "fragment F on IssueFields { ... on IssueFieldDate { name } ... on IssueFieldNumber { name } ... on IssueFieldSingleSelect { name } ... on IssueFieldText { name } }";
fn issue_read_enrich() -> String {
    format!("query($ids:[ID!]!){{ nodes(ids:$ids){{ ... on Issue {{ id {FV_FRAG} parent {{ number title state url author {{ login }} repository {{ nameWithOwner }} }} closedByPullRequestsReferences(first:5, includeClosedPrs:true, orderByState:true){{ totalCount nodes {{ number title state url author {{ login }} repository {{ nameWithOwner }} }} }} subIssuesSummary {{ total completed percentCompleted }} }} }} }} {FIELD_REF}")
}

fn field_values(n: &Value) -> Value {
    let vals: Vec<Value> = n["issueFieldValues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|fv| {
            let v = match fv["__typename"].as_str()? {
                "IssueFieldNumberValue" => fv["valueNumber"].as_f64().map(|f| f.to_string())?,
                "IssueFieldDateValue" | "IssueFieldSingleSelectValue" | "IssueFieldTextValue" => {
                    fv["value"].as_str()?.to_string()
                }
                _ => return None,
            };
            Some(json!({"field": fv["field"]["name"], "value": v}))
        })
        .collect();
    json!(vals)
}
fn gql_ref(r: &Value) -> Value {
    json!({"number": r["number"], "title": r["title"], "state": r["state"], "url": r["url"], "repository": r["repository"]["nameWithOwner"]})
}
/// applyIssueReadEnrichment (issues.go:957), hors lockdown.
fn apply_enrichment(issue: &mut Value, n: &Value) {
    if !n.is_object() || issue["node_id"] != n["id"] {
        return;
    }
    issue["field_values"] = field_values(n);
    let total = n["subIssuesSummary"]["total"].as_i64().unwrap_or(0);
    issue["has_parent"] = json!(n["parent"].is_object());
    issue["has_children"] = json!(total > 0);
    if n["parent"].is_object() {
        issue["parent"] = gql_ref(&n["parent"]);
    }
    let c = &n["closedByPullRequestsReferences"];
    let refs: Vec<Value> = c["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(gql_ref)
        .collect();
    issue["closed_by_pull_requests"] =
        json!({"total_count": c["totalCount"].as_i64().unwrap_or(0), "references": refs});
    if total > 0 {
        let s = &n["subIssuesSummary"];
        issue["sub_issues_summary"] = json!({"total": total, "completed": s["completed"], "percent_completed": s["percentCompleted"]});
    }
}

async fn add_issue_comment(ctx: Ctx, a: Value) -> Result<Value> {
    let (p, body) = (issue_path(&a)?, arg_str(&a, "body"));
    let (react, cid) = (arg_str(&a, "reaction"), arg_i64(&a, "comment_id"));
    if body.is_some() && cid.is_some() {
        return Err(GhError::InvalidArgs(
            "comment_id cannot be combined with body".into(),
        ));
    }
    if body.is_none() && react.is_none() {
        return Err(GhError::InvalidArgs(
            "at least one of body or reaction is required".into(),
        ));
    }
    if body == Some("") {
        return Err(GhError::InvalidArgs(
            "body cannot be empty when provided".into(),
        ));
    }
    if react == Some("") {
        return Err(GhError::InvalidArgs(
            "reaction cannot be empty when provided".into(),
        ));
    }
    let gh = ctx.gh(&a)?;
    if let Some(r) = react {
        reaction(r)?;
        // Go : sans comment_id, la réaction va sur l'issue elle-même.
        let Some(c) = cid else {
            return gh
                .rest(
                    "POST",
                    &format!("{p}/reactions"),
                    &[],
                    Some(json!({"content": r})),
                )
                .await;
        };
        if c <= 0 {
            return Err(GhError::InvalidArgs(
                "comment_id must be greater than 0".into(),
            ));
        }
        // Go (issues.go:1494) : le commentaire doit appartenir à l'issue visée.
        let com = gh
            .rest(
                "GET",
                &format!("{}/issues/comments/{c}", base(&a)?),
                &[],
                None,
            )
            .await?;
        if com.get("test_mode").is_none() || com.get("issue_url").is_some() {
            let n = com["issue_url"]
                .as_str()
                .and_then(|u| u.rsplit('/').next())
                .and_then(|s| s.parse::<i64>().ok());
            let want = arg_i64(&a, "issue_number");
            if n.is_none() || n != want {
                return Err(GhError::InvalidArgs(format!(
                    "comment_id does not belong to issue_number {}",
                    want.unwrap_or(0)
                )));
            }
        }
        return gh
            .rest(
                "POST",
                &format!("{}/issues/comments/{c}/reactions", base(&a)?),
                &[],
                Some(json!({"content": r})),
            )
            .await;
    }
    if cid.is_some() {
        return Err(GhError::InvalidArgs(
            "comment_id can only be provided when reaction is provided".into(),
        ));
    }
    gh.rest(
        "POST",
        &format!("{p}/comments"),
        &[],
        Some(json!({"body": body})),
    )
    .await
}

async fn update_issue_comment(ctx: Ctx, a: Value) -> Result<Value> {
    let c = int(&a, "comment_id")
        .ok()
        .filter(|c| *c > 0)
        .ok_or_else(|| GhError::InvalidArgs("comment_id must be greater than 0".into()))?;
    let body = req_str(&a, "body")
        .ok()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| GhError::InvalidArgs("body cannot be empty when provided".into()))?;
    ctx.gh(&a)?
        .rest(
            "PATCH",
            &format!("{}/issues/comments/{c}", base(&a)?),
            &[],
            Some(json!({"body": body})),
        )
        .await
}

async fn sub_issue_write(ctx: Ctx, a: Value) -> Result<Value> {
    let p = format!("{}/sub_issues", issue_path(&a)?);
    let id = int(&a, "sub_issue_id")?;
    let gh = ctx.gh(&a)?;
    match req_str(&a, "method")? {
        "add" => {
            let mut b = json!({"sub_issue_id": id});
            if let Some(r) = arg_bool(&a, "replace_parent") {
                b["replace_parent"] = json!(r);
            }
            gh.rest("POST", &p, &[], Some(b)).await
        }
        "remove" => {
            gh.rest(
                "DELETE",
                &format!("{}/sub_issue", issue_path(&a)?),
                &[],
                Some(json!({"sub_issue_id": id})),
            )
            .await
        }
        "reprioritize" => {
            let mut b = json!({"sub_issue_id": id});
            match (arg_i64(&a, "after_id"), arg_i64(&a, "before_id")) {
                (None, None) => {
                    return Err(GhError::InvalidArgs(
                        "either after_id or before_id must be specified".into(),
                    ))
                }
                (Some(_), Some(_)) => {
                    return Err(GhError::InvalidArgs(
                        "only one of after_id or before_id should be specified, not both".into(),
                    ))
                }
                (Some(x), None) => b["after_id"] = json!(x),
                (None, Some(x)) => b["before_id"] = json!(x),
            }
            gh.rest("PATCH", &format!("{p}/priority"), &[], Some(b))
                .await
        }
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}

// ---- issues_granular.go ----
async fn create_issue(ctx: Ctx, mut a: Value) -> Result<Value> {
    a["method"] = json!("create");
    issue_write(ctx, a).await
}
async fn update_title(ctx: Ctx, a: Value) -> Result<Value> {
    let t = req_str(&a, "title")?.to_string();
    patch(ctx, a, json!({"title": t})).await
}
async fn update_body(ctx: Ctx, a: Value) -> Result<Value> {
    let t = req_str(&a, "body")?.to_string();
    patch(ctx, a, json!({"body": t})).await
}
async fn update_assignees(ctx: Ctx, a: Value) -> Result<Value> {
    let v = arr(&a, "assignees")?;
    patch(ctx, a, json!({"assignees": v})).await
}
async fn update_labels(ctx: Ctx, a: Value) -> Result<Value> {
    let v = arr(&a, "labels")?;
    patch(ctx, a, json!({"labels": v})).await
}
async fn update_milestone(ctx: Ctx, a: Value) -> Result<Value> {
    let m = int(&a, "milestone")?;
    patch(
        ctx,
        a,
        json!({"milestone": if m > 0 { json!(m) } else { Value::Null }}),
    )
    .await
}
async fn update_type(ctx: Ctx, a: Value) -> Result<Value> {
    let t = req_str(&a, "issue_type")?.to_string();
    patch(ctx, a, json!({"type": t})).await
}
async fn update_state(ctx: Ctx, a: Value) -> Result<Value> {
    check_duplicate(&a)?;
    let state = req_str(&a, "state")?;
    let (rationale, confidence) = (
        arg_str(&a, "rationale").unwrap_or(""),
        arg_str(&a, "confidence").unwrap_or(""),
    );
    let suggest = arg_bool(&a, "is_suggestion").unwrap_or(false);
    let dup = arg_i64(&a, "duplicate_of").filter(|d| *d != 0);
    // issues_granular.go:702 : intention ou doublon -> state en objet + duplicate_issue_id (id de l'issue doublon)
    let mut b = if rationale.is_empty() && confidence.is_empty() && !suggest && dup.is_none() {
        json!({"state": state})
    } else {
        let mut s = json!({"value": state});
        if !rationale.is_empty() {
            s["rationale"] = json!(rationale);
        }
        if !confidence.is_empty() {
            s["confidence"] = json!(confidence);
        }
        if suggest {
            s["suggest"] = json!(true);
        }
        json!({"state": s})
    };
    if let Some(r) = arg_str(&a, "state_reason").filter(|r| !r.is_empty()) {
        b["state_reason"] = json!(r);
    }
    if let Some(d) = dup {
        let p = format!(
            "/repos/{}/{}/issues/{d}",
            req_str(&a, "owner")?,
            req_str(&a, "repo")?
        );
        b["duplicate_issue_id"] = ctx.gh(&a)?.rest("GET", &p, &[], None).await?["id"].clone();
    }
    patch(ctx, a, b).await
}
async fn add_sub_issue(ctx: Ctx, mut a: Value) -> Result<Value> {
    a["method"] = json!("add");
    sub_issue_write(ctx, a).await
}
async fn remove_sub_issue(ctx: Ctx, mut a: Value) -> Result<Value> {
    a["method"] = json!("remove");
    sub_issue_write(ctx, a).await
}
async fn reprioritize_sub_issue(ctx: Ctx, mut a: Value) -> Result<Value> {
    a["method"] = json!("reprioritize");
    sub_issue_write(ctx, a).await
}

async fn add_issue_reaction(ctx: Ctx, a: Value) -> Result<Value> {
    let c = req_str(&a, "content")?;
    reaction(c)?;
    ctx.gh(&a)?
        .rest(
            "POST",
            &format!("{}/reactions", issue_path(&a)?),
            &[],
            Some(json!({"content": c})),
        )
        .await
}
async fn remove_issue_reaction(ctx: Ctx, a: Value) -> Result<Value> {
    let id = int(&a, "reaction_id")?;
    ctx.gh(&a)?
        .rest(
            "DELETE",
            &format!("{}/reactions/{id}", issue_path(&a)?),
            &[],
            None,
        )
        .await?;
    Ok(json!({"deleted": true, "reaction_id": id}))
}
async fn add_comment_reaction(ctx: Ctx, a: Value) -> Result<Value> {
    let (c, id) = (req_str(&a, "content")?, int(&a, "comment_id")?);
    reaction(c)?;
    ctx.gh(&a)?
        .rest(
            "POST",
            &format!("{}/issues/comments/{id}/reactions", base(&a)?),
            &[],
            Some(json!({"content": c})),
        )
        .await
}
async fn remove_comment_reaction(ctx: Ctx, a: Value) -> Result<Value> {
    let (id, rid) = (int(&a, "comment_id")?, int(&a, "reaction_id")?);
    ctx.gh(&a)?
        .rest(
            "DELETE",
            &format!("{}/issues/comments/{id}/reactions/{rid}", base(&a)?),
            &[],
            None,
        )
        .await?;
    Ok(json!({"deleted": true, "reaction_id": rid}))
}

const SET_FIELD: &str = "mutation($input: SetIssueFieldValueInput!) { setIssueFieldValue(input: $input) { issue { id url } } }";
const ISSUE_ID: &str = "query($o:String!,$r:String!,$n:Int!){ repository(owner:$o,name:$r){ issue(number:$n){ id } } }";

async fn set_issue_fields(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = (req_str(&a, "owner")?, req_str(&a, "repo")?);
    let n = int(&a, "issue_number")?;
    let fields = arr(&a, "fields")?;
    let gh = ctx.gh(&a)?;
    let mut inputs = vec![];
    for f in fields.as_array().into_iter().flatten() {
        let id = f
            .get("field_id")
            .and_then(Value::as_str)
            .ok_or_else(|| GhError::InvalidArgs("field_id is required".into()))?;
        let mut i = json!({"fieldId": id});
        for (s, d) in [
            ("text_value", "textValue"),
            ("number_value", "numberValue"),
            ("date_value", "dateValue"),
            ("single_select_option_id", "singleSelectOptionId"),
            ("delete", "delete"),
            ("rationale", "rationale"),
            ("is_suggestion", "suggest"),
        ] {
            if let Some(v) = f.get(s) {
                i[d] = v.clone();
            }
        }
        inputs.push(i);
    }
    let d = gh
        .graphql(ISSUE_ID, json!({"o": o, "r": r, "n": n}))
        .await?;
    let issue_id = d
        .pointer("/repository/issue/id")
        .cloned()
        .unwrap_or_else(|| {
            if ctx.test_mode() {
                json!("TEST_ISSUE_ID")
            } else {
                Value::Null
            }
        });
    if issue_id.is_null() {
        return Err(GhError::Other(format!("issue {o}/{r}#{n} introuvable")));
    }
    gh.graphql(
        SET_FIELD,
        json!({"input": {"issueId": issue_id, "issueFields": inputs}}),
    )
    .await
}

// ---- issue_fields.go ----
const FIELDS_FRAG: &str = "nodes { __typename ... on IssueFieldText { id fullDatabaseId name } ... on IssueFieldNumber { id fullDatabaseId name } ... on IssueFieldDate { id fullDatabaseId name } ... on IssueFieldSingleSelect { id fullDatabaseId name options { id name } } }";
async fn list_issue_fields(ctx: Ctx, a: Value) -> Result<Value> {
    let o = req_str(&a, "owner")?;
    let gh = ctx.gh(&a)?;
    match arg_str(&a, "repo").filter(|r| !r.is_empty()) {
        Some(r) => gh.graphql(&format!("query($o:String!,$r:String!){{ repository(owner:$o,name:$r){{ issueFields(first:100){{ {FIELDS_FRAG} }} }} }}"), json!({"o": o, "r": r})).await,
        None => gh.graphql(&format!("query($l:String!){{ organization(login:$l){{ issueFields(first:100){{ {FIELDS_FRAG} }} }} }}"), json!({"l": o})).await,
    }
}

// ---- labels.go ----
// labels.go:76 et :200 : le Go lit les labels en GraphQL (id = node id, tri par nombre d'issues, curseur `after`).
fn label_min(l: &Value) -> Value {
    json!({"id": l["id"].as_str().unwrap_or(""), "name": l["name"].as_str().unwrap_or(""), "color": l["color"].as_str().unwrap_or(""), "description": l["description"].as_str().unwrap_or("")})
}
async fn get_label(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r, name) = (
        req_str(&a, "owner")?,
        req_str(&a, "repo")?,
        req_str(&a, "name")?,
    );
    let d = ctx.gh(&a)?.graphql("query($owner:String!,$repo:String!,$name:String!){ repository(owner:$owner,name:$repo){ label(name:$name){ id name color description } } }", json!({"owner": o, "repo": r, "name": name})).await?;
    // Gh::graphql rend déjà le contenu de `data` (net.rs graphql_with).
    if d.get("repository").is_none() {
        return Ok(d); // mode test : réponse simulée sans repository
    }
    let l = &d["repository"]["label"];
    if l["name"].as_str().unwrap_or("").is_empty() {
        return Err(GhError::InvalidArgs(format!(
            "label '{name}' not found in {o}/{r}"
        )));
    }
    Ok(label_min(l))
}
async fn list_label(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = (req_str(&a, "owner")?, req_str(&a, "repo")?);
    let first = arg_i64(&a, "perPage").unwrap_or(100).clamp(1, 100);
    let vars = json!({"owner": o, "repo": r, "first": first, "after": arg_str(&a, "after")});
    let d = ctx.gh(&a)?.graphql("query($owner:String!,$repo:String!,$first:Int!,$after:String){ repository(owner:$owner,name:$repo){ labels(first:$first,after:$after,orderBy:{field:ISSUE_COUNT,direction:DESC}){ nodes { id name color description } totalCount pageInfo { hasNextPage hasPreviousPage startCursor endCursor } } } }", vars).await?;
    if d.get("repository").is_none() {
        return Ok(d);
    }
    Ok(labels_page(&d))
}
/// Sortie de list_label depuis le `data` GraphQL (déjà déballé par Gh::graphql).
fn labels_page(d: &Value) -> Value {
    let ls = &d["repository"]["labels"];
    let p = &ls["pageInfo"];
    json!({
        "labels": ls["nodes"].as_array().into_iter().flatten().map(label_min).collect::<Vec<_>>(),
        "totalCount": ls["totalCount"].as_i64().unwrap_or(0),
        "pageInfo": {"hasNextPage": p["hasNextPage"].as_bool().unwrap_or(false), "hasPreviousPage": p["hasPreviousPage"].as_bool().unwrap_or(false), "startCursor": p["startCursor"].as_str().unwrap_or(""), "endCursor": p["endCursor"].as_str().unwrap_or("")},
    })
}
async fn label_write(ctx: Ctx, a: Value) -> Result<Value> {
    let (gh, b, name) = (ctx.gh(&a)?, base(&a)?, req_str(&a, "name")?);
    let enc = urlencoding::encode(name).into_owned();
    let mut body = json!({});
    for k in ["color", "description", "new_name"] {
        if let Some(v) = arg_str(&a, k) {
            body[k] = json!(v);
        }
    }
    match req_str(&a, "method")? {
        "create" => {
            body["name"] = json!(name);
            gh.rest("POST", &format!("{b}/labels"), &[], Some(body))
                .await
        }
        "update" => {
            gh.rest("PATCH", &format!("{b}/labels/{enc}"), &[], Some(body))
                .await
        }
        "delete" => {
            gh.rest("DELETE", &format!("{b}/labels/{enc}"), &[], None)
                .await?;
            Ok(json!({"deleted": name}))
        }
        m => Err(GhError::InvalidArgs(format!("unknown method: {m}"))),
    }
}

fn w(t: Tool) -> Tool {
    t.scopes(&["repo"]).perm("issues", Access::Write)
}
fn r(t: Tool) -> Tool {
    t.scopes(&["repo"]).perm("issues", Access::Read)
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    macro_rules! t {
        ($f:ident, $n:expr, $h:ident) => {
            $f(Tool::official($n, $h))
        };
    }
    vec![
        t!(w, "add_issue_comment", add_issue_comment),
        t!(r, "issue_read", issue_read),
        t!(w, "issue_write", issue_write),
        t!(r, "list_issue_types", list_issue_types),
        t!(r, "list_issues", list_issues),
        t!(r, "search_issues", search_issues),
        t!(w, "sub_issue_write", sub_issue_write),
        t!(w, "update_issue_comment", update_issue_comment),
        t!(w, "add_issue_comment_reaction", add_comment_reaction),
        t!(w, "add_issue_reaction", add_issue_reaction),
        t!(w, "add_sub_issue", add_sub_issue),
        t!(w, "create_issue", create_issue),
        t!(w, "remove_issue_comment_reaction", remove_comment_reaction),
        t!(w, "remove_issue_reaction", remove_issue_reaction),
        t!(w, "remove_sub_issue", remove_sub_issue),
        t!(w, "reprioritize_sub_issue", reprioritize_sub_issue),
        t!(w, "set_issue_fields", set_issue_fields),
        t!(w, "update_issue_assignees", update_assignees),
        t!(w, "update_issue_body", update_body),
        t!(w, "update_issue_labels", update_labels),
        t!(w, "update_issue_milestone", update_milestone),
        t!(w, "update_issue_state", update_state),
        t!(w, "update_issue_title", update_title),
        t!(w, "update_issue_type", update_type),
        t!(r, "list_issue_fields", list_issue_fields).scopes(&["repo", "read:org"]),
        t!(r, "get_label", get_label),
        t!(w, "label_write", label_write),
        t!(r, "list_label", list_label),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // GITHUB-2611100326-0n9 : Gh::graphql rend le contenu de `data`, pas l'enveloppe.
    #[test]
    fn list_label_lit_la_reponse_graphql_deballee() {
        let d = json!({"repository": {"labels": {"nodes": [{"id": "L1", "name": "bug", "color": "d73a4a", "description": "x"}], "totalCount": 1,
            "pageInfo": {"hasNextPage": false, "hasPreviousPage": false, "startCursor": "a", "endCursor": "b"}}}});
        let v = labels_page(&d);
        assert_eq!(v["totalCount"], 1);
        assert_eq!(v["labels"][0]["name"], "bug");
        assert_eq!(v["pageInfo"]["endCursor"], "b");
    }
    use ghmcp_core::{config::Config, net, Approver};
    use std::sync::Arc;

    fn args(name: &str) -> Value {
        let method = match name {
            "issue_read" => "get",
            "issue_write" => "update",
            "sub_issue_write" => "add",
            "label_write" => "create",
            _ => "",
        };
        json!({"owner":"octo","repo":"demo","issue_number":1,"comment_id":2,"reaction_id":3,"content":"+1",
        "title":"t","body":"b","assignees":["a"],"labels":["bug"],"milestone":1,"issue_type":"Bug","state":"closed",
        "sub_issue_id":9,"after_id":8,"name":"bug","color":"ffffff","query":"x","fields":[{"field_id":"F","text_value":"v"}],
        "method": method})
    }

    #[tokio::test]
    async fn all_tools_present_and_answer_in_test_mode() {
        net::init(true, Approver::Approve);
        let ctx = Ctx::new(
            Arc::new(Config::from_toml("").expect("config")),
            ghmcp_core::bus(),
        );
        let ts = tools();
        assert_eq!(ts.len(), 28);
        for t in &ts {
            let mut a = args(&t.name);
            if t.name == "add_issue_comment" {
                a.as_object_mut().unwrap().remove("comment_id");
            }
            let out = (t.handler)(ctx.clone(), a)
                .await
                .unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(out.is_object() || out.is_array(), "{}: {out}", t.name);
        }
        let h = &ts
            .iter()
            .find(|t| t.name == "add_issue_comment")
            .unwrap()
            .handler;
        assert!((h)(ctx, json!({"owner":"o","repo":"r","issue_number":1}))
            .await
            .is_err());
    }

    /// Bug r94 : field_filters accepté (Go : issues.go:3915), liste en GraphQL avec curseur et en-tête issue_fields.
    #[tokio::test]
    async fn list_issues_graphql_accepte_field_filters() {
        net::init(true, Approver::Approve);
        let ctx = Ctx::new(
            Arc::new(Config::from_toml("").expect("config")),
            ghmcp_core::bus(),
        );
        let h = &tools()
            .into_iter()
            .find(|t| t.name == "list_issues")
            .unwrap()
            .handler;
        let out = (h)(
            ctx.clone(),
            json!({"owner":"o","repo":"r","field_filters":[{"field_name":"Prio","value":"P1"}]}),
        )
        .await
        .unwrap();
        assert!(
            out["graphql"]
                .as_str()
                .unwrap()
                .contains("issueFields(first:100)"),
            "{out}"
        );
        let out = (h)(ctx.clone(), json!({"owner":"o","repo":"r","labels":["bug"],"since":"2026-01-02","after":"C1","perPage":5})).await.unwrap();
        let q = out["graphql"].as_str().unwrap();
        assert!(
            q.contains("labels:$labels")
                && q.contains("filterBy:{since:$since, issueFieldValues:$issueFieldValues}"),
            "{q}"
        );
        assert_eq!(out["variables"]["after"], "C1");
        assert_eq!(out["variables"]["first"], 5);
        assert_eq!(out["variables"]["since"], "2026-01-02T00:00:00Z");
        assert_eq!(out["variables"]["states"], json!(["OPEN", "CLOSED"]));
        let e = (h)(
            ctx.clone(),
            json!({"owner":"o","repo":"r","field_filters":[{"field_name":"Prio","value":""}]}),
        )
        .await;
        assert!(e.is_err(), "{e:?}");
        assert!((h)(ctx, json!({"owner":"o","repo":"r","page":2}))
            .await
            .is_err());
    }

    /// resolveFieldFilters (issues.go:4041) sur la forme `data` de issueFieldsRepoQuery.
    #[test]
    fn list_issues_field_filters_resolus_comme_le_go() {
        let fields = json!({"repository": {"issueFields": {"nodes": [
            {"__typename": "IssueFieldSingleSelect", "id": "F1", "name": "Priority", "options": [{"id": "o1", "name": "P1"}, {"id": "o2", "name": "P2"}]},
            {"__typename": "IssueFieldNumber", "id": "F2", "name": "Points"},
            {"__typename": "IssueFieldDate", "id": "F3", "name": "Due"},
            {"__typename": "IssueFieldText", "id": "F4", "name": "Note"}]}}});
        let raw = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        let out = resolve_field_filters(
            &raw(&[
                ("priority", "p1"),
                ("Points", "3"),
                ("due", "2026-05-01"),
                ("note", "x"),
            ]),
            &fields,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                json!({"fieldName": "Priority", "singleSelectOptionValue": "P1"}),
                json!({"fieldName": "Points", "numberValue": 3.0}),
                json!({"fieldName": "Due", "dateValue": "2026-05-01"}),
                json!({"fieldName": "Note", "textValue": "x"})
            ]
        );
        for bad in [
            ("Nope", "x"),
            ("Priority", "P9"),
            ("Due", "01/05/2026"),
            ("Points", "abc"),
        ] {
            assert!(
                resolve_field_filters(&raw(&[bad]), &fields).is_err(),
                "{bad:?}"
            );
        }
    }

    /// convertToMinimalIssuesResponse (minimal_types.go:979) sur une réponse GraphQL `data`.
    #[test]
    fn list_issues_reponse_graphql_vers_forme_go() {
        let d = json!({"repository": {"isPrivate": false, "issues": {"nodes": [{
            "number": 7, "title": "t", "body": "", "state": "OPEN", "databaseId": 1, "author": {"login": "u"},
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "labels": {"nodes": [{"name": "bug", "id": "L", "description": ""}]}, "assignees": {"nodes": []}, "comments": {"totalCount": 0},
            "issueFieldValues": {"nodes": [{"__typename": "IssueFieldSingleSelectValue", "field": {"name": "Priority"}, "value": "P1"}]}}],
            "pageInfo": {"hasNextPage": true, "hasPreviousPage": false, "startCursor": "A", "endCursor": "B"}, "totalCount": 12}}});
        assert_eq!(
            minimal_issues(&d),
            json!({"issues": [{"number": 7, "title": "t", "state": "OPEN", "user": {"login": "u"}, "assignees": [],
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z", "labels": ["bug"],
            "field_values": [{"field": "Priority", "value": "P1"}]}],
            "totalCount": 12, "pageInfo": {"hasNextPage": true, "hasPreviousPage": false, "startCursor": "A", "endCursor": "B"}})
        );
        assert!(unsupported_field_values(&GhError::GraphQl(
            json!([{"message": "Field 'issueFieldValues' doesn't exist on type 'Issue'"}])
        )));
    }

    /// Bug GITHUB-2611100103-5hc : remove doit être DELETE .../sub_issue (singulier), comme go-github.
    #[test]
    fn issue_read_enrichment_applies_go_fields() {
        // forme d'une réponse nodes(ids:) GitHub (issues.go:2402), données fictives
        let n = json!({"id": "I_x", "issueFieldValues": {"nodes": [
            {"__typename": "IssueFieldNumberValue", "field": {"name": "Points"}, "valueNumber": 3.0},
            {"__typename": "IssueFieldTextValue", "field": {"name": "Note"}, "value": "a"}]},
            "parent": {"number": 2, "title": "p", "state": "OPEN", "url": "u", "author": {"login": "x"}, "repository": {"nameWithOwner": "o/r"}},
            "closedByPullRequestsReferences": {"totalCount": 0, "nodes": []},
            "subIssuesSummary": {"total": 2, "completed": 1, "percentCompleted": 50}});
        let mut issue = json!({"node_id": "I_x"});
        apply_enrichment(&mut issue, &n);
        assert_eq!(
            issue["field_values"],
            json!([{"field": "Points", "value": "3"}, {"field": "Note", "value": "a"}])
        );
        assert_eq!(issue["has_parent"], true);
        assert_eq!(issue["parent"]["repository"], "o/r");
        assert_eq!(issue["has_children"], true);
        assert_eq!(issue["sub_issues_summary"]["percent_completed"], 50);
        assert_eq!(
            issue["closed_by_pull_requests"],
            json!({"total_count": 0, "references": []})
        );
    }

    #[tokio::test]
    async fn sub_issue_write_remove_uses_singular_sub_issue_path() {
        net::init(true, Approver::Approve);
        let ctx = Ctx::new(
            Arc::new(Config::from_toml("").expect("config")),
            ghmcp_core::bus(),
        );
        let ts = tools();
        let h = &ts
            .iter()
            .find(|t| t.name == "sub_issue_write")
            .unwrap()
            .handler;
        let call = |method: &str| json!({"owner":"octo","repo":"demo","issue_number":1,"sub_issue_id":9,"method":method,"after_id":8});
        let rem = (h)(ctx.clone(), call("remove")).await.unwrap();
        assert_eq!(rem["method"], "DELETE", "{rem}");
        assert!(
            rem["path"].as_str().unwrap().ends_with("/sub_issue"),
            "{rem}"
        );
        assert_eq!(rem["path"], "/repos/octo/demo/issues/1/sub_issue", "{rem}");
        let add = (h)(ctx.clone(), call("add")).await.unwrap();
        assert_eq!(add["method"], "POST", "{add}");
        assert_eq!(add["path"], "/repos/octo/demo/issues/1/sub_issues", "{add}");
        let re = (h)(ctx, call("reprioritize")).await.unwrap();
        assert_eq!(re["method"], "PATCH", "{re}");
        assert_eq!(
            re["path"], "/repos/octo/demo/issues/1/sub_issues/priority",
            "{re}"
        );
    }
}
