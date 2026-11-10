//! Bug rjd : create_or_update_file refuse l'écrasement à l'aveugle et l'écriture à travers un lien symbolique
//! (repositories.go:562-638) ; get_file_contents signale un lien symbolique non rendu (repositories_helper.go:193-213).
//! Réponses à la vraie forme GitHub : crates/server/tests/data/fixtures.
use ghmcp_core::{bus, net, Approver, Config};
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
async fn liens_symboliques_et_ecrasement_a_l_aveugle() {
    std::env::set_var(
        "GHMCP_FIXTURES",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../server/tests/data/fixtures"),
    );
    net::init(true, Approver::Approve);
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    let ctx = ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
    let tool = |n: &str| {
        ghmcp_tools_repos::tools()
            .into_iter()
            .find(|t| t.name == n)
            .unwrap()
    };
    let put = || net::recorded_calls().iter().any(|c| c.method == "PUT");
    let w = tool("create_or_update_file");
    let base =
        json!({"owner":"o","repo":"r","path":"lien","content":"x","message":"m","branch":"b-lien"});

    net::clear_recorded();
    let mut a = base.clone();
    a["sha"] = json!("3333333333333333333333333333333333333333");
    let e = (w.handler)(ctx.clone(), a)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        e.contains("symbolic link") && e.contains("cible/gros.bin"),
        "lien non bloqué : {e:?}"
    );
    assert!(!put(), "PUT envoyé à travers un lien symbolique");

    net::clear_recorded();
    let e = (w.handler)(ctx.clone(), base.clone())
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        e.contains("File already exists"),
        "écrasement à l'aveugle : {e:?}"
    );
    assert!(!put(), "PUT envoyé sans sha sur un fichier existant");

    let v: Value = (tool("get_file_contents").handler)(ctx.clone(), json!({"owner":"o","repo":"r","path":"gros-lien","sha":"1111111111111111111111111111111111111111"})).await.unwrap();
    assert_eq!(
        (
            v["type"].as_str(),
            v["target"].as_str(),
            v["resolved_target_path"].as_str()
        ),
        (
            Some("symlink"),
            Some("cible/gros.bin"),
            Some("cible/gros.bin")
        ),
        "{v}"
    );
}
