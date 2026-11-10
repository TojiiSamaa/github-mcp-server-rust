//! Enregistreur du backend simulé + options de requête en mode test (processus dédié, un seul test : l'enregistreur est global).
use ghmcp_core::config::{AccountCfg, Config};
use ghmcp_core::net::{self, Approver, Gh, ReqOpts};
use serde_json::json;

#[tokio::test]
async fn appels_enregistres() {
    net::init(true, Approver::Deny);
    let a: AccountCfg = Config::from_toml("").unwrap().accounts[0].clone();
    let gh = Gh::new(a);
    net::clear_recorded();

    gh.rest("GET", "/user", &[], None).await.unwrap();
    gh.paginate("/repos/o/r/issues", &[], None, 2)
        .await
        .unwrap();
    gh.graphql_with(
        "query{viewer{login}}",
        json!({}),
        &[("GraphQL-Features".into(), "x".into())],
    )
    .await
    .unwrap();
    gh.graphql("query{a}", json!({})).await.unwrap();
    let opts = ReqOpts {
        accept: Some("application/vnd.github.diff".into()),
        headers: vec![("X-Test".into(), "1".into())],
        no_redirect: true,
    };
    let r = gh
        .rest_raw_with("GET", "/repos/o/r/actions/runs/1/logs", &[], None, &opts)
        .await
        .unwrap();
    let r2 = gh.rest_raw("GET", "/user", &[], None).await.unwrap();

    assert_eq!(r.status, 302);
    assert_eq!(
        r.headers,
        vec![(
            "location".to_string(),
            "https://test-mode.invalid/repos/o/r/actions/runs/1/logs".to_string()
        )]
    );
    assert_eq!(r2.status, 200);

    let c = net::recorded_calls();
    let vu: Vec<_> = c
        .iter()
        .map(|x| (x.method.as_str(), x.path.as_str()))
        .collect();
    assert_eq!(
        vu,
        [
            ("GET", "/user"),
            ("GET", "/repos/o/r/issues"),
            ("POST", "graphql"),
            ("POST", "graphql"),
            ("GET", "/repos/o/r/actions/runs/1/logs"),
            ("GET", "/user")
        ]
    );
    assert_eq!(
        c[2].headers,
        vec![("GraphQL-Features".to_string(), "x".to_string())]
    );
    assert!(c[3].headers.is_empty());
    assert_eq!(c[4].accept.as_deref(), Some("application/vnd.github.diff"));
    assert_eq!(c[4].headers, vec![("X-Test".to_string(), "1".to_string())]);

    net::clear_recorded();
    assert!(net::recorded_calls().is_empty());
}
