//! /mcp en HTTP : jeton Bearer exigé dès que `[webhook] api_token_env` est défini, et refus de démarrer hors boucle
//! locale sans jeton (bug GITHUB-2611100633-f4v : /mcp ouvert pilotait les jetons GitHub). Mode test : aucune sortie réseau.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const JETON: &str = "jeton-api-fictif";

fn ghmcp(args: &[&str], cfg: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ghmcp"));
    c.args(args)
        .arg("--config")
        .arg(cfg)
        .arg("--test")
        .env("GHMCP_API_FICTIF", JETON)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn config(nom: &str, avec_jeton: bool) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ghmcp-auth-{nom}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("config.toml");
    let w = if avec_jeton {
        "[webhook]\napi_token_env = \"GHMCP_API_FICTIF\"\n"
    } else {
        ""
    };
    std::fs::write(&p, w).unwrap();
    p
}

fn post_mcp(port: u16, auth: Option<&str>) -> u16 {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let a = auth
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\n{a}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    r.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

#[test]
fn hors_boucle_locale_sans_jeton_refuse_de_demarrer() {
    let cfg = config("refus", false);
    let st = ghmcp(&["--http", "0.0.0.0:0"], &cfg).status().unwrap();
    assert_eq!(st.code(), Some(2), "doit refuser 0.0.0.0 sans jeton d'API");
}

#[test]
fn mcp_exige_le_jeton_quand_il_est_configure() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = config("bearer", true);
    let mut child = ghmcp(&["--http", &format!("127.0.0.1:{port}")], &cfg)
        .spawn()
        .unwrap();
    let t = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "le serveur n'écoute pas"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let (sans, faux, bon) = (
        post_mcp(port, None),
        post_mcp(port, Some("faux")),
        post_mcp(port, Some(JETON)),
    );
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!((sans, faux, bon), (401, 401, 200));
}
