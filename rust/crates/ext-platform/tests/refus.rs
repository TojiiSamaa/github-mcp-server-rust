//! Approbateur "deny" : toutes les suppressions échouent fermé.
use ghmcp_core::{bus, Config};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn suppressions_refusees() {
    ghmcp_core::net::init(true, ghmcp_core::Approver::parse("deny"));
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    let ctx = ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
    let args = json!({"owner": "o", "repo": "r", "org": "x", "username": "u", "package_type": "npm", "package_name": "p",
        "package_version_id": 1, "environment_name": "e", "deployment_id": 1, "protection_rule_id": 1});
    let mut n = 0;
    for t in ghmcp_ext_platform::tools()
        .into_iter()
        .filter(|t| t.needs_approval)
    {
        assert_eq!(
            (t.handler)(ctx.clone(), args.clone())
                .await
                .unwrap_err()
                .code(),
            "DENIED",
            "{}",
            t.name
        );
        n += 1;
    }
    assert_eq!(n, 7); // paquet + version (user, org) + environnement + règle + déploiement
}
