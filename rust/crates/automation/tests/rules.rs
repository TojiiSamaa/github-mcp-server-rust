//! Hors mode test (net::init(false)) : la destination webhook reçoit vraiment le POST sur un écouteur local.
use ghmcp_activity::Store;
use ghmcp_automation::{invitation_allowed, preset, process, rule_matches, spawn, Auto, Rule};
use ghmcp_core::{bus, net, Config, Event};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const CFG: &str = r#"
default_account = "a"
[[accounts]]
name = "a"
kind = "classic"
token_env = "GHMCP_TEST_UNUSED"
"#;

fn ev(id: &str, kind: &str, action: &str, repo: &str, payload: Value) -> Event {
    Event {
        id: id.into(),
        account: "a".into(),
        repo: repo.into(),
        kind: kind.into(),
        action: action.into(),
        actor: "bob".into(),
        title: "CI".into(),
        url: "https://github.invalid/x".into(),
        at: chrono::Utc::now(),
        payload,
    }
}
fn run(id: &str, concl: &str) -> Event {
    ev(
        id,
        "workflow_run",
        "completed",
        "monorg/api",
        json!({"workflow_run": {"conclusion": concl, "head_branch": "main"}}),
    )
}

/// Écouteur axum sur 127.0.0.1:0 ; chaque corps JSON reçu part dans le canal.
async fn listener() -> (String, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |axum::Json(v): axum::Json<Value>| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(v);
                "ok"
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, rx)
}

fn cfg_with(url: &str) -> Arc<Config> {
    let t = format!(
        "{CFG}\n[destinations.hook]\ntype = \"webhook\"\nurl = \"{url}\"\n[[rules]]\nname = \"ci-rouge\"\non = \"ci_failure\"\nfilters = {{ repo = \"monorg/*\", branch = \"main\" }}\nto = [\"hook\"]\n"
    );
    Arc::new(Config::from_toml(&t).unwrap())
}

fn mem() -> Arc<Store> {
    Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap())
}

#[tokio::test]
async fn workflow_run_failure_declenche_la_destination() {
    net::init(false, net::Approver::Deny);
    let (url, mut rx) = listener().await;
    let bus = bus();
    spawn(cfg_with(&url), mem(), bus.clone());
    tokio::time::sleep(Duration::from_millis(50)).await; // laisse l'abonnement se poser
    bus.send(run("r1", "failure")).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("rien reçu")
        .unwrap();
    assert_eq!(got["rule"], "ci-rouge");
    assert_eq!(got["event"]["id"], "r1");
    assert_eq!(
        got["event"]["payload"]["workflow_run"]["conclusion"],
        "failure"
    );
}

#[tokio::test]
async fn regle_non_correspondante_ne_declenche_rien() {
    net::init(false, net::Approver::Deny);
    let (url, mut rx) = listener().await;
    let bus = bus();
    spawn(cfg_with(&url), mem(), bus.clone());
    tokio::time::sleep(Duration::from_millis(50)).await;
    bus.send(run("ok", "success")).unwrap(); // mauvaise conclusion
    bus.send(ev(
        "autre",
        "workflow_run",
        "completed",
        "autreorg/api",
        json!({"workflow_run": {"conclusion": "failure", "head_branch": "main"}}),
    ))
    .unwrap(); // mauvais dépôt
    bus.send(ev("pr", "pull_request", "opened", "monorg/api", json!({})))
        .unwrap(); // mauvais type
    bus.send(run("rouge", "failure")).unwrap(); // sentinelle : seule celle-ci doit sortir
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("sentinelle non reçue")
        .unwrap();
    assert_eq!(got["event"]["id"], "rouge");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err(),
        "un second POST est parti"
    );
}

#[tokio::test]
async fn liste_blanche_des_invitations() {
    net::init(false, net::Approver::Deny);
    let allow = vec!["monorg".to_string(), "ami/projet".to_string()];
    assert!(
        invitation_allowed(&allow, "MonOrg/depot")
            && invitation_allowed(&allow, "ami/projet")
            && invitation_allowed(&allow, "monorg")
    );
    assert!(
        !invitation_allowed(&allow, "ami/autre")
            && !invitation_allowed(&allow, "inconnu/x")
            && !invitation_allowed(&[], "monorg/x")
    );

    // Parcours complet : invitation hors liste = refusée AVANT tout appel réseau.
    let cfg = Arc::new(
        Config::from_toml(&format!("{CFG}\n[actions]\nenabled = true\ninvitation_allow = [\"monorg\"]\n[[rules]]\nname = \"inv\"\non = \"invitation\"\nactions = [\"accept_invitation\"]\n")).unwrap(),
    );
    let res = process(
        &Auto::from_config(cfg).unwrap(),
        &ev(
            "i1",
            "invitation",
            "created",
            "pirate/depot",
            json!({"invitation": {"id": 7}}),
        ),
    )
    .await;
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].1.as_ref().unwrap_err().code(), "DENIED");
}

#[tokio::test]
async fn actions_desactivees_par_defaut() {
    net::init(false, net::Approver::Deny);
    let cfg = Arc::new(
        Config::from_toml(&format!(
            "{CFG}\n[[rules]]\nname = \"m\"\non = \"pr_opened\"\nactions = [\"merge_if_green\"]\n"
        ))
        .unwrap(),
    );
    let res = process(
        &Auto::from_config(cfg).unwrap(),
        &ev(
            "p",
            "pull_request",
            "opened",
            "o/r",
            json!({"pull_request": {"number": 3}}),
        ),
    )
    .await;
    assert_eq!(res[0].1.as_ref().unwrap_err().code(), "DENIED");
}

#[test]
fn prereglages() {
    let e = |kind: &str, action: &str, p: Value| ev("x", kind, action, "o/r", p);
    assert!(preset(
        "ci_failure",
        &e(
            "check_suite",
            "completed",
            json!({"check_suite": {"conclusion": "failure"}})
        )
    ));
    assert!(!preset(
        "ci_failure",
        &e(
            "workflow_run",
            "completed",
            json!({"workflow_run": {"conclusion": "success"}})
        )
    ));
    assert!(preset(
        "package_failed",
        &e("release", "created", json!({"release": {"draft": true}}))
    ));
    assert!(preset(
        "deploy_error",
        &e(
            "deployment_status",
            "created",
            json!({"deployment_status": {"state": "error"}})
        )
    ));
    assert!(!preset(
        "deploy_error",
        &e(
            "deployment_status",
            "created",
            json!({"deployment_status": {"state": "success"}})
        )
    ));
    assert!(preset("pr_opened", &e("pull_request", "opened", json!({}))));
    assert!(preset(
        "pr_merged",
        &e(
            "pull_request",
            "closed",
            json!({"pull_request": {"merged": true}})
        )
    ));
    assert!(!preset(
        "pr_merged",
        &e(
            "pull_request",
            "closed",
            json!({"pull_request": {"merged": false}})
        )
    ));
    assert!(preset("comment", &e("issue_comment", "created", json!({}))));
    assert!(preset("invitation", &e("invitation", "created", json!({}))));
    assert!(preset(
        "security_alert",
        &e("dependabot_alert", "created", json!({}))
    ));
    assert!(preset(
        "workflow_run",
        &e("workflow_run", "completed", json!({}))
    ));
    // mention : frontière de mot (@bob ne matche pas @bobby)
    let r: Rule =
        serde_json::from_value(json!({"on": "mention", "filters": {"mention": "bob"}})).unwrap();
    let m = |body: &str| {
        rule_matches(
            &r,
            &e(
                "issue_comment",
                "created",
                json!({"comment": {"body": body}}),
            ),
        )
    };
    assert!(m("salut @bob, un avis ?") && !m("salut @bobby") && !m("pas de mention"));
}
