//! `ghmcp gen-skill [FICHIER]` : réécrit le bloc généré de skill/github-mcp-rust/SKILL.md depuis le vrai
//! registre. Le reste du fichier (sections rédigées à la main) est conservé tel quel.

use ghmcp_core::{Access, Level, Tool};
use serde_json::{json, Value};

pub const BEGIN: &str = "<!-- BEGIN GENERATED TOOLS -->";
pub const END: &str = "<!-- END GENERATED TOOLS -->";

fn cell(s: &str) -> String {
    s.replace('|', r"\|").replace('\n', " ")
}

fn role(t: &Tool) -> String {
    let d = t.description.trim();
    let first = d.split_inclusive(". ").next().unwrap_or(d).trim();
    let cut: String = first.chars().take(110).collect();
    cell(
        if first.chars().count() > 110 {
            format!("{cut}…")
        } else {
            first.to_string()
        }
        .as_str(),
    )
}

fn params(schema: &Value) -> String {
    let req: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let names: Vec<String> = schema["properties"]
        .as_object()
        .map(|o| {
            o.keys()
                .filter(|k| *k != "account")
                .map(|k| {
                    if req.contains(&k.as_str()) {
                        format!("`{k}`*")
                    } else {
                        format!("`{k}`")
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        "—".into()
    } else {
        names.join(", ")
    }
}

fn level(l: Level) -> &'static str {
    match l {
        Level::Lecture => "lecture",
        Level::Dev => "dev",
        Level::Cicd => "cicd",
        Level::Securite => "securite",
        Level::Admin => "admin",
    }
}

/// Exemple d'appel : seulement les paramètres requis, valeurs factices tirées du schéma.
fn example(t: &Tool) -> String {
    let s = t.published_schema();
    let mut args = serde_json::Map::new();
    for k in s["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let p = &s["properties"][k];
        let v = match (p["enum"].get(0), p["type"].as_str()) {
            (Some(e), _) => e.clone(),
            (_, Some("integer" | "number")) => json!(1),
            (_, Some("boolean")) => json!(false),
            (_, Some("array")) => json!([]),
            (_, Some("object")) => json!({}),
            _ => json!(match k {
                "owner" => "exemple-org",
                "repo" => "exemple-repo",
                _ => "valeur",
            }),
        };
        args.insert(k.into(), v);
    }
    json!({ "name": t.name, "arguments": args }).to_string()
}

pub fn render(tools: &[Tool]) -> String {
    let mut o = format!("{BEGIN}\n<!-- Généré par `ghmcp gen-skill` : ne pas éditer à la main. {} outils. `*` = paramètre requis ; `account` (optionnel) existe sur tous. -->\n\n", tools.len());
    o += "| Outil | Rôle | Paramètres | Niveau | Scope classique | Permission fine | Validation admin |\n|---|---|---|---|---|---|---|\n";
    for t in tools {
        let perms: Vec<String> = t
            .fine_perms
            .iter()
            .map(|(p, a)| format!("{p}:{}", if *a == Access::Write { "write" } else { "read" }))
            .collect();
        let j = |v: Vec<String>| {
            if v.is_empty() {
                "—".to_string()
            } else {
                cell(&v.join(", "))
            }
        };
        o += &format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            t.name,
            role(t),
            params(&t.published_schema()),
            level(t.level),
            j(t.classic_scopes.iter().map(|s| s.to_string()).collect()),
            j(perms),
            if t.needs_approval { "oui" } else { "non" }
        );
    }
    o += "\n### Exemples d'appel (`tools/call`, paramètres requis seulement)\n\n";
    for t in tools {
        o += &format!("- `{}` : `{}`\n", t.name, example(t));
    }
    o + END
}

/// Remplace le bloc entre marqueurs dans `text`.
pub fn splice(text: &str, block: &str) -> Result<String, String> {
    let (b, e) = (
        text.find(BEGIN).ok_or("marqueur de début absent")?,
        text.find(END).ok_or("marqueur de fin absent")?,
    );
    if e < b {
        return Err("marqueurs inversés".into());
    }
    Ok(format!("{}{}{}", &text[..b], block, &text[e + END.len()..]))
}

pub fn run(path: &str) -> Result<usize, String> {
    let tools = ghmcp::registry();
    let old = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    std::fs::write(path, splice(&old, &render(&tools))?).map_err(|e| e.to_string())?;
    Ok(tools.len())
}
