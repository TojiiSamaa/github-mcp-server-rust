//! Mode test, approbateur "approve" : chaque outil répond avec la bonne méthode et le bon chemin.
use ghmcp_core::{bus, Config};
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
async fn tous_les_outils_repondent() {
    ghmcp_core::net::init(true, ghmcp_core::Approver::parse("approve"));
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    let ctx = ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
    let args = json!({"owner": "o-test", "repo": "r-test", "org": "org-test", "username": "u-test", "package_type": "npm",
        "package_name": "pkg", "package_version_id": 7, "environment_name": "prod/eu", "deployment_id": 3, "protection_rule_id": 4,
        "alert_number": 5, "integration_id": 6, "ref": "main", "state": "open", "name": "main", "enabled": true,
        "secret_scanning": "enabled"});
    let tools = ghmcp_ext_platform::tools();
    assert_eq!(tools.len(), 45);
    let mut by = std::collections::HashMap::new();
    for t in &tools {
        assert!(t.name.starts_with("gh_"), "{}", t.name);
        let r: Value = (t.handler)(ctx.clone(), args.clone())
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", t.name));
        assert_eq!(r["test_mode"], true, "{}", t.name);
        by.insert(t.name.clone(), r);
    }
    let r = &by["gh_org_package_version_delete"];
    assert_eq!(
        (r["method"].as_str(), r["path"].as_str()),
        (
            Some("DELETE"),
            Some("/orgs/org-test/packages/npm/pkg/versions/7")
        )
    );
    assert_eq!(
        by["gh_environment_delete"]["path"],
        "/repos/o-test/r-test/environments/prod%2Feu"
    );
    assert_eq!(by["gh_vulnerability_alerts_set"]["method"], "PUT");
    assert_eq!(
        by["gh_repo_security_analysis_set"]["body"],
        json!({"security_and_analysis": {"secret_scanning": {"status": "enabled"}}})
    );
    assert_eq!(by["gh_deployment_create"]["body"], json!({"ref": "main"}));
    // un paramètre de chemin manquant est refusé
    let t = tools.iter().find(|t| t.name == "gh_pages_get").unwrap();
    assert_eq!(
        (t.handler)(ctx, json!({"owner": "o"}))
            .await
            .unwrap_err()
            .code(),
        "INVALID_ARGS"
    );
}
