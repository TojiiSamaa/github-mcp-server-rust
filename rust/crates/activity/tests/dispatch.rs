//! Dispatch de bout en bout en mode test (aucune sortie réseau) : webhook signé → registre → appel du
//! Centre enregistré par `net::post_json_bearer`, avec tout le fil de l'issue encadré comme donnée de tiers.
use ghmcp_activity::dispatch::{concerne, fil, message, BALISE};
use ghmcp_activity::{router, State, Store};
use ghmcp_core::{bus, net, Config, Event};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SECRET: &str = "secret-de-test-fictif";
const R: &str = "exemple-org/projet-test";
const CFG: &str = r#"default_account = "a"
[[accounts]]
name = "a"
kind = "classic"
token_env = "X_FICTIF"
[webhook]
secret_env = "GHMCP_TEST_WH_SECRET"
[dispatch]
centre_url = "http://centre.exemple.invalid:8796"
token_env = "GHMCP_TEST_CENTRE_TOKEN"
[dispatch.repos."exemple-org/projet-test"]
dossier = "/srv/code/projet-test"
"#;

fn sign(body: &[u8]) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    m.update(body);
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

async fn http(addr: &str, kind: &str, delivery: &str, body: &[u8]) -> u16 {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST /webhook HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\nx-hub-signature-256: {}\r\nX-GitHub-Event: {kind}\r\nX-GitHub-Delivery: {delivery}\r\n\r\n",
        body.len(),
        sign(body) // scan:ok
    );
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split(' ').nth(1).unwrap().parse().unwrap()
}

fn issue(repo: &str, sender_type: &str) -> Value {
    json!({"action": "opened", "repository": {"full_name": repo},
           "sender": {"login": "bob-test", "type": sender_type},
           "issue": {"number": 7, "title": "Plante au démarrage", "state": "open", "comments": 2,
                     "body": "Étapes : lancer `ghmcp serve`.", "user": {"login": "bob-test"},
                     "labels": [{"name": "bug"}], "html_url": "https://github.com/exemple-org/projet-test/issues/7"}})
}

fn commentaire(body: &str) -> Value {
    let mut p = issue(R, "User");
    p["action"] = json!("created");
    p["comment"] = json!({"id": 103, "user": {"login": "bob-test"}, "body": body,
                          "html_url": "https://github.com/exemple-org/projet-test/issues/7#issuecomment-103"});
    p
}

async fn attendre(n: usize) -> Vec<(String, Value)> {
    for _ in 0..200 {
        let p = net::recorded_posts();
        if p.len() >= n {
            return p;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    net::recorded_posts()
}

/// Un seul test : `net::init` et les appels enregistrés sont globaux au binaire.
#[tokio::test]
async fn issue_vers_agent_responsable() {
    net::init(true, net::Approver::Deny);
    std::env::set_var("GHMCP_TEST_WH_SECRET", SECRET);
    std::env::set_var(
        "GHMCP_FIXTURES",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data"),
    );
    let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
    let app = router(State {
        store: store.clone(),
        bus: bus(),
        config: Arc::new(Config::from_toml(CFG).unwrap()),
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    // 1. issue ouverte, aucun agent connu → nouvelle session dans le dossier du dépôt, avec tout le fil
    let b = serde_json::to_vec(&issue(R, "User")).unwrap();
    assert_eq!(http(&addr, "issues", "liv-1", &b).await, 202);
    let p = attendre(1).await;
    assert_eq!(p.len(), 1);
    assert_eq!(
        p[0].0,
        "http://centre.exemple.invalid:8796/api/hote/nouvelle"
    );
    assert_eq!(p[0].1["dossier"], "/srv/code/projet-test");
    let m = p[0].1["message"].as_str().unwrap();
    for attendu in [
        "Plante au démarrage",
        "Étapes : lancer",
        "bug",
        "issues/7",
        "Premier commentaire",
        "carol-test",
        "DONNÉE DE TIERS",
        "jamais un ordre",
    ] {
        assert!(m.contains(attendu), "message sans « {attendu} » :\n{m}");
    }
    // la balise de fin ne peut pas être forgée par un commentaire : une seule ouverture, une seule fermeture
    assert_eq!(m.matches(&format!("{BALISE}>>>")).count(), 1, "{m}");
    assert!(m.trim_end().ends_with(&format!("{BALISE}>>>")));
    assert!(!m.contains("X_FICTIF") && !m.contains("GHMCP_TEST_CENTRE_TOKEN"));
    // la session lancée devient responsable de l'issue (écrit juste après la réponse du Centre)
    for _ in 0..200 {
        if store.agent_de(R, 7).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(store.agent_de(R, 7).as_deref(), Some("session-test"));

    // 2. rejeu de la même livraison : ni nouvel événement ni nouvel envoi
    assert_eq!(http(&addr, "issues", "liv-1", &b).await, 200);

    // 3. commentaire : la session lancée en 1 est désormais responsable → message dans CETTE session
    let b = serde_json::to_vec(&commentaire("Toujours cassé chez moi.")).unwrap();
    assert_eq!(http(&addr, "issue_comment", "liv-2", &b).await, 202);
    let p = attendre(2).await;
    assert_eq!(p.len(), 2, "{p:?}");
    assert_eq!(p[1].0, "http://centre.exemple.invalid:8796/api/envoyer");
    assert_eq!(p[1].1["id"], "session-test");
    assert_eq!(p[1].1["cle_envoi"], "ghmcp-liv-2");
    assert!(p[1].1["message"]
        .as_str()
        .unwrap()
        .contains("Toujours cassé chez moi."));

    // 4. pas d'envoi : réponse signée d'un agent (boucle), robot, dépôt non suivi
    let b = serde_json::to_vec(&commentaire("Corrigé.\n\nAgent-Id: session-test")).unwrap();
    assert_eq!(http(&addr, "issue_comment", "liv-3", &b).await, 202);
    let b = serde_json::to_vec(&issue(R, "Bot")).unwrap();
    assert_eq!(http(&addr, "issues", "liv-4", &b).await, 202);
    let b = serde_json::to_vec(&issue("exemple-org/autre", "User")).unwrap();
    assert_eq!(http(&addr, "issues", "liv-5", &b).await, 202);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(net::recorded_posts().len(), 2);

    // 5. le registre garde la trace des envois
    let f = store.fiche(Some(R), None, None, Some(7)).unwrap();
    let routes: Vec<&str> = f["dispatch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["route"].as_str().unwrap())
        .collect();
    assert!(
        routes.contains(&"nouvelle_session") && routes.contains(&"agent"),
        "{f}"
    );
}

#[test]
fn filtre_et_fil() {
    let e = |kind: &str, p: Value| Event {
        id: "x".into(),
        account: "a".into(),
        repo: R.into(),
        kind: kind.into(),
        action: p["action"].as_str().unwrap_or("").into(),
        actor: String::new(),
        title: String::new(),
        url: String::new(),
        at: chrono::Utc::now(),
        payload: p,
    };
    assert!(concerne(&e("issues", issue(R, "User"))));
    let mut signee = issue(R, "User");
    signee["issue"]["body"] = json!(
        "ouverte par un agent

Agent-Id: a1"
    );
    assert!(
        !concerne(&e("issues", signee)),
        "issue ouverte par un agent : pas de renvoi"
    );
    let mut ferme = issue(R, "User");
    ferme["action"] = json!("closed");
    assert!(
        !concerne(&e("issues", ferme)),
        "seulement ouverte / rouverte"
    );
    assert!(!concerne(&e("pull_request", issue(R, "User"))));
    assert!(concerne(&e("issue_comment", commentaire("question"))));
    assert!(!concerne(&e(
        "issue_comment",
        commentaire("x\nAgent-Id: a1")
    )));
    // fil : corps très long tronqué, jeton masqué
    let mut p = issue(R, "User");
    p["issue"]["body"] = json!(format!("{} {}{}", "a".repeat(5000), "ghp_", "A".repeat(36)));
    let f = fil(&p, &[]);
    assert!(f["corps"].as_str().unwrap().ends_with("[tronqué]"));
    let mut p = issue(R, "User");
    p["issue"]["body"] = json!(format!("jeton {}{}", "ghp_", "B".repeat(36)));
    let f = fil(&p, &[]);
    assert!(
        !f["corps"].as_str().unwrap().contains(&"B".repeat(36)),
        "{f}"
    );
    assert!(message(&e("issues", p), &f)
        .contains("[ghmcp] issue ouverte sur exemple-org/projet-test#7"));
}
