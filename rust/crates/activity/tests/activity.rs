use ghmcp_activity::{router, tools, verify_signature, Filter, State, Store};
use ghmcp_core::{bus, Config, Ctx, Event};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SECRET: &str = "secret-de-test-fictif";
const CFG: &str = "default_account = \"a\"\n[[accounts]]\nname = \"a\"\nkind = \"classic\"\ntoken_env = \"X\"\n[webhook]\nsecret_env = \"GHMCP_TEST_WH_SECRET\"\n";

fn sign(body: &[u8]) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    m.update(body);
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

fn ev(id: &str, repo: &str, kind: &str, at: &str) -> Event {
    Event {
        id: id.into(),
        account: "a".into(),
        repo: repo.into(),
        kind: kind.into(),
        action: "opened".into(),
        actor: "u".into(),
        title: "t".into(),
        url: "".into(),
        at: at.parse().unwrap(),
        payload: json!({"k": id}),
    }
}

async fn serve() -> (String, Arc<Store>, ghmcp_core::Bus) {
    std::env::set_var("GHMCP_TEST_WH_SECRET", SECRET);
    let config = Arc::new(Config::from_toml(CFG).unwrap());
    let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
    let bus = bus();
    let app = router(State {
        store: store.clone(),
        bus: bus.clone(),
        config,
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (addr, store, bus)
}

/// Requête HTTP/1.1 brute (pas de client HTTP dans les dépendances) ; rend (statut, corps).
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(b"\r\n").await.unwrap();
    s.write_all(body).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split(' ').nth(1).unwrap().parse().unwrap();
    (
        status,
        out.split("\r\n\r\n").nth(1).unwrap_or("").to_string(),
    )
}

fn body() -> Vec<u8> {
    br#"{"action":"opened","repository":{"full_name":"o/r"},"sender":{"login":"bob"},"pull_request":{"title":"Ajoute X","html_url":"https://github.com/o/r/pull/1"}}"#.to_vec()
}

#[tokio::test]
async fn signature_valide_acceptee() {
    let (addr, store, _bus) = serve().await;
    let b = body();
    let h = [
        ("x-hub-signature-256", sign(&b)), // scan:ok
        ("X-GitHub-Event", "pull_request".into()),
        ("X-GitHub-Delivery", "d1".into()),
    ];
    assert_eq!(http(&addr, "POST", "/webhook", &h, &b).await.0, 202);
    assert_eq!(
        http(&addr, "POST", "/webhook/a", &h, &b).await.0,
        200,
        "rejeu = idempotent"
    );
    let evs = store.query(&Filter::default()).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(
        (
            evs[0].repo.as_str(),
            evs[0].kind.as_str(),
            evs[0].actor.as_str(),
            evs[0].title.as_str()
        ),
        ("o/r", "pull_request", "bob", "Ajoute X")
    );
}

#[tokio::test]
async fn signature_invalide_refusee() {
    let (addr, store, _bus) = serve().await;
    let b = body();
    let bad = format!("sha256={}", "0".repeat(64));
    let h = [
        ("x-hub-signature-256", bad), // scan:ok
        ("X-GitHub-Event", "pull_request".into()),
    ];
    assert_eq!(http(&addr, "POST", "/webhook", &h, &b).await.0, 401);
    // signature valide pour un autre corps
    let h = [
        ("x-hub-signature-256", sign(b"autre")),
        ("X-GitHub-Event", "pull_request".into()),
    ];
    assert_eq!(http(&addr, "POST", "/webhook", &h, &b).await.0, 401);
    assert!(store.query(&Filter::default()).unwrap().is_empty());
}

#[tokio::test]
async fn signature_absente_refusee() {
    let (addr, store, _bus) = serve().await;
    let h = [("X-GitHub-Event", "pull_request".into())];
    assert_eq!(http(&addr, "POST", "/webhook", &h, &body()).await.0, 401);
    assert!(!verify_signature(b"k", b"x", None));
    assert!(!verify_signature(b"k", b"x", Some("sha1=abcd")));
    assert!(store.query(&Filter::default()).unwrap().is_empty());
}

#[tokio::test]
async fn journal_filtre() {
    let (addr, store, _bus) = serve().await;
    for e in [
        ev("1", "o/a", "pull_request", "2026-01-01T00:00:00Z"),
        ev("2", "o/a", "workflow_run", "2026-01-02T00:00:00Z"),
        ev("3", "o/b", "pull_request", "2026-01-03T00:00:00Z"),
    ] {
        assert!(store.insert(&e).unwrap());
    }
    assert!(!store
        .insert(&ev("1", "o/a", "pull_request", "2026-01-01T00:00:00Z"))
        .unwrap());
    let q = |f: Filter| {
        store
            .query(&f)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(q(Filter::default()), ["3", "2", "1"]);
    assert_eq!(
        q(Filter {
            repo: Some("o/a".into()),
            ..Default::default()
        }),
        ["2", "1"]
    );
    assert_eq!(
        q(Filter {
            kind: Some("pull_request".into()),
            ..Default::default()
        }),
        ["3", "1"]
    );
    assert_eq!(
        q(Filter {
            since: Some("2026-01-02T00:00:00Z".parse().unwrap()),
            until: Some("2026-01-02T12:00:00Z".parse().unwrap()),
            ..Default::default()
        }),
        ["2"]
    );
    assert_eq!(
        q(Filter {
            limit: Some(1),
            ..Default::default()
        }),
        ["3"]
    );
    assert!(q(Filter {
        account: Some("zz".into()),
        ..Default::default()
    })
    .is_empty());
    // API HTTP, mêmes filtres
    let (st, b) = http(
        &addr,
        "GET",
        "/api/events?repo=o/a&type=workflow_run",
        &[],
        b"",
    )
    .await;
    let v: Value = serde_json::from_str(&b).unwrap_or_else(|_| panic!("corps: {b}"));
    assert_eq!((st, v.as_array().unwrap().len()), (200, 1));
}

#[tokio::test]
async fn sse_recoit_evenement_publie() {
    let (addr, _store, bus) = serve().await;
    let mut s = TcpStream::connect(&addr).await.unwrap();
    s.write_all(b"GET /api/stream HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n")
        .await
        .unwrap();
    let mut buf = vec![0u8; 8192];
    let n = s.read(&mut buf).await.unwrap(); // en-têtes reçus = abonné enregistré
    assert!(String::from_utf8_lossy(&buf[..n]).contains("text/event-stream"));
    bus.send(ev("sse1", "o/r", "release", "2026-01-01T00:00:00Z"))
        .unwrap();
    let mut got = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !got.contains("sse1") {
            let n = s.read(&mut buf).await.unwrap();
            got += &String::from_utf8_lossy(&buf[..n]);
        }
    })
    .await
    .expect("événement SSE non reçu");
    assert!(got.contains("event: release"));
}

#[tokio::test]
async fn outils_et_suivi() {
    let (_addr, _store, bus) = serve().await; // pose le magasin des outils
    let t = tools();
    let names: Vec<_> = t.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, ["gh_activity_feed", "gh_watch"]);
    let ctx = Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus);
    let call = |i: usize, a: Value| (t[i].handler)(ctx.clone(), a);
    call(1, json!({"action":"add","repo":"o/r","pr":7}))
        .await
        .unwrap();
    call(1, json!({"action":"add","repo":"o/r"})).await.unwrap();
    let l = call(1, json!({"action":"list"})).await.unwrap();
    assert_eq!(l["watches"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(1, json!({"action":"remove","repo":"o/r","pr":7}))
            .await
            .unwrap()["removed"],
        true
    );
    assert!(call(1, json!({"action":"add","repo":"pas un depot"}))
        .await
        .is_err());
    assert!(call(0, json!({"since":"hier"})).await.is_err());
    assert!(call(0, json!({"limit":5})).await.unwrap()["events"].is_array());
}
