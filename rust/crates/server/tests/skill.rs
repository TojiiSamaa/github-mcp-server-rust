//! Le skill (skill/github-mcp-rust/SKILL.md) doit lister exactement les outils de `tools/list` :
//! aucun manquant, aucun fantôme. Régénérer avec `ghmcp gen-skill`.

use std::collections::BTreeSet;

fn skill() -> String {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skill/github-mcp-rust/SKILL.md"
    );
    std::fs::read_to_string(p).expect("SKILL.md")
}

/// Noms entre accents graves qui ressemblent à un nom d'outil : `gh_...` ou première colonne du tableau.
fn cited(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let (b, e) = (
        text.find("<!-- BEGIN GENERATED TOOLS -->").expect("début"),
        text.find("<!-- END GENERATED TOOLS -->").expect("fin"),
    );
    for line in text[b..e].lines() {
        if let Some(rest) = line.strip_prefix("| `") {
            if let Some(n) = rest.split('`').next() {
                out.insert(n.to_string());
            }
        }
    }
    for line in text.lines() {
        for (i, part) in line.split('`').enumerate() {
            let is_name = part.starts_with("gh_")
                && part.len() > 3
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if i % 2 == 1 && is_name {
                out.insert(part.to_string());
            }
        }
    }
    out
}

#[test]
fn skill_liste_exactement_les_outils() {
    let text = skill();
    let tools: BTreeSet<String> = ghmcp::registry().iter().map(|t| t.name.clone()).collect();
    let cited = cited(&text);
    let manquants: Vec<_> = tools.difference(&cited).collect();
    let fantomes: Vec<_> = cited.difference(&tools).collect();
    assert!(
        manquants.is_empty(),
        "outils absents du SKILL.md : {manquants:?}"
    );
    assert!(
        fantomes.is_empty(),
        "outils fantômes dans le SKILL.md : {fantomes:?}"
    );
    for h in [
        "## Possible",
        "## Impossible / hors périmètre",
        "## Comment faire",
        "## Mode test",
        "## Stats",
    ] {
        assert!(text.contains(h), "section absente : {h}");
    }
    assert!(
        text.starts_with("---\nname: github-mcp-rust\ndescription: "),
        "frontmatter"
    );
}

#[test]
fn detecte_un_fantome() {
    let faux = "<!-- BEGIN GENERATED TOOLS -->
| `gh_outil_qui_nexiste_pas` | x |\nvoir `gh_stats`
<!-- END GENERATED TOOLS -->";
    assert!(cited(faux).contains("gh_outil_qui_nexiste_pas"));
}
