//! assign_copilot_to_issue, assign_copilot_to_issue_with_intent, request_copilot_review, ui_get.
use crate::{bad, req_num};
use ghmcp_core::{arg_bool, arg_str, req_str, Access, Ctx, Gh, Result, Tool};
use serde_json::{json, Value};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::official("assign_copilot_to_issue", assign)
            .scopes(&["repo"])
            .perm("issues", Access::Write),
        Tool::official("assign_copilot_to_issue_with_intent", assign_intent)
            .scopes(&["repo"])
            .perm("issues", Access::Write),
        Tool::official("request_copilot_review", review)
            .scopes(&["repo"])
            .perm("pull_requests", Access::Write),
        Tool::official("ui_get", ui_get)
            .scopes(&["repo"])
            .perm("metadata", Access::Read),
    ]
}

const SUGGESTED: &str = "query($owner:String!,$name:String!,$endCursor:String){repository(owner:$owner,name:$name){suggestedActors(first:100,after:$endCursor,capabilities:CAN_BE_ASSIGNED){nodes{... on Bot{id login __typename}} pageInfo{hasNextPage endCursor}}}}";
const ISSUE: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){id issue(number:$number){id assignees(first:100){nodes{id}}}}}";
const MUTATE: &str =
    "mutation($input:UpdateIssueInput!){updateIssue(input:$input){issue{id number url}}}";
const UNAVAILABLE: &str = "copilot isn't available as an assignee for this issue. Please inform the user to visit https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-cloud-agent for more information.";

/// Identifiants GraphQL de l issue, du dépôt et du bot copilot-swe-agent.
/// `Err(Ok(v))` = réponse de mode test à rendre telle quelle.
struct Ids {
    repo_id: Value,
    issue_id: Value,
    assignees: Vec<Value>,
    bot: Value,
}

async fn lookup(gh: &Gh, o: &str, r: &str, n: i64) -> std::result::Result<Ids, Result<Value>> {
    let mut cursor = Value::Null;
    let bot = loop {
        let d = gh
            .graphql(
                SUGGESTED,
                json!({"owner": o, "name": r, "endCursor": cursor}),
            )
            .await
            .map_err(Err)?;
        if d.get("test_mode").is_some() {
            return Err(Ok(d));
        }
        let sa = &d["repository"]["suggestedActors"];
        if let Some(b) = sa["nodes"]
            .as_array()
            .and_then(|a| a.iter().find(|b| b["login"] == "copilot-swe-agent"))
        {
            break b["id"].clone();
        }
        if sa["pageInfo"]["hasNextPage"] != true {
            return Err(Err(bad(UNAVAILABLE)));
        }
        cursor = sa["pageInfo"]["endCursor"].clone();
    };
    let d = gh
        .graphql(ISSUE, json!({"owner": o, "name": r, "number": n}))
        .await
        .map_err(Err)?;
    let repo = &d["repository"];
    let assignees = repo["issue"]["assignees"]["nodes"]
        .as_array()
        .map(|a| a.iter().map(|x| x["id"].clone()).collect())
        .unwrap_or_default();
    Ok(Ids {
        repo_id: repo["id"].clone(),
        issue_id: repo["issue"]["id"].clone(),
        assignees,
        bot,
    })
}

fn agent(args: &Value, repo_id: &Value) -> Value {
    let mut a = json!({"customAgent": "", "customInstructions": "", "targetRepositoryId": repo_id});
    if let Some(b) = arg_str(args, "base_ref").filter(|s| !s.is_empty()) {
        a["baseRef"] = json!(b);
    }
    if let Some(c) = arg_str(args, "custom_instructions").filter(|s| !s.is_empty()) {
        a["customInstructions"] = json!(c);
    }
    a
}

/// Lance la mutation et rend le résumé. Shortcut : pas d attente de la PR créée par Copilot (le Go
/// l interroge ; ici l appelant relit l issue).
async fn mutate(gh: &Gh, input: Value, o: &str, r: &str, extra: Value) -> Result<Value> {
    // en-tête requis par l'API d'assignation d'agent (valeur du Go, copilot.go)
    let hdr = [(
        "GraphQL-Features".to_string(),
        "issues_copilot_assignment_api_support".to_string(),
    )];
    let d = gh
        .graphql_with(MUTATE, json!({"input": input}), &hdr)
        .await?;
    let issue = &d["updateIssue"]["issue"];
    let mut out =
        json!({"issue_number": issue["number"], "issue_url": issue["url"], "owner": o, "repo": r});
    if let (Some(m), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        m.extend(e.clone());
    }
    Ok(out)
}

async fn assign(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let n = req_num(&args, "issue_number")?;
    let gh = ctx.gh(&args)?;
    let ids = match lookup(&gh, o, r, n).await {
        Ok(i) => i,
        Err(e) => return e,
    };
    let mut actors = ids.assignees.clone();
    actors.push(ids.bot.clone());
    let input = json!({"id": ids.issue_id, "assigneeIds": actors, "agentAssignment": agent(&args, &ids.repo_id)});
    mutate(
        &gh,
        input,
        o,
        r,
        json!({"message": "successfully assigned copilot to issue"}),
    )
    .await
}

async fn assign_intent(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let n = req_num(&args, "issue_number")?;
    let rationale = req_str(&args, "rationale")?.trim().to_string();
    if rationale.is_empty() {
        return Err(bad("rationale is required"));
    }
    if rationale.chars().count() > 280 {
        return Err(bad("rationale must be 280 characters or less"));
    }
    let conf = req_str(&args, "confidence")
        .map_err(|_| bad("confidence is required"))?
        .trim()
        .to_uppercase();
    if !["LOW", "MEDIUM", "HIGH"].contains(&conf.as_str()) {
        return Err(bad("confidence must be one of: LOW, MEDIUM, HIGH"));
    }
    let suggest = arg_bool(&args, "is_suggestion")
        .ok_or_else(|| bad("missing required parameter: is_suggestion"))?;
    let gh = ctx.gh(&args)?;
    let ids = match lookup(&gh, o, r, n).await {
        Ok(i) => i,
        Err(e) => return e,
    };
    let mut assignees: Vec<Value> = ids
        .assignees
        .iter()
        .filter(|a| **a != ids.bot)
        .map(|a| json!({"actorId": a}))
        .collect();
    assignees.push(
        json!({"actorId": ids.bot, "rationale": rationale, "confidence": conf, "suggest": suggest}),
    );
    let mut input = json!({"id": ids.issue_id, "assignees": assignees});
    if !suggest {
        input["agentAssignment"] = agent(&args, &ids.repo_id);
    }
    let msg = if suggest {
        "recorded pending copilot assignment suggestion"
    } else {
        "successfully assigned copilot to issue"
    };
    mutate(
        &gh,
        input,
        o,
        r,
        json!({"is_suggestion": suggest, "message": msg}),
    )
    .await
}

async fn review(ctx: Ctx, args: Value) -> Result<Value> {
    let (o, r) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
    let n = req_num(&args, "pullNumber")?;
    let body = json!({"reviewers": ["copilot-pull-request-reviewer[bot]"]});
    ctx.gh(&args)?
        .rest(
            "POST",
            &format!("/repos/{o}/{r}/pulls/{n}/requested_reviewers"),
            &[],
            Some(body),
        )
        .await?;
    Ok(json!("")) // le Go ne rend rien en cas de succès
}

/// ui_get : listes minimales pour l interface (REST ; le Go utilise GraphQL pour les labels).
async fn ui_get(ctx: Ctx, args: Value) -> Result<Value> {
    let (m, o) = (req_str(&args, "method")?, req_str(&args, "owner")?);
    let repo = || req_str(&args, "repo").map_err(|_| bad("missing required parameter: repo"));
    let gh = ctx.gh(&args)?;
    let q = [("per_page", "100".to_string())];
    let list = |key: &str, v: Value| {
        let n = v.as_array().map_or(0, Vec::len);
        json!({key: v, "totalCount": n})
    };
    match m {
        "labels" => Ok(list(
            "labels",
            gh.rest("GET", &format!("/repos/{o}/{}/labels", repo()?), &q, None)
                .await?,
        )),
        "assignees" => Ok(list(
            "assignees",
            gh.rest(
                "GET",
                &format!("/repos/{o}/{}/assignees", repo()?),
                &q,
                None,
            )
            .await?,
        )),
        "milestones" => Ok(list(
            "milestones",
            gh.rest(
                "GET",
                &format!("/repos/{o}/{}/milestones", repo()?),
                &q,
                None,
            )
            .await?,
        )),
        "branches" => Ok(list(
            "branches",
            gh.rest("GET", &format!("/repos/{o}/{}/branches", repo()?), &q, None)
                .await?,
        )),
        "issue_types" => Ok(list(
            "issueTypes",
            gh.rest("GET", &format!("/orgs/{o}/issue-types"), &[], None)
                .await?,
        )),
        "issue_fields" => Ok(list(
            "fields",
            gh.rest("GET", &format!("/orgs/{o}/issue-fields"), &[], None)
                .await?,
        )),
        "reviewers" => {
            let r = repo()?;
            let users = gh
                .rest(
                    "GET",
                    &format!("/repos/{o}/{r}/collaborators"),
                    &[("affiliation", "all".into()), q[0].clone()],
                    None,
                )
                .await?;
            let teams = gh
                .rest("GET", &format!("/repos/{o}/{r}/teams"), &q, None)
                .await?;
            Ok(json!({"users": users, "teams": teams}))
        }
        _ => Err(bad(format!("unknown method: {m}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::config::Config;

    /// Le mode test court-circuite l'outil avant la mutation : on rejoue donc `mutate` directement.
    #[tokio::test]
    async fn mutation_pose_graphql_features_du_go() {
        ghmcp_core::net::init(true, ghmcp_core::Approver::Deny);
        let gh = Gh::new(Config::from_toml("").unwrap().accounts[0].clone());
        mutate(&gh, json!({"id": "I_test"}), "o", "r", json!({}))
            .await
            .unwrap();
        let want = (
            "GraphQL-Features".to_string(),
            "issues_copilot_assignment_api_support".to_string(),
        );
        assert!(ghmcp_core::net::recorded_calls()
            .iter()
            .any(|c| c.path == "graphql" && c.headers == [want.clone()]));
    }
}
