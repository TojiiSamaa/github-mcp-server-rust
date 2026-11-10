//! Mode test, approbateur "deny" : toutes les suppressions échouent fermé.
use ghmcp_core::{bus, Config};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn suppressions_refusees() {
    ghmcp_core::net::init(true, ghmcp_core::Approver::parse("deny"));
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    let ctx = ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
    let args = json!({"owner": "o", "repo": "r", "hook_id": 1, "secret_name": "S", "name": "V", "runner_id": 2});
    let mut n = 0;
    for t in ghmcp_ext_hooks::tools()
        .into_iter()
        .filter(|t| t.needs_approval)
    {
        let mut a = args.clone();
        if t.name.contains("org_hook") {
            a["org"] = json!("x");
            a.as_object_mut().unwrap().remove("owner");
            a.as_object_mut().unwrap().remove("repo");
        }
        let e = (t.handler)(ctx.clone(), a).await.unwrap_err();
        assert_eq!(e.code(), "DENIED", "{}", t.name);
        n += 1;
    }
    assert_eq!(n, 5); // 2 webhooks, secret, variable, runner
}
