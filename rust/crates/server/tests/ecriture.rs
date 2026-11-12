//! Écriture bornée (mode test, sans réseau) : branche et PR permises ; fusion, suppression et écriture
//! sur la branche par défaut refusées avant tout appel à GitHub ; `mode = "libre"` lève la borne.
use ghmcp::{registry, Server, Session};
use ghmcp_core::{bus, net, Config, Ctx};
use serde_json::{json, Value};
use std::sync::Arc;

async fn appel(cfg: &str, name: &str, args: Value) -> (bool, String) {
    let s = Server::new(
        Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus()),
        registry(),
    );
    let r = Session::new(s, None)
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}))
        .await
        .unwrap();
    let res = &r["result"];
    (
        res["isError"] == true,
        res["content"][0]["text"].as_str().unwrap_or("").to_string(),
    )
}

fn borne(texte: &str) -> bool {
    texte.contains("écriture bornée")
}

#[tokio::test]
async fn bornes_d_ecriture() {
    net::init(true, net::Approver::Approve);
    let o = json!({"owner": "exemple-org", "repo": "exemple-repo"});
    let avec = |extra: Value| {
        let mut a = o.clone();
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        a
    };
    let fichiers = json!([{"path": "a.txt", "content": "x"}]);

    // refusés en mode borné (défaut, config vide)
    for (outil, args) in [
        ("merge_pull_request", avec(json!({"pullNumber": 1}))),
        (
            "delete_file",
            avec(json!({"path": "a.txt", "message": "m", "branch": "feat/x"})),
        ),
        ("delete_repository", o.clone()),
        ("gh_repo_hook_delete", avec(json!({"hook_id": 1}))),
        ("update_pull_request_branch", avec(json!({"pullNumber": 1}))),
        (
            "push_files",
            avec(json!({"branch": "main", "files": fichiers, "message": "m"})),
        ),
        (
            "push_files",
            avec(json!({"branch": "refs/heads/master", "files": fichiers, "message": "m"})),
        ),
        (
            "create_or_update_file",
            avec(json!({"path": "a.txt", "content": "x", "message": "m"})),
        ),
        (
            "label_write",
            avec(json!({"method": "delete", "name": "bug"})),
        ),
        // niveau cicd / securite / admin en écriture : portes, variables, déploiements, réglages
        (
            "gh_branch_protection",
            avec(json!({"action": "set", "branch": "main"})),
        ),
        (
            "gh_ruleset",
            avec(json!({"action": "update", "ruleset_id": 1})),
        ),
        (
            "gh_repo_settings",
            avec(json!({"action": "update", "default_branch": "x"})),
        ),
        (
            "gh_actions_variable_update",
            avec(json!({"name": "PROD_HOTE", "value": "x"})),
        ),
        (
            "gh_actions_secret_set",
            avec(json!({"name": "S", "value": "x"})),
        ),
        ("gh_deployment_create", avec(json!({"ref": "main"}))),
        (
            "gh_repo_hook_create",
            avec(json!({"url": "https://hook.exemple.invalid"})),
        ),
        (
            "actions_run_trigger",
            avec(json!({"method": "run_workflow", "workflow_id": "prod.yml", "ref": "main"})),
        ),
        ("create_repository_ruleset", avec(json!({"name": "r"}))),
    ] {
        let (err, t) = appel("", outil, args.clone()).await;
        assert!(err && borne(&t), "{outil} {args} devrait être refusé : {t}");
    }

    // permis : ouvrir une branche, une PR, écrire sur une branche de travail, lire
    for (outil, args) in [
        ("create_branch", avec(json!({"branch": "feat/x"}))),
        (
            "create_pull_request",
            avec(json!({"title": "t", "head": "feat/x", "base": "main"})),
        ),
        (
            "push_files",
            avec(json!({"branch": "feat/x", "files": fichiers, "message": "m"})),
        ),
        (
            "create_or_update_file",
            avec(json!({"path": "a.txt", "content": "x", "message": "m", "branch": "feat/x"})),
        ),
        (
            "delete_pending_pull_request_review",
            avec(json!({"pullNumber": 1})),
        ),
        (
            "gh_watch",
            json!({"action": "remove", "repo": "exemple-org/exemple-repo"}),
        ),
        ("list_branches", o.clone()),
        (
            "gh_branch_protection",
            avec(json!({"action": "get", "branch": "main"})),
        ),
        ("gh_deployments_list", o.clone()),
        ("gh_actions_run_rerun_failed", avec(json!({"run_id": 1}))),
    ] {
        let (_, t) = appel("", outil, args.clone()).await;
        assert!(!borne(&t), "{outil} {args} ne devrait pas être borné : {t}");
    }

    // mode libre : la borne ne s'applique plus (la validation humaine de l'outil, elle, reste)
    let (_, t) = appel(
        "[ecriture]\nmode = \"libre\"\n",
        "merge_pull_request",
        avec(json!({"pullNumber": 1})),
    )
    .await;
    assert!(!borne(&t), "{t}");
    assert!(net::recorded_calls()
        .iter()
        .any(|c| c.method == "PUT" && c.path.ends_with("/pulls/1/merge")));
}
