//! Mode test (défaut, aucun egress) : l'outil est publié et répond, le relevé ne plante pas.
use ghmcp_activity::Store;
use ghmcp_automation::{ci_summary, poll_once, spawn, tools};
use ghmcp_core::{bus, Config, Ctx};
use serde_json::json;
use std::sync::Arc;

const CFG: &str = "default_account = \"a\"\n[[accounts]]\nname = \"a\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_UNUSED\"\n";

#[tokio::test]
async fn gh_pr_dashboard_publie_et_repond_en_mode_test() {
    let ts = tools();
    let t = ts
        .iter()
        .find(|t| t.name == "gh_pr_dashboard")
        .expect("outil absent de tools()");
    assert!(t.published_schema()["properties"]["repo"].is_object());
    let cfg = Arc::new(Config::from_toml(CFG).unwrap());
    let ctx = Ctx::new(cfg.clone(), bus());
    let v = (t.handler)(ctx.clone(), json!({"repo": "monorg/api", "pr": 5}))
        .await
        .unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["pull_requests"][0]["number"], 5);
    assert_eq!(v["pull_requests"][0]["account"], "a");

    // PR suivie via gh_watch (Store) : le tableau de bord la retrouve sans argument.
    let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
    store.watch_add("a", "monorg/api", Some(9)).unwrap();
    spawn(cfg, store, bus());
    let v = (t.handler)(ctx, json!({})).await.unwrap();
    assert_eq!(v["pull_requests"][0]["number"], 9);
}

#[tokio::test]
async fn releve_en_mode_test_ne_publie_rien() {
    let cfg = Config::from_toml(CFG).unwrap();
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    store.watch_add("a", "monorg/api", None).unwrap();
    assert_eq!(poll_once(&cfg, &store, &bus()).await, 0);
}

#[test]
fn resume_ci() {
    let ok = json!({"check_runs": [{"status": "completed", "conclusion": "success"}, {"status": "completed", "conclusion": "skipped"}]});
    assert_eq!(
        ci_summary(&json!({"state": "success", "total_count": 1}), &ok).0,
        "success"
    );
    let ko = json!({"check_runs": [{"status": "completed", "conclusion": "failure"}]});
    assert_eq!(
        ci_summary(&json!({"state": "success", "total_count": 1}), &ko),
        ("failure".into(), 1, 1, 0)
    );
    let en_cours = json!({"check_runs": [{"status": "in_progress", "conclusion": null}]});
    assert_eq!(
        ci_summary(&json!({"state": "pending", "total_count": 0}), &en_cours).0,
        "pending"
    );
    assert_eq!(
        ci_summary(
            &json!({"state": "pending", "total_count": 0}),
            &json!({"check_runs": []})
        )
        .0,
        "none"
    );
}
