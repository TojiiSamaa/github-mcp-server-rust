//! token_cmd hors mode test (processus dédié : `net::init` ne se fixe qu'une fois).
use ghmcp_core::config::AccountCfg;
use ghmcp_core::net::{self, Approver};

const VALEUR: &str = "ghp_FAKEvaleurFictive123"; // scan:ok

fn sh(script_win: &str, script_unix: &str) -> Vec<String> {
    let v: [&str; 3] = if cfg!(windows) {
        ["cmd", "/C", script_win]
    } else {
        ["sh", "-c", script_unix]
    };
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn token_cmd() {
    net::init(false, Approver::Deny);

    // succès : sortie rognée
    let ok = net::token_from_cmd(
        "alice-test",
        &sh(&format!("echo {VALEUR}"), &format!("echo {VALEUR}")),
    )
    .await
    .unwrap();
    assert_eq!(ok, VALEUR);

    // code non nul, avec la valeur sur la sortie standard : jamais dans l'erreur
    let e = net::token_from_cmd(
        "alice-test",
        &sh(
            &format!("echo {VALEUR} & exit 3"),
            &format!("echo {VALEUR}; exit 3"),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "CONFIG");
    assert!(
        !e.to_string().contains(VALEUR) && !format!("{e:?}").contains(VALEUR),
        "{e}"
    );

    // sortie vide
    let e = net::token_from_cmd("alice-test", &sh("echo.", "true"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "CONFIG");
    assert!(e.to_string().contains("rien écrit"), "{e}");

    // liste vide, programme absent
    assert_eq!(
        net::token_from_cmd("alice-test", &[])
            .await
            .unwrap_err()
            .code(),
        "CONFIG"
    );
    assert_eq!(
        net::token_from_cmd("alice-test", &["ghmcp-programme-absent".into()])
            .await
            .unwrap_err()
            .code(),
        "CONFIG"
    );

    // Debug d'AccountCfg : pas d'arguments du token_cmd
    let c: AccountCfg = toml::from_str(&format!(
        "name = \"a\"\ntoken_cmd = [\"prog\", \"{VALEUR}\"]"
    ))
    .unwrap();
    assert!(!format!("{c:?}").contains(VALEUR));
}
