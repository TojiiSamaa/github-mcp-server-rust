//! Essai de bout en bout de `ghmcp serve` : le vrai binaire (sans --test, `post_json` coupé sinon), webhooks
//! signés HMAC, journal, outil gh_activity_feed, flux SSE, destination webhook générique, signature invalide.
//! Aucune sortie Internet : le compte fictif et la destination visent le même écouteur local du test.

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const SECRET: &str = "secret-fictif-e2e";

/// Arrête le serveur même si une assertion échoue.
struct Guard(Child, std::path::PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Requête HTTP/1.1 minimale (Connection: close) ; rend (statut, corps).
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    s.write_all(format!("{req}\r\n{body}").as_bytes()).unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (
        status,
        raw.split_once("\r\n\r\n")
            .map(|x| x.1.to_string())
            .unwrap_or_default(),
    )
}

fn signed(port: u16, event: &str, delivery: &str, body: &Value, secret: &str) -> u16 {
    let raw = body.to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(raw.as_bytes());
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let h = [
        ("x-hub-signature-256", sig), // scan:ok
        ("X-GitHub-Event", event.into()),
        ("X-GitHub-Delivery", delivery.into()),
        ("Content-Type", "application/json".into()),
    ];
    http(port, "POST", "/webhook", &h, &raw).0
}

/// Écouteur de la destination : consigne (méthode+chemin, corps JSON) de chaque POST reçu.
#[allow(clippy::type_complexity)] // (port, requêtes reçues) : type local à ce test
fn spawn_sink() -> (u16, Arc<Mutex<Vec<(String, Value)>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let got = Arc::new(Mutex::new(vec![]));
    let g = got.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s);
            let mut line = String::new();
            r.read_line(&mut line).ok();
            let path = format!(
                "{} {}",
                line.split(' ').next().unwrap_or(""),
                line.split(' ').nth(1).unwrap_or("")
            );
            let mut len = 0usize;
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).ok();
            g.lock()
                .unwrap()
                .push((path, serde_json::from_slice(&body).unwrap_or(Value::Null)));
            r.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").ok();
        }
    });
    (port, got)
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "délai dépassé : {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn up(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
        && http(port, "GET", "/api/events", &[], "").0 == 200
}

#[test]
fn serve_webhooks_signes_journal_feed_sse_destination() {
    let (sink_port, sink) = spawn_sink();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("ghmcp-serve-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "default_account = \"fictif\"\n[[accounts]]\nname = \"fictif\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FAKE_TOKEN\"\napi_url = \"http://127.0.0.1:{sink_port}\"\n\
             [webhook]\nsecret_env = \"GHMCP_E2E_SECRET\"\n\
             [destinations.cible]\ntype = \"webhook\"\nurl = \"http://127.0.0.1:{sink_port}/dest\"\n\
             [[rules]]\nname = \"ci-rouge\"\non = \"ci_failure\"\nto = [\"cible\"]\n\
             [[rules]]\nname = \"pr-ouverte\"\non = \"pr_opened\"\nto = [\"cible\"]\n"
        ),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_ghmcp"))
        .args(["serve", "--http", &format!("127.0.0.1:{port}"), "--config"])
        .arg(&cfg)
        .arg("--db")
        .arg(dir.join("activity.db"))
        .env("GHMCP_E2E_SECRET", SECRET)
        .env("GHMCP_FAKE_TOKEN", "jeton-fictif")
        .env_remove("MCP_TEST_MODE")
        .env_remove("MCP_TEST_APPROVER")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = Guard(child, dir.clone());
    wait_for("le serveur écoute", || up(port));
    assert_eq!(http(port, "GET", "/healthz", &[], ""), (200, "ok".into()));

    // SSE ouvert AVANT les envois ; les lignes arrivent par un canal lu avec délai borné.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /api/stream HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n")
            .unwrap();
        for l in BufReader::new(s).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let first = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("en-tête SSE");
    assert!(first.contains("200"), "SSE : {first}");

    let pr = json!({"action":"opened","pull_request":{"title":"Ajoute X","html_url":"https://exemple.test/pr/1","number":1},"repository":{"full_name":"org-fictive/depot"},"sender":{"login":"alice"}});
    let run = json!({"action":"completed","workflow_run":{"name":"CI","conclusion":"failure","head_branch":"main","html_url":"https://exemple.test/run/9"},"repository":{"full_name":"org-fictive/depot"},"sender":{"login":"bob"}});
    assert_eq!(signed(port, "pull_request", "d-pr-1", &pr, SECRET), 202);
    assert_eq!(signed(port, "workflow_run", "d-run-1", &run, SECRET), 202);

    // Mal signé ou non signé : refusé et absent du journal.
    let bad = json!({"action":"opened","pull_request":{"title":"Intrus"},"repository":{"full_name":"org-fictive/depot"}});
    let st = signed(port, "pull_request", "d-bad", &bad, "autre-secret");
    assert!(st == 401 || st == 403, "signature invalide acceptée : {st}");
    let (st0, _) = http(
        port,
        "POST",
        "/webhook",
        &[
            ("X-GitHub-Event", "pull_request".into()),
            ("X-GitHub-Delivery", "d-nosig".into()),
        ],
        &bad.to_string(),
    );
    assert!(st0 == 401 || st0 == 403, "sans signature accepté : {st0}");

    // Journal.
    let (st, body) = http(port, "GET", "/api/events", &[], "");
    assert_eq!(st, 200);
    let evs: Vec<Value> = serde_json::from_str(&body).unwrap();
    let mut ids: Vec<&str> = evs.iter().map(|e| e["id"].as_str().unwrap()).collect();
    ids.sort();
    assert_eq!(ids, ["d-pr-1", "d-run-1"], "journal : {body}");
    assert!(!body.contains("Intrus"));

    // gh_activity_feed via POST /mcp.
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gh_activity_feed","arguments":{"repo":"org-fictive/depot"}}});
    let (st, body) = http(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json".into()),
        ],
        &call.to_string(),
    );
    assert_eq!(st, 200, "{body}");
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_ne!(resp["result"]["isError"], true, "{body}");
    let feed: Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let kinds: Vec<&str> = feed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.len(), 2, "feed : {feed}");
    assert!(
        kinds.contains(&"pull_request") && kinds.contains(&"workflow_run"),
        "feed : {feed}"
    );

    // SSE : les deux événements, lus avec délai borné.
    let mut sse = String::new();
    let t = Instant::now();
    while !(sse.contains("d-pr-1") && sse.contains("d-run-1")) {
        let left = Duration::from_secs(10)
            .checked_sub(t.elapsed())
            .expect("SSE : événements non reçus");
        sse += &rx.recv_timeout(left).expect("SSE : événements non reçus");
        sse.push('\n');
    }
    assert!(
        sse.contains("event: pull_request") && sse.contains("event: workflow_run"),
        "SSE : {sse}"
    );
    assert!(!sse.contains("Intrus"));

    // Destination : un POST par règle, rien d'autre (polling désactivé).
    wait_for("2 POST de destination", || sink.lock().unwrap().len() >= 2);
    std::thread::sleep(Duration::from_millis(300));
    let got = sink.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "reçus : {got:?}");
    assert!(got.iter().all(|(p, _)| p == "POST /dest"), "{got:?}");
    let mut rules: Vec<(&str, &str)> = got
        .iter()
        .map(|(_, b)| {
            (
                b["rule"].as_str().unwrap(),
                b["event"]["id"].as_str().unwrap(),
            )
        })
        .collect();
    rules.sort();
    assert_eq!(rules, [("ci-rouge", "d-run-1"), ("pr-ouverte", "d-pr-1")]);
}
