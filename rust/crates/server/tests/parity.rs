//! Parité avec github/github-mcp-server (snaps de schemas/official).
//!
//! - `parity_full` : ROUGE tant que les crates d'outils ne sont pas remplies (c'est attendu) :
//!   tools/list doit contenir tous les noms officiels distincts, et chaque outil porté doit avoir
//!   les mêmes `properties` (hors `account`) et le même `required` que son snap de référence.
//! - `parity_report` : toujours vert, imprime manquants / écarts / OK (`-- --nocapture`).

use ghmcp_core::schema::{official_names, snap};
use serde_json::Value;
use std::collections::BTreeSet;

fn keys(v: &Value, ptr: &str) -> BTreeSet<String> {
    v.pointer(ptr)
        .and_then(Value::as_object)
        .map(|o| o.keys().filter(|k| *k != "account").cloned().collect())
        .unwrap_or_default()
}
fn required(v: &Value, ptr: &str) -> BTreeSet<String> {
    v.pointer(ptr)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// (manquants, écarts, ok)
fn compare() -> (Vec<String>, Vec<String>, Vec<String>) {
    let tools = ghmcp::registry();
    let listed: Vec<Value> = tools.iter().map(ghmcp::tool_json).collect();
    let (mut missing, mut diffs, mut ok) = (vec![], vec![], vec![]);
    for name in official_names() {
        let Some(t) = listed.iter().find(|t| t["name"] == name.as_str()) else {
            missing.push(name);
            continue;
        };
        let s = snap(&name).expect("snap");
        let (pt, ps) = (
            keys(t, "/inputSchema/properties"),
            keys(&s, "/inputSchema/properties"),
        );
        let (rt, rs) = (
            required(t, "/inputSchema/required"),
            required(&s, "/inputSchema/required"),
        );
        if pt != ps || rt != rs {
            diffs.push(format!(
                "{name}: properties {pt:?} vs {ps:?}, required {rt:?} vs {rs:?}"
            ));
        } else {
            ok.push(name);
        }
    }
    (missing, diffs, ok)
}

#[test]
fn parity_report() {
    let (missing, diffs, ok) = compare();
    println!(
        "PARITÉ : {} officiels, {} OK, {} écarts, {} manquants",
        official_names().len(),
        ok.len(),
        diffs.len(),
        missing.len()
    );
    for d in &diffs {
        println!("ÉCART {d}");
    }
    println!("MANQUANTS ({}): {}", missing.len(), missing.join(", "));
}

#[test]
fn parity_full() {
    let (missing, diffs, _) = compare();
    assert!(
        missing.is_empty() && diffs.is_empty(),
        "manquants ({}): {}\nécarts: {:#?}",
        missing.len(),
        missing.join(", "),
        diffs
    );
}

#[test]
fn noms_uniques_et_prefixes() {
    let tools = ghmcp::registry();
    let mut seen = BTreeSet::new();
    for t in &tools {
        assert!(seen.insert(t.name.clone()), "doublon: {}", t.name);
        assert!(
            t.is_official() || t.name.starts_with("gh_"),
            "outil étendu sans préfixe gh_: {}",
            t.name
        );
    }
}
