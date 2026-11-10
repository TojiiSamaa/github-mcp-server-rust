//! ghmcp-tools-adp : actions, discussions, projects et variantes sous drapeau (ff).
//! Contrat : voir l en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//! Porté de github-mcp-server (actions.go, discussions.go, projects.go, issue_*.go, find_duplicate.go).
mod actions;
mod discussions;
mod ff;
mod projects;

use ghmcp_core::{GhError, Result, Tool};
use serde_json::Value;

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    [
        actions::tools(),
        discussions::tools(),
        projects::tools(),
        ff::tools(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(crate) fn bad(m: impl Into<String>) -> GhError {
    GhError::Other(m.into())
}

/// Entier venant d'un nombre JSON ou d'une chaîne numérique (resource_id est une chaîne dans le schéma).
pub(crate) fn int_arg(args: &Value, k: &str) -> Result<Option<i64>> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_i64()),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => s
            .parse()
            .map(Some)
            .map_err(|_| bad(format!("invalid {k}, must be an integer"))),
        _ => Err(bad(format!("invalid {k}"))),
    }
}

/// Paramètres de pagination REST communs (page, perPage).
pub(crate) fn page_q(args: &Value) -> Vec<(&'static str, String)> {
    let mut q = vec![];
    if let Some(p) = ghmcp_core::arg_i64(args, "page").filter(|p| *p > 0) {
        q.push(("page", p.to_string()));
    }
    if let Some(p) = ghmcp_core::arg_i64(args, "perPage").filter(|p| *p > 0) {
        q.push(("per_page", p.to_string()));
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config, Ctx};
    use serde_json::json;
    use std::sync::Arc;

    const CFG: &str = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_ADP\"\n";

    const NOMS: [&str; 17] = [
        "actions_get",
        "actions_list",
        "actions_run_trigger",
        "get_job_logs",
        "discussion_comment_write",
        "get_discussion",
        "get_discussion_comments",
        "list_discussion_categories",
        "list_discussions",
        "projects_get",
        "projects_list",
        "projects_write",
        "issue_dependency_read",
        "issue_dependency_write",
        "issue_artifact_read",
        "issue_artifact_write",
        "find_duplicate",
    ];

    #[test]
    fn noms_presents() {
        let names: Vec<String> = tools().iter().map(|t| t.name.clone()).collect();
        for n in NOMS {
            assert!(names.iter().any(|x| x == n), "manque {n}");
        }
        assert_eq!(names.len(), 17);
    }

    /// Chaque outil répond en mode test (non initialisé = fermé) avec des arguments minimaux valides.
    #[tokio::test]
    async fn chaque_outil_repond_en_mode_test() {
        let ctx = Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus());
        let ro = json!({"owner":"o","repo":"r"});
        let cas: Vec<(&str, Value)> = vec![
            (
                "actions_get",
                json!({"owner":"o","repo":"r","method":"get_workflow_run","resource_id":"12"}),
            ),
            (
                "actions_list",
                json!({"owner":"o","repo":"r","method":"list_workflows"}),
            ),
            (
                "actions_run_trigger",
                json!({"owner":"o","repo":"r","method":"rerun_workflow_run","run_id":5}),
            ),
            ("get_job_logs", json!({"owner":"o","repo":"r","job_id":9})),
            (
                "discussion_comment_write",
                json!({"method":"add","owner":"o","repo":"r","discussionNumber":1,"body":"x"}),
            ),
            (
                "get_discussion",
                json!({"owner":"o","repo":"r","discussionNumber":1}),
            ),
            (
                "get_discussion_comments",
                json!({"owner":"o","repo":"r","discussionNumber":1}),
            ),
            ("list_discussion_categories", ro.clone()),
            ("list_discussions", ro.clone()),
            (
                "projects_get",
                json!({"method":"get_project","owner":"o","owner_type":"org","project_number":1}),
            ),
            (
                "projects_list",
                json!({"method":"list_projects","owner":"o","owner_type":"org"}),
            ),
            (
                "projects_write",
                json!({"method":"delete_project_item","owner":"o","owner_type":"org","project_number":1,"item_id":2}),
            ),
            (
                "issue_dependency_read",
                json!({"method":"get_blocked_by","owner":"o","repo":"r","issue_number":1}),
            ),
            (
                "issue_dependency_write",
                json!({"method":"remove","type":"blocked_by","owner":"o","repo":"r","issue_number":1,"related_issue_number":2}),
            ),
            (
                "issue_artifact_read",
                json!({"method":"list","owner":"o","repo":"r","issue_number":1}),
            ),
            (
                "issue_artifact_write",
                json!({"method":"delete","owner":"o","repo":"r","issue_number":1,"artifact_number":3}),
            ),
            (
                "find_duplicate",
                json!({"owner":"o","repo":"r","issue_number":1}),
            ),
        ];
        let all = tools();
        for (n, a) in cas {
            let t = all.iter().find(|t| t.name == n).unwrap();
            let v = (t.handler)(ctx.clone(), a)
                .await
                .unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(!v.is_null(), "{n} vide");
        }
    }

    #[tokio::test]
    async fn parametres_manquants_refuses() {
        let ctx = Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus());
        let all = tools();
        let t = all.iter().find(|t| t.name == "get_job_logs").unwrap();
        let e = (t.handler)(ctx, json!({"owner":"o","repo":"r"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("job_id is required"));
    }
}
