//! Bug 7z7 : actions_run_logs rend l'URL de redirection 302 sans la suivre.
use ghmcp_core::{bus, net, Approver, Config};
use serde_json::{json, Value};
use std::sync::Arc;

fn ctx() -> ghmcp_core::Ctx {
    net::init(true, Approver::Deny);
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus())
}

#[tokio::test]
async fn run_logs_rend_l_url_location() {
    let ctx = ctx();
    let t = ghmcp_ext_hooks::tools()
        .into_iter()
        .find(|t| t.name == "gh_actions_run_logs")
        .unwrap();
    net::clear_recorded();
    let v: Value = (t.handler)(ctx, json!({"owner":"o","repo":"r","run_id":12}))
        .await
        .unwrap();
    assert_eq!(
        v["download_url"], "https://test-mode.invalid/repos/o/r/actions/runs/12/logs",
        "{v}"
    );
    let c = net::recorded_calls();
    assert_eq!(
        (c[0].method.as_str(), c[0].path.as_str()),
        ("GET", "/repos/o/r/actions/runs/12/logs")
    );
}
