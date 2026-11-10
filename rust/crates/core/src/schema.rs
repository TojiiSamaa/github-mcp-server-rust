//! Accès aux 143 snaps officiels embarqués (`snaps.rs`).

use crate::snaps::SNAPS;
use serde_json::Value;
use std::collections::BTreeSet;

fn parse(s: &str) -> Value {
    serde_json::from_str(s).expect("snap JSON valide")
}

/// Schéma de référence de l'outil `name` : le snap dont le fichier == name, sinon une variante
/// (`*_ff_*`…) dont le champ `name` == name et qui a un inputSchema.
pub fn snap(name: &str) -> Option<Value> {
    if let Some((_, s)) = SNAPS.iter().find(|(f, _)| *f == name) {
        return Some(parse(s));
    }
    SNAPS
        .iter()
        .map(|(_, s)| parse(s))
        .find(|v| v["name"] == name && !v["inputSchema"].is_null())
}

/// Noms distincts des outils officiels (champ `name` des snaps).
pub fn official_names() -> BTreeSet<String> {
    SNAPS
        .iter()
        .filter_map(|(_, s)| parse(s)["name"].as_str().map(String::from))
        .collect()
}
