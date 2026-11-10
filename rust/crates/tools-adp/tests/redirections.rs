//! Bug 7z7 : logs et artefacts rendent l'URL de redirection 302 (en-tête location), sans la suivre.
use ghmcp_core::{bus, net, Approver, Config};
use serde_json::{json, Value};
use std::sync::Arc;

fn ctx() -> ghmcp_core::Ctx {
    net::init(true, Approver::Deny);
    let cfg = "default_account = \"a-test\"\n[[accounts]]\nname = \"a-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FICTIF\"\n";
    ghmcp_core::Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus())
}

#[tokio::test]
async fn logs_et_artefact_rendent_l_url_location() {
    let ctx = ctx();
    let t = ghmcp_tools_adp::tools()
        .into_iter()
        .find(|t| t.name == "actions_get")
        .unwrap();
    for (m, route, champ) in [
        (
            "get_workflow_run_logs_url",
            "/repos/o/r/actions/runs/12/logs",
            "logs_url",
        ),
        (
            "download_workflow_run_artifact",
            "/repos/o/r/actions/artifacts/12/zip",
            "download_url",
        ),
    ] {
        net::clear_recorded();
        let v: Value = (t.handler)(
            ctx.clone(),
            json!({"owner":"o","repo":"r","method":m,"resource_id":"12"}),
        )
        .await
        .unwrap();
        assert_eq!(
            v[champ],
            format!("https://test-mode.invalid{route}"),
            "{m}: {v}"
        );
        let c = net::recorded_calls();
        assert_eq!(
            (c[0].method.as_str(), c[0].path.as_str()),
            ("GET", route),
            "{m}"
        );
    }
}
