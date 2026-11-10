//! get_file_contents vu par le client MCP (tools/call sur le binaire, mode test, réponse GitHub simulée) :
//! comme le Go (repositories.go NewToolResultResource), un texte « successfully downloaded » puis une ressource embarquée.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn get_file_contents_rend_la_ressource_embarquee_au_client() {
    let dir = std::env::temp_dir().join(format!("ghmcp-ressource-{}", std::process::id()));
    let fx = dir.join("fixtures");
    std::fs::create_dir_all(&fx).unwrap();
    // Vraie forme de GET /repos/{o}/{r}/contents/{path} pour un fichier ("# Titre\n" en base64).
    std::fs::write(
        fx.join("get_repos_acme_demo_contents_README_md.json"),
        r#"{"type":"file","encoding":"base64","content":"IyBUaXRyZQo=\n","size":8,"name":"README.md","path":"README.md","sha":"0635b4b"}"#,
    )
    .unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "default_account = \"fictif\"\n[[accounts]]\nname = \"fictif\"\nkind = \"classic\"\ntoken_env = \"GHMCP_FAKE_TOKEN\"\napi_url = \"https://api.invalid\"\n").unwrap();

    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_file_contents","arguments":{"owner":"acme","repo":"demo","path":"README.md","sha":"abc"}}}),
    ]
    .iter()
    .map(|v| format!("{v}\n"))
    .collect::<String>();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ghmcp"))
        .args(["--test", "--config"])
        .arg(&cfg)
        .env("GHMCP_FIXTURES", &fx)
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
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let r: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == 2)
        .expect("réponse tools/call");
    let c = &r["result"]["content"];
    assert_eq!(c[0]["type"], "text", "{r}");
    assert!(
        c[0]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("successfully downloaded text file (SHA: 0635b4b)"),
        "{r}"
    );
    assert_eq!(c[1]["type"], "resource", "{r}");
    assert_eq!(
        c[1]["resource"]["uri"],
        "repo://acme/demo/sha/abc/contents/README.md"
    );
    assert_eq!(c[1]["resource"]["text"], "# Titre\n");
}
