//! get_diff : GET natif /pulls/{n} avec Accept vnd.github.diff (comme le Go GetRaw).
use ghmcp_core::{bus, net, Approver, Config};
use serde_json::json;
use std::sync::Arc;

fn ctx() -> ghmcp_core::Ctx {
    net::init(true, Approver::Deny);
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus())
}

#[tokio::test]
async fn get_diff_natif_avec_accept() {
    let ctx = ctx();
    let t = ghmcp_tools_pulls::tools()
        .into_iter()
        .find(|t| t.name == "pull_request_read")
        .unwrap();
    net::clear_recorded();
    (t.handler)(
        ctx,
        json!({"owner":"o","repo":"r","pullNumber":7,"method":"get_diff"}),
    )
    .await
    .unwrap();
    let c = net::recorded_calls();
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(
        (c[0].method.as_str(), c[0].path.as_str()),
        ("GET", "/repos/o/r/pulls/7")
    );
    assert_eq!(c[0].accept.as_deref(), Some("application/vnd.github.diff"));
}
