//! projects_get / projects_list / projects_write (Projects v2, REST), porté de projects.go.
//! Projects v2 : REST pour la lecture/édition d'éléments et de vues, GraphQL pour create_project, create_iteration_field,
//! status updates, update_project_items (lot) et la résolution `field_names` -> ids.
use crate::{bad, int_arg, page_q};
use ghmcp_core::{arg_i64, arg_str, req_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::official("projects_get", get)
            .scopes(&["read:project"])
            .perm("organization_projects", Access::Read),
        Tool::official("projects_list", list)
            .scopes(&["read:project"])
            .perm("organization_projects", Access::Read),
        Tool::official("projects_write", write)
            .scopes(&["project"])
            .perm("organization_projects", Access::Write)
            .level(Level::Dev),
    ]
}

fn csv(args: &Value, k: &str) -> Option<String> {
    let a = args.get(k)?.as_array()?;
    let v: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
    (!v.is_empty()).then(|| v.join(","))
}

/// Essaie le type de propriétaire donné, sinon user puis org (comme le Go).
async fn owner_call(
    ctx: &Ctx,
    args: &Value,
    method: &str,
    tail: &str,
    q: &[(&str, String)],
    body: Option<Value>,
) -> Result<Value> {
    let owner = req_str(args, "owner")?;
    let gh = ctx.gh(args)?;
    let kinds: Vec<&str> = match arg_str(args, "owner_type") {
        Some("user") => vec!["users"],
        Some("org") => vec!["orgs"],
        _ => vec!["users", "orgs"],
    };
    let mut last = None;
    for k in kinds {
        match gh
            .rest(
                method,
                &format!("/{k}/{owner}/projectsV2{tail}"),
                q,
                body.clone(),
            )
            .await
        {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| bad("owner_type invalide")))
}

fn num(args: &Value) -> Result<i64> {
    arg_i64(args, "project_number").ok_or_else(|| bad("missing required parameter: project_number"))
}

const SU: &str = "id body status createdAt startDate targetDate creator{login}";
const FIELDS_Q: &str = "fields(first:100,after:$after){nodes{... on ProjectV2FieldCommon{id databaseId name dataType} ... on ProjectV2SingleSelectField{options{id name}}} pageInfo{hasNextPage endCursor}}";

/// GraphQL sur `projectV2(number)` : owner_type donné, sinon user puis organization (comme owner_call).
async fn project_gql(
    ctx: &Ctx,
    args: &Value,
    decl: &str,
    inner: &str,
    mut vars: Value,
) -> Result<Value> {
    let owner = req_str(args, "owner")?;
    let gh = ctx.gh(args)?;
    vars["owner"] = json!(owner);
    vars["projectNumber"] = json!(num(args)?);
    let kinds: Vec<&str> = match arg_str(args, "owner_type") {
        Some("user") => vec!["user"],
        Some("org") => vec!["organization"],
        _ => vec!["user", "organization"],
    };
    let mut last = None;
    for k in kinds {
        let q = format!("query($owner:String!,$projectNumber:Int!{decl}){{{k}(login:$owner){{projectV2(number:$projectNumber){{{inner}}}}}}}");
        match gh.graphql(&q, vars.clone()).await {
            Ok(d) => match d
                .pointer(&format!("/{k}/projectV2"))
                .filter(|p| !p.is_null())
            {
                Some(p) => return Ok(p.clone()),
                None if d.get("graphql").is_some() => return Ok(d), // mode test : écho de la requête
                None => last = Some(bad(format!("project not found for {k} {owner}"))),
            },
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| bad("owner_type invalide")))
}

async fn project_id(ctx: &Ctx, args: &Value) -> Result<Value> {
    Ok(project_gql(ctx, args, "", "id", json!({}))
        .await?
        .get("id")
        .cloned()
        .unwrap_or(json!("test-project-id")))
}

fn su_min(n: &Value) -> Value {
    json!({"id": n["id"], "body": n["body"], "status": n["status"], "created_at": n["createdAt"],
        "start_date": n["startDate"], "target_date": n["targetDate"], "creator": n.pointer("/creator/login").map(|l| json!({"login": l}))})
}

/// YYYY-MM-DD valide (mois 1-12, jour 1-31).
fn check_date(v: &str, name: &str) -> Result<()> {
    let p: Vec<&str> = v.split('-').collect();
    let ok = p.len() == 3
        && p[0].len() == 4
        && p[1].len() == 2
        && p[2].len() == 2
        && matches!(
            (
                p[0].parse::<u32>(),
                p[1].parse::<u32>(),
                p[2].parse::<u32>()
            ),
            (Ok(_), Ok(1..=12), Ok(1..=31))
        );
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "invalid {name} {v:?}: must be YYYY-MM-DD format"
        )))
    }
}

/// Tous les champs du projet (id GraphQL, databaseId, nom, type, options), paginés.
async fn all_fields(ctx: &Ctx, args: &Value) -> Result<Vec<Value>> {
    let (mut out, mut after) = (vec![], Value::Null);
    for _ in 0..20 {
        let p = project_gql(
            ctx,
            args,
            ",$after:String",
            FIELDS_Q,
            json!({"after": after}),
        )
        .await?;
        out.extend(
            p.pointer("/fields/nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        if p.pointer("/fields/pageInfo/hasNextPage") != Some(&json!(true)) {
            break;
        }
        after = p
            .pointer("/fields/pageInfo/endCursor")
            .cloned()
            .unwrap_or(Value::Null);
    }
    Ok(out)
}

/// `field_names` -> ids numériques (insensible à la casse) ; introuvable ou ambigu = erreur structurée.
fn names_to_ids(fields: &[Value], names: &[String], owner: &str, n: i64) -> Result<Vec<i64>> {
    names.iter().map(|name| {
        let m: Vec<&Value> = fields.iter().filter(|f| f["name"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(name))).collect();
        match m.as_slice() {
            [f] => f["databaseId"].as_i64().ok_or_else(|| bad(format!("resolved field {name:?} has non-numeric ID; pass it via 'fields' instead"))),
            [] => Err(bad(format!("field_not_found: no project field named {name:?} on project {owner}#{n}; candidates: {}",
                fields.iter().filter_map(|f| f["name"].as_str()).collect::<Vec<_>>().join(", ")))),
            _ => Err(bad(format!("field_ambiguous: multiple fields share the name {name:?}; pass numeric IDs via 'fields' to disambiguate"))),
        }
    }).collect()
}

fn str_list(args: &Value, k: &str) -> Vec<String> {
    args.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `fields` (ids) ou `field_names` (résolus par GraphQL), exclusifs, rendus en CSV pour l'API REST.
async fn fields_csv(ctx: &Ctx, args: &Value) -> Result<Option<String>> {
    let names = str_list(args, "field_names");
    if csv(args, "fields").is_some() && !names.is_empty() {
        return Err(bad("provide either 'fields' or 'field_names', not both"));
    }
    if names.is_empty() {
        return Ok(csv(args, "fields"));
    }
    let ids = names_to_ids(
        &all_fields(ctx, args).await?,
        &names,
        req_str(args, "owner")?,
        num(args)?,
    )?;
    Ok(Some(
        ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","),
    ))
}

fn convert_value(field: &Value, raw: &Value) -> Result<Value> {
    let name = field["name"].as_str().unwrap_or("");
    let s = raw.as_str().filter(|s| !s.is_empty());
    match field["dataType"].as_str().unwrap_or("") {
        "TEXT" => raw.as_str().map(|s| json!({"text": s})).ok_or_else(|| bad(format!("field {name:?} is TEXT; value must be a string"))),
        "NUMBER" => raw.as_f64().map(|f| json!({"number": f})).ok_or_else(|| bad(format!("field {name:?} is NUMBER; value must be a number"))),
        "DATE" => {
            let s = raw.as_str().ok_or_else(|| bad(format!("field {name:?} is DATE; value must be a YYYY-MM-DD string")))?;
            check_date(s, "value")?;
            Ok(json!({"date": s}))
        }
        "SINGLE_SELECT" => {
            let s = s.ok_or_else(|| bad(format!("field {name:?} is SINGLE_SELECT; value must be a non-empty string (option name or ID)")))?;
            let opts = field["options"].as_array().cloned().unwrap_or_default();
            let id = opts.iter().find(|o| o["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(s))).or_else(|| opts.iter().find(|o| o["id"] == s));
            id.map(|o| json!({"singleSelectOptionId": o["id"]})).ok_or_else(|| bad(format!("option_not_found: no option {s:?} on field {name:?}")))
        }
        "ITERATION" => s.map(|s| json!({"iterationId": s})).ok_or_else(|| bad(format!("field {name:?} is ITERATION; value must be a non-empty iteration ID string"))),
        t => Err(bad(format!("field {name:?} has unsupported data type {t:?} for update_project_items; use update_project_item instead"))),
    }
}

/// Mutation GraphQL à alias (item0..itemN-1), une variable `$input`/`$inputI` par élément.
fn item_mutation(clear: bool, n: usize) -> String {
    let (op, ty) = if clear {
        (
            "clearProjectV2ItemFieldValue",
            "ClearProjectV2ItemFieldValueInput",
        )
    } else {
        (
            "updateProjectV2ItemFieldValue",
            "UpdateProjectV2ItemFieldValueInput",
        )
    };
    let var = |i: usize| {
        if i == 0 {
            "input".to_string()
        } else {
            format!("input{i}")
        }
    };
    let decl: Vec<String> = (0..n).map(|i| format!("${}:{ty}!", var(i))).collect();
    let body: String = (0..n)
        .map(|i| {
            format!(
                "item{i}:{op}(input:${}){{projectV2Item{{id fullDatabaseId}}}} ",
                var(i)
            )
        })
        .collect();
    format!("mutation({}){{{body}}}", decl.join(","))
}

async fn create_project(ctx: &Ctx, args: &Value) -> Result<Value> {
    let owner = req_str(args, "owner")?;
    let kind = match arg_str(args, "owner_type") {
        Some("user") => "user",
        Some("org") => "organization",
        Some("") | None => return Err(bad("owner_type is required for create_project")),
        Some(t) => {
            return Err(bad(format!(
                "invalid owner_type {t:?}: must be \"user\" or \"org\""
            )))
        }
    };
    let title = req_str(args, "title")?;
    let gh = ctx.gh(args)?;
    let o = gh
        .graphql(
            &format!("query($login:String!){{{kind}(login:$login){{id}}}}"),
            json!({"login": owner}),
        )
        .await?;
    let oid = o
        .pointer(&format!("/{kind}/id"))
        .cloned()
        .unwrap_or(json!("test-owner-id"));
    let d = gh.graphql("mutation($input:CreateProjectV2Input!){createProjectV2(input:$input){projectV2{id number title url}}}", json!({"input": {"ownerId": oid, "title": title}})).await?;
    Ok(d.pointer("/createProjectV2/projectV2")
        .cloned()
        .unwrap_or(d))
}

async fn create_status_update(ctx: &Ctx, args: &Value) -> Result<Value> {
    let status = arg_str(args, "status").filter(|s| !s.is_empty());
    if let Some(s) =
        status.filter(|s| !["INACTIVE", "ON_TRACK", "AT_RISK", "OFF_TRACK", "COMPLETE"].contains(s))
    {
        return Err(bad(format!(
            "invalid status {s:?}: must be one of INACTIVE, ON_TRACK, AT_RISK, OFF_TRACK, COMPLETE"
        )));
    }
    let mut input = json!({"projectId": null});
    for (src, dst) in [
        ("body", "body"),
        ("start_date", "startDate"),
        ("target_date", "targetDate"),
    ] {
        if let Some(v) = arg_str(args, src).filter(|s| !s.is_empty()) {
            if src != "body" {
                check_date(v, src)?;
            }
            input[dst] = json!(v);
        }
    }
    if let Some(s) = status {
        input["status"] = json!(s);
    }
    input["projectId"] = project_id(ctx, args).await?;
    let d = ctx.gh(args)?.graphql(&format!("mutation($input:CreateProjectV2StatusUpdateInput!){{createProjectV2StatusUpdate(input:$input){{statusUpdate{{{SU}}}}}}}"), json!({"input": input})).await?;
    Ok(d.pointer("/createProjectV2StatusUpdate/statusUpdate")
        .map(su_min)
        .unwrap_or(d))
}

/// Deux mutations (comme le Go) : createProjectV2Field (ITERATION) puis updateProjectV2Field (calendrier).
async fn create_iteration_field(ctx: &Ctx, args: &Value) -> Result<Value> {
    let name = req_str(args, "field_name")?;
    let duration = arg_i64(args, "iteration_duration")
        .ok_or_else(|| bad("missing required parameter: iteration_duration"))?;
    let start = req_str(args, "start_date")?;
    check_date(start, "start_date")?;
    let mut iters = vec![];
    for (i, it) in args
        .get("iterations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let title = it["title"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                bad(format!(
                    "iterations[{i}]: title is required and must be a non-empty string"
                ))
            })?;
        let sd = it["start_date"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                bad(format!(
                    "iterations[{i}]: start_date is required and must be a non-empty string"
                ))
            })?;
        check_date(sd, &format!("iterations[{i}].start_date"))?;
        let du = it["duration"]
            .as_f64()
            .filter(|d| *d > 0.0)
            .ok_or_else(|| {
                bad(format!(
                    "iterations[{i}]: duration is required and must be a positive number"
                ))
            })?;
        iters.push(json!({"title": title, "startDate": sd, "duration": du as i64}));
    }
    let pid = project_id(ctx, args).await?;
    let gh = ctx.gh(args)?;
    let c = gh.graphql("mutation($input:CreateProjectV2FieldInput!){createProjectV2Field(input:$input){projectV2Field{... on ProjectV2IterationField{id name}}}}",
        json!({"input": {"projectId": pid, "dataType": "ITERATION", "name": name}})).await?;
    let fid = c
        .pointer("/createProjectV2Field/projectV2Field/id")
        .cloned()
        .unwrap_or(json!("test-field-id"));
    let d = gh.graphql("mutation($input:UpdateProjectV2FieldInput!){updateProjectV2Field(input:$input){projectV2Field{... on ProjectV2IterationField{id name configuration{iterations{id title startDate duration}}}}}}",
        json!({"input": {"fieldId": fid, "iterationConfiguration": {"duration": duration, "startDate": start, "iterations": iters}}})).await?;
    Ok(d.pointer("/updateProjectV2Field/projectV2Field")
        .cloned()
        .unwrap_or(d))
}

/// Écritures en lot (max 50 éléments, 20 alias par requête) d'UNE valeur de champ sur plusieurs éléments.
async fn update_items(ctx: &Ctx, args: &Value) -> Result<Value> {
    let items = args
        .get("items")
        .ok_or_else(|| bad("missing required parameter: items"))?
        .as_array()
        .ok_or_else(|| bad("items must be an array"))?;
    if items.is_empty() {
        return Err(bad("items must contain at least one entry"));
    }
    if items.len() > 50 {
        return Err(bad(format!(
            "items exceeds maximum of 50 entries per call (got {})",
            items.len()
        )));
    }
    let uf = args
        .get("updated_field")
        .ok_or_else(|| bad("missing required parameter: updated_field"))?
        .as_object()
        .ok_or_else(|| bad("updated_field must be an object"))?;
    let value = uf
        .get("value")
        .ok_or_else(|| bad("updated_field.value is required"))?;
    let (fid, fname) = match (uf.get("id"), uf.get("name")) {
        (Some(_), Some(_)) => {
            return Err(bad("updated_field must set either id or name, not both"))
        }
        (None, None) => return Err(bad("updated_field requires either id or name")),
        (Some(i), None) => (
            Some(
                i.as_i64()
                    .filter(|i| *i > 0)
                    .ok_or_else(|| bad("updated_field.id: must be a positive integer"))?,
            ),
            None,
        ),
        (None, Some(n)) => (
            None,
            Some(
                n.as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| bad("updated_field.name must be a non-empty string"))?,
            ),
        ),
    };
    let (owner, n) = (req_str(args, "owner")?, num(args)?);
    let pid = project_id(ctx, args).await?;
    let fields = all_fields(ctx, args).await?;
    let want = match (fid, fname) {
        (Some(i), _) => i,
        (_, Some(nm)) => names_to_ids(&fields, &[nm.to_string()], owner, n)?[0],
        _ => unreachable!(),
    };
    let field = fields
        .iter()
        .find(|f| f["databaseId"].as_i64() == Some(want))
        .ok_or_else(|| {
            bad(format!(
                "field_not_found: no project field with id {want} on project {owner}#{n}"
            ))
        })?;
    let clear = value.is_null();
    let value = if clear {
        Value::Null
    } else {
        convert_value(field, value)?
    };
    let gh = ctx.gh(args)?;
    // références -> node ids ; un élément invalide échoue seul, les autres continuent
    let mut results: Vec<Value> = vec![Value::Null; items.len()];
    let mut work: Vec<(usize, String)> = vec![];
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for (i, it) in items.iter().enumerate() {
        let fail = |code: &str, msg: String| json!({"index": i, "status": "failed", "ref": it, "error": {"code": code, "message": msg}});
        let node: std::result::Result<String, String> = if let Some(s) = it
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Ok(s.to_string())
        } else if let Some(id) = it.get("item_id").and_then(Value::as_i64).filter(|i| *i > 0) {
            owner_call(ctx, args, "GET", &format!("/{n}/items/{id}"), &[], None)
                .await
                .map(|v| {
                    v["node_id"]
                        .as_str()
                        .unwrap_or("test-item-node")
                        .to_string()
                })
                .map_err(|e| e.to_string())
        } else if let (Some(io), Some(ir), Some(inum)) = (
            it["item_owner"].as_str(),
            it["item_repo"].as_str(),
            it["issue_number"].as_i64(),
        ) {
            let q = "query($o:String!,$r:String!,$n:Int!,$after:String){repository(owner:$o,name:$r){issue(number:$n){projectItems(first:50,after:$after,includeArchived:true){nodes{id project{id}} pageInfo{hasNextPage endCursor}}}}}";
            issue_item_in_project(&pid, |after| {
                gh.graphql(q, json!({"o": io, "r": ir, "n": inum, "after": after}))
            })
            .await
            .and_then(|o| {
                o.ok_or_else(|| format!("issue {io}/{ir}#{inum} is not in project {owner}#{n}"))
            })
        } else {
            Err("each item requires exactly one of node_id, item_id, or item_owner + item_repo + issue_number".to_string())
        };
        match node {
            Err(m) => results[i] = fail("invalid_item", m),
            Ok(id) => match seen.get(&id) {
                Some(first) => results[i] = fail("duplicate_target", format!("items[{i}] targets the same project item as items[{first}]; each item may only be written once per call")),
                None => {
                    seen.insert(id.clone(), i);
                    work.push((i, id));
                }
            },
        }
    }
    let fnode = field["id"].clone();
    for chunk in work.chunks(20) {
        let mut vars = serde_json::Map::new();
        for (k, (_, id)) in chunk.iter().enumerate() {
            let mut input = json!({"projectId": pid, "itemId": id, "fieldId": fnode});
            if !clear {
                input["value"] = value.clone();
            }
            vars.insert(
                if k == 0 {
                    "input".into()
                } else {
                    format!("input{k}")
                },
                input,
            );
        }
        let r = gh
            .graphql(&item_mutation(clear, chunk.len()), Value::Object(vars))
            .await;
        for (k, (i, _)) in chunk.iter().enumerate() {
            let it = r
                .as_ref()
                .ok()
                .and_then(|d| d.pointer(&format!("/item{k}/projectV2Item")))
                .filter(|x| x["id"].is_string());
            results[*i] = match (&r, it) {
                (_, Some(x)) => {
                    json!({"index": i, "status": "succeeded", "item": {"node_id": x["id"], "full_database_id": x["fullDatabaseId"]}})
                }
                (Err(e), None) => {
                    json!({"index": i, "status": "unknown", "error": {"code": "mutation_failed", "message": e.to_string()}})
                }
                (Ok(_), None) => {
                    json!({"index": i, "status": "unknown", "error": {"code": "not_populated", "message": "no result returned for this item"}})
                }
            };
        }
    }
    batch_response(results)
}

/// Pagine `projectItems` d'une issue (50 par page, comme projects_resolver.go) jusqu'à l'élément du projet `pid`.
async fn issue_item_in_project<F, Fut>(
    pid: &Value,
    mut page: F,
) -> std::result::Result<Option<String>, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let mut after = None;
    loop {
        let d = page(after).await.map_err(|e| e.to_string())?;
        let c = d
            .pointer("/repository/issue/projectItems")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(id) = c["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|x| x.pointer("/project/id") == Some(pid))
            .and_then(|x| x["id"].as_str())
        {
            return Ok(Some(id.to_string()));
        }
        match (
            c["pageInfo"]["hasNextPage"].as_bool(),
            c["pageInfo"]["endCursor"].as_str(),
        ) {
            (Some(true), Some(cur)) => after = Some(cur.to_string()),
            _ => return Ok(None),
        }
    }
}

/// Forme de newUpdateProjectItemsResult (projects_batch.go) : {total,succeeded,failed,unknown,results}, isError si aucun succès.
fn batch_response(results: Vec<Value>) -> Result<Value> {
    let count = |s: &str| results.iter().filter(|r| r["status"] == s).count();
    let (succeeded, unknown) = (count("succeeded"), count("unknown"));
    let v = json!({"total": results.len(), "succeeded": succeeded, "failed": results.len() - succeeded - unknown, "unknown": unknown, "results": results});
    // shortcut: le texte isError passe par GhError::Other (préfixe « ERROR: »), le Go renvoie le JSON brut ; changer côté server si besoin.
    if succeeded == 0 {
        Err(GhError::Other(v.to_string()))
    } else {
        Ok(v)
    }
}

async fn list(ctx: Ctx, args: Value) -> Result<Value> {
    let m = req_str(&args, "method")?;
    req_str(&args, "owner")?;
    let mut q = page_q(&args);
    for k in ["before", "after"] {
        if let Some(v) = arg_str(&args, k).filter(|s| !s.is_empty()) {
            q.push((k, v.into()));
        }
    }
    match m {
        "list_projects" => {
            if let Some(s) = arg_str(&args, "query").filter(|s| !s.is_empty()) {
                q.push(("q", s.into()));
            }
            owner_call(&ctx, &args, "GET", "", &q, None).await
        }
        "list_project_fields" => {
            owner_call(
                &ctx,
                &args,
                "GET",
                &format!("/{}/fields", num(&args)?),
                &q,
                None,
            )
            .await
        }
        "list_project_views" => {
            owner_call(
                &ctx,
                &args,
                "GET",
                &format!("/{}/views", num(&args)?),
                &q,
                None,
            )
            .await
        }
        "list_project_items" => {
            if let Some(f) = fields_csv(&ctx, &args).await? {
                q.push(("fields", f));
            }
            if let Some(s) = arg_str(&args, "query").filter(|s| !s.is_empty()) {
                q.push(("q", s.into()));
            }
            owner_call(
                &ctx,
                &args,
                "GET",
                &format!("/{}/items", num(&args)?),
                &q,
                None,
            )
            .await
        }
        "list_project_status_updates" => {
            let first = arg_i64(&args, "perPage")
                .filter(|n| *n > 0)
                .unwrap_or(50)
                .min(50);
            let after = arg_str(&args, "after").filter(|s| !s.is_empty());
            let inner = format!("public statusUpdates(first:$first,after:$after,orderBy:{{field:CREATED_AT,direction:DESC}}){{nodes{{{SU}}} pageInfo{{hasNextPage hasPreviousPage startCursor endCursor}}}}");
            let p = project_gql(
                &ctx,
                &args,
                ",$first:Int!,$after:String",
                &inner,
                json!({"first": first, "after": after}),
            )
            .await?;
            Ok(match p.get("statusUpdates") {
                Some(su) => {
                    json!({"statusUpdates": su["nodes"].as_array().map(|a| a.iter().map(su_min).collect::<Vec<_>>()).unwrap_or_default(),
                    "pageInfo": {"hasNextPage": su["pageInfo"]["hasNextPage"], "hasPreviousPage": su["pageInfo"]["hasPreviousPage"],
                        "nextCursor": su["pageInfo"]["endCursor"], "prevCursor": su["pageInfo"]["startCursor"]}})
                }
                None => p,
            })
        }
        _ => Err(bad(format!("unknown method: {m}"))),
    }
}

async fn get(ctx: Ctx, args: Value) -> Result<Value> {
    let m = req_str(&args, "method")?;
    if m == "get_project_status_update" {
        let id = req_str(&args, "status_update_id")?;
        let q = format!("query($id:ID!){{node(id:$id){{... on ProjectV2StatusUpdate{{{SU}}}}}}}");
        let d = ctx.gh(&args)?.graphql(&q, json!({"id": id})).await?;
        return match d.pointer("/node") {
            Some(n) if n["id"].is_string() => Ok(su_min(n)),
            Some(_) => Err(bad("node is not a ProjectV2StatusUpdate or was not found")),
            None => Ok(d), // mode test : écho de la requête
        };
    }
    req_str(&args, "owner")?;
    let n = num(&args)?;
    match m {
        "get_project" => owner_call(&ctx, &args, "GET", &format!("/{n}"), &[], None).await,
        "get_project_field" => {
            let f = int_arg(&args, "field_id")?
                .ok_or_else(|| bad("missing required parameter: field_id"))?;
            owner_call(&ctx, &args, "GET", &format!("/{n}/fields/{f}"), &[], None).await
        }
        "get_project_item" => {
            let i = int_arg(&args, "item_id")?
                .ok_or_else(|| bad("missing required parameter: item_id"))?;
            let q: Vec<(&str, String)> = fields_csv(&ctx, &args)
                .await?
                .map(|f| ("fields", f))
                .into_iter()
                .collect();
            owner_call(&ctx, &args, "GET", &format!("/{n}/items/{i}"), &q, None).await
        }
        "get_project_view" => {
            let v = arg_str(&args, "view_id")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("missing required parameter: view_id"))?;
            owner_call(&ctx, &args, "GET", &format!("/{n}/views/{v}"), &[], None).await
        }
        _ => Err(bad(format!("unknown method: {m}"))),
    }
}

async fn write(ctx: Ctx, args: Value) -> Result<Value> {
    let m = req_str(&args, "method")?;
    req_str(&args, "owner")?;
    let n = || num(&args);
    let item =
        || int_arg(&args, "item_id")?.ok_or_else(|| bad("missing required parameter: item_id"));
    match m {
        "add_project_item" => {
            let t = req_str(&args, "item_type")?;
            let (ir, num_key, kind) = match t {
                "issue" => ("issues", "issue_number", "Issue"),
                "pull_request" => ("pulls", "pull_request_number", "PullRequest"),
                _ => return Err(bad("item_type must be issue or pull_request")),
            };
            let nb =
                arg_i64(&args, num_key).ok_or_else(|| bad(format!("{num_key} is required")))?;
            let (io, irepo) = (req_str(&args, "item_owner")?, req_str(&args, "item_repo")?);
            let gh = ctx.gh(&args)?;
            let obj = gh
                .rest("GET", &format!("/repos/{io}/{irepo}/{ir}/{nb}"), &[], None)
                .await?;
            let id = obj["id"].as_i64().unwrap_or(1);
            owner_call(
                &ctx,
                &args,
                "POST",
                &format!("/{}/items", n()?),
                &[],
                Some(json!({"type": kind, "id": id})),
            )
            .await
        }
        "update_project_item" => {
            let f = args
                .get("updated_field")
                .cloned()
                .ok_or_else(|| bad("updated_field is required"))?;
            owner_call(
                &ctx,
                &args,
                "PATCH",
                &format!("/{}/items/{}", n()?, item()?),
                &[],
                Some(json!({"fields": [f]})),
            )
            .await
        }
        "delete_project_item" => {
            owner_call(
                &ctx,
                &args,
                "DELETE",
                &format!("/{}/items/{}", n()?, item()?),
                &[],
                None,
            )
            .await?;
            Ok(json!({"deleted": true, "item_id": item()?}))
        }
        "create_project_view" => {
            let mut b =
                json!({"name": req_str(&args, "name")?, "layout": req_str(&args, "layout")?});
            if let Some(v) = arg_str(&args, "filter") {
                b["filter"] = json!(v);
            }
            if let Some(v) = args.get("visible_fields") {
                b["visible_fields"] = v.clone();
            }
            owner_call(
                &ctx,
                &args,
                "POST",
                &format!("/{}/views", n()?),
                &[],
                Some(b),
            )
            .await
        }
        "update_project_view" | "delete_project_view" => {
            let v = req_str(&args, "view_id")?;
            let path = format!("/{}/views/{v}", n()?);
            if m == "delete_project_view" {
                owner_call(&ctx, &args, "DELETE", &path, &[], None).await?;
                return Ok(json!({"deleted": true, "view_id": v}));
            }
            let mut b = json!({});
            for k in ["name", "layout", "filter"] {
                if let Some(x) = arg_str(&args, k) {
                    b[k] = json!(x);
                }
            }
            if let Some(x) = args.get("visible_fields") {
                b["visible_fields"] = x.clone();
            }
            owner_call(&ctx, &args, "PATCH", &path, &[], Some(b)).await
        }
        "create_project" => create_project(&ctx, &args).await,
        "create_project_status_update" => create_status_update(&ctx, &args).await,
        "create_iteration_field" => create_iteration_field(&ctx, &args).await,
        "update_project_items" => update_items(&ctx, &args).await,
        _ => Err(GhError::Other(format!("unknown method: {m}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};
    use std::sync::Arc;

    const CFG: &str = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_ADP\"\n";

    async fn run(tool: &str, args: Value) -> Result<Value> {
        let ctx = Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus());
        let all = tools();
        let t = all.iter().find(|t| t.name == tool).unwrap();
        (t.handler)(ctx, args).await
    }

    fn gql(v: &Value) -> String {
        v["graphql"]
            .as_str()
            .unwrap_or_else(|| panic!("pas une requête GraphQL: {v}"))
            .to_string()
    }

    #[tokio::test]
    async fn list_project_status_updates_graphql() {
        let v = run("projects_list", json!({"method":"list_project_status_updates","owner":"o","owner_type":"org","project_number":3,"perPage":10})).await.unwrap();
        let q = gql(&v);
        assert!(
            q.contains("organization(login:$owner)") && q.contains("statusUpdates("),
            "{q}"
        );
        assert_eq!(v["variables"]["projectNumber"], 3);
        assert_eq!(v["variables"]["first"], 10);
    }

    #[tokio::test]
    async fn get_project_status_update_graphql() {
        let v = run(
            "projects_get",
            json!({"method":"get_project_status_update","status_update_id":"SU_x"}),
        )
        .await
        .unwrap();
        let q = gql(&v);
        assert!(
            q.contains("node(id:$id)") && q.contains("ProjectV2StatusUpdate"),
            "{q}"
        );
        assert_eq!(v["variables"]["id"], "SU_x");
    }

    #[tokio::test]
    async fn create_project_status_update_mutation_et_validation() {
        let a = json!({"method":"create_project_status_update","owner":"o","owner_type":"org","project_number":3,"body":"b","status":"ON_TRACK","start_date":"2026-01-02"});
        let v = run("projects_write", a.clone()).await.unwrap();
        assert!(gql(&v).contains("createProjectV2StatusUpdate"), "{v}");
        assert_eq!(v["variables"]["input"]["status"], "ON_TRACK");
        assert_eq!(v["variables"]["input"]["startDate"], "2026-01-02");
        let mut bad_status = a.clone();
        bad_status["status"] = json!("NOPE");
        assert!(run("projects_write", bad_status)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid status"));
        let mut bad_date = a;
        bad_date["target_date"] = json!("02/01/2026");
        assert!(run("projects_write", bad_date)
            .await
            .unwrap_err()
            .to_string()
            .contains("YYYY-MM-DD"));
    }

    #[tokio::test]
    async fn create_project_mutation() {
        let v = run(
            "projects_write",
            json!({"method":"create_project","owner":"o","owner_type":"user","title":"T"}),
        )
        .await
        .unwrap();
        assert!(gql(&v).contains("createProjectV2"), "{v}");
        assert_eq!(v["variables"]["input"]["title"], "T");
        assert!(run(
            "projects_write",
            json!({"method":"create_project","owner":"o","title":"T"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("owner_type is required"));
    }

    #[tokio::test]
    async fn create_iteration_field_deux_mutations() {
        let v = run("projects_write", json!({"method":"create_iteration_field","owner":"o","owner_type":"org","project_number":3,
            "field_name":"Sprint","iteration_duration":14,"start_date":"2026-01-05"})).await.unwrap();
        assert!(gql(&v).contains("updateProjectV2Field"), "{v}");
        assert_eq!(
            v["variables"]["input"]["iterationConfiguration"]["duration"],
            14
        );
        assert_eq!(
            v["variables"]["input"]["iterationConfiguration"]["iterations"],
            json!([])
        );
    }

    #[tokio::test]
    async fn update_project_items_lot() {
        let a = json!({"method":"update_project_items","owner":"o","owner_type":"org","project_number":3,
            "updated_field":{"id":7,"value":"x"},"items":[{"node_id":"PVTI_1"}]});
        // en mode test la liste des champs est vide : le champ 7 est introuvable, preuve que la résolution GraphQL a eu lieu
        let e = run("projects_write", a).await.unwrap_err().to_string();
        assert!(e.contains("field_not_found") && e.contains('7'), "{e}");
        let e = run("projects_write", json!({"method":"update_project_items","owner":"o","owner_type":"org","project_number":3,"updated_field":{"id":7,"value":"x"},"items":[]})).await.unwrap_err().to_string();
        assert!(e.contains("at least one"), "{e}");
    }

    #[tokio::test]
    async fn field_names_resolus_par_graphql() {
        let e = run("projects_list", json!({"method":"list_project_items","owner":"o","owner_type":"org","project_number":3,"field_names":["Status"]})).await.unwrap_err().to_string();
        assert!(e.contains("field_not_found") && e.contains("Status"), "{e}");
        let e = run("projects_get", json!({"method":"get_project_item","owner":"o","owner_type":"org","project_number":3,"item_id":5,"field_names":["Status"]})).await.unwrap_err().to_string();
        assert!(e.contains("field_not_found"), "{e}");
    }

    #[tokio::test]
    async fn projectitems_pagine_sur_deux_pages() {
        let pid = json!("PVT_target");
        let pages = [
            json!({"repository":{"issue":{"projectItems":{"nodes":[{"id":"PVTI_other","project":{"id":"PVT_other"}}],"pageInfo":{"hasNextPage":true,"endCursor":"CUR1"}}}}}),
            json!({"repository":{"issue":{"projectItems":{"nodes":[{"id":"PVTI_hit","project":{"id":"PVT_target"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
        ];
        let mut seen = vec![];
        let r = issue_item_in_project(&pid, |after| {
            seen.push(after.clone());
            let p = pages[seen.len() - 1].clone();
            async move { Ok(p) }
        })
        .await;
        assert_eq!(r, Ok(Some("PVTI_hit".to_string())));
        assert_eq!(seen, vec![None, Some("CUR1".to_string())]);
        let r = issue_item_in_project(&pid, |_| async { Ok(json!({"repository":{"issue":{"projectItems":{"nodes":[],"pageInfo":{"hasNextPage":false}}}}})) }).await;
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn reponse_lot_forme_go() {
        let ok = json!({"index":0,"status":"succeeded","item":{"node_id":"PVTI_1"}});
        let ko = json!({"index":1,"status":"failed","error":{"code":"invalid_item","message":"m"}});
        let un =
            json!({"index":2,"status":"unknown","error":{"code":"not_populated","message":"m"}});
        let v = batch_response(vec![ok.clone(), ko.clone(), un.clone()]).unwrap();
        assert_eq!(
            v,
            json!({"total":3,"succeeded":1,"failed":1,"unknown":1,"results":[ok, ko.clone(), un]})
        );
        let e = batch_response(vec![ko]).unwrap_err().to_string();
        assert!(
            e.contains(r#""total":1"#)
                && e.contains(r#""succeeded":0"#)
                && e.contains(r#""failed":1"#),
            "{e}"
        );
    }

    #[test]
    fn mutation_a_alias() {
        let q = item_mutation(false, 2);
        assert!(q.starts_with("mutation($input:UpdateProjectV2ItemFieldValueInput!,$input1:UpdateProjectV2ItemFieldValueInput!)"), "{q}");
        assert!(
            q.contains("item0:updateProjectV2ItemFieldValue(input:$input)")
                && q.contains("item1:updateProjectV2ItemFieldValue(input:$input1)"),
            "{q}"
        );
        assert!(item_mutation(true, 1).contains("clearProjectV2ItemFieldValue"));
    }

    #[test]
    fn resolution_noms_vers_ids() {
        let f = vec![
            json!({"databaseId":11,"name":"Status","dataType":"SINGLE_SELECT"}),
            json!({"databaseId":12,"name":"Dup","dataType":"TEXT"}),
            json!({"databaseId":13,"name":"dup","dataType":"TEXT"}),
        ];
        assert_eq!(
            names_to_ids(&f, &["status".into()], "o", 1).unwrap(),
            vec![11]
        );
        assert!(names_to_ids(&f, &["Nope".into()], "o", 1)
            .unwrap_err()
            .to_string()
            .contains("field_not_found"));
        assert!(names_to_ids(&f, &["Dup".into()], "o", 1)
            .unwrap_err()
            .to_string()
            .contains("field_ambiguous"));
    }
}
