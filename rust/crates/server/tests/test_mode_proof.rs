//! Preuve A + B du mode test (règle MCP publics).
//! A = structurelle : aucun reqwest::/TcpStream/Command::new hors de core::net, et le vrai client
//!     refuse de se construire en mode test. Test de mutation : le scan appliqué à un extrait fictif.
//! B = dynamique : le binaire en mode test, compte dont api_url vise un écouteur sentinelle local,
//!     TOUS les outils appelés ; 0 connexion reçue.

use serde_json::{json, Value};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const FORBIDDEN: [&str; 3] = ["reqwest::", "TcpStream", "Command::new"];

/// Violations dans un texte source (lignes de commentaire ignorées).
fn scan(text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .filter(|(_, l)| FORBIDDEN.iter().any(|f| l.contains(f)))
        .map(|(i, l)| format!("ligne {}: {}", i + 1, l.trim()))
        .collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn a_aucun_appel_reseau_hors_du_module_net() {
    let mut bad = vec![];
    let mut scanned = 0;
    for c in std::fs::read_dir(crates_dir()).unwrap().flatten() {
        let src = c.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = vec![];
        rs_files(&src, &mut files);
        for f in files {
            if f.file_name().is_some_and(|n| n == "net.rs") && c.file_name() == "core" {
                continue; // le point unique
            }
            scanned += 1;
            for v in scan(&std::fs::read_to_string(&f).unwrap()) {
                bad.push(format!("{}: {v}", f.display()));
            }
        }
    }
    assert!(scanned > 20, "scan trop pauvre : {scanned} fichiers");
    assert!(
        bad.is_empty(),
        "appel réseau/processus hors core::net : {bad:#?}"
    );
}

#[test]
fn a_le_vrai_client_refuse_de_se_construire_en_mode_test() {
    let net = std::fs::read_to_string(crates_dir().join("core/src/net.rs")).unwrap();
    let body = net.split("fn client()").nth(1).expect("fn client");
    let guard = body.find("if is_test()").expect("garde is_test");
    let build = body
        .find("reqwest::Client::builder()")
        .expect("construction");
    assert!(
        guard < build,
        "la garde du mode test doit précéder la construction du client"
    );
    assert!(
        body[guard..build].contains("return Err(blocked("),
        "la garde doit refuser"
    );
}

/// Mutation documentée : un appel réel ajouté hors du point unique DOIT être vu par le scan.
#[test]
fn a_mutation_un_appel_reel_ferait_echouer_le_scan() {
    let sain = "pub fn f() { let _ = ghmcp_core::net::post_json(\"u\", &v); }";
    assert!(scan(sain).is_empty());
    for mute in [
        "fn f() { let c = reqwest::Client::new(); }",
        "fn f() { let s = std::net::TcpStream::connect(\"192.0.2.1:80\"); }",
        "fn f() { std::process::Command::new(\"curl\"); }",
    ] {
        assert!(
            !scan(mute).is_empty(),
            "le scan aurait dû échouer sur : {mute}"
        );
    }
}

/// Arguments minimaux valides pour le schéma : les paramètres requis seulement.
fn minimal_args(schema: &Value) -> Value {
    let mut a = serde_json::Map::new();
    for k in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let p = &schema["properties"][k];
        let v = match (p["enum"].get(0), p["type"].as_str()) {
            (Some(e), _) => e.clone(),
            (_, Some("integer" | "number")) => json!(1),
            (_, Some("boolean")) => json!(false),
            (_, Some("array")) => json!([]),
            (_, Some("object")) => json!({}),
            _ => json!("x"),
        };
        a.insert(k.into(), v);
    }
    Value::Object(a)
}

#[test]
fn b_tous_les_outils_repondent_sans_aucune_connexion() {
    // Écouteur sentinelle : toute connexion reçue = échec.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            h.fetch_add(1, Ordering::SeqCst);
        }
    });

    let dir = std::env::temp_dir().join(format!("ghmcp-proof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!("default_account = \"sentinelle\"\n[[accounts]]\nname = \"sentinelle\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FAKE_TOKEN\"\napi_url = \"http://127.0.0.1:{port}\"\n"),
    )
    .unwrap();

    let tools = ghmcp::registry();
    let mut input = String::new();
    input += &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"preuve","version":"0"}}}).to_string();
    input += "\n";
    input += &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string();
    input += "\n";
    for (i, t) in tools.iter().enumerate() {
        let req = json!({"jsonrpc":"2.0","id":100 + i,"method":"tools/call","params":{"name":t.name,"arguments":minimal_args(&t.published_schema())}});
        input += &format!("{req}\n");
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_ghmcp"))
        .args(["--test", "--config"])
        .arg(&cfg)
        .env_remove("MCP_TEST_APPROVER")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap(); // fermé à la fin : le serveur sort à l'EOF
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output()).ok());
    let out = rx
        .recv_timeout(std::time::Duration::from_secs(240))
        .expect("le serveur ne s'est pas terminé")
        .unwrap();
    assert!(
        out.status.success(),
        "le serveur a planté (panique ?) : {:?}",
        out.status
    );

    let responses: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let listed = responses.iter().find(|r| r["id"] == 2).expect("tools/list")["result"]["tools"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(listed, tools.len());

    // Le marqueur est posé par le serveur (point unique) : tout outil le porte, ou rend une erreur typée.
    let (mut marques, mut erreurs_typees, mut fautifs) = (0, 0, vec![]);
    for (i, t) in tools.iter().enumerate() {
        let r = responses
            .iter()
            .find(|r| r["id"] == 100 + i)
            .unwrap_or_else(|| panic!("pas de réponse pour {}", t.name));
        assert!(
            r.get("error").is_none() || r["error"].is_null(),
            "{} : erreur JSON-RPC {}",
            t.name,
            r["error"]
        );
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or("");
        if text.contains("\"test_mode\":true") {
            marques += 1;
        } else if r["result"]["isError"] == true {
            erreurs_typees += 1;
        } else {
            fautifs.push(t.name.clone());
        }
    }
    eprintln!("B : {} outils, {marques} marqués test_mode, {erreurs_typees} erreurs typées, {} fautifs {fautifs:?}", tools.len(), fautifs.len());
    assert!(
        fautifs.is_empty(),
        "réponse sans marqueur test_mode ni erreur typée : {fautifs:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "l'écouteur sentinelle a reçu une connexion"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
