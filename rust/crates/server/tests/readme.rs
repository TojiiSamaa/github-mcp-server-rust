//! README.md : le bloc stats reprend bench/results.json (régénérer avec `node bench/table.mjs`),
//! chaque outil de `tools/list` y est cité, et l'image d'aperçu ouvre le fichier.

use std::collections::BTreeSet;

fn lire(rel: &str) -> String {
    let p = format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Premier nombre d'une cellule du tableau ("822.9 ms" -> 822.9).
fn nombre(cell: &str) -> f64 {
    let n: String = cell
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    n.parse()
        .unwrap_or_else(|_| panic!("pas de nombre dans {cell:?}"))
}

fn cellules<'a>(bloc: &'a str, debut: &str) -> Vec<&'a str> {
    let l = bloc
        .lines()
        .find(|l| l.starts_with(debut))
        .unwrap_or_else(|| panic!("ligne absente : {debut}"));
    l.split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect()
}

#[test]
fn stats_egales_a_results_json() {
    let readme = lire("README.md");
    let (b, e) = (
        readme.find("<!-- stats:debut -->").expect("début"),
        readme.find("<!-- stats:fin -->").expect("fin"),
    );
    let bloc = &readme[b..e];
    assert!(!bloc.contains("TBD"), "cellule TBD restante");
    let r: serde_json::Value = serde_json::from_str(&lire("bench/results.json")).unwrap();
    let arrondi = |v: f64| (v * 10.0).round() / 10.0;
    // Cellule Go = « défaut / --toolsets all » (sauf taille du binaire, identique).
    for (ligne, cle, div) in [
        ("| Startup time", "startup_ms_median", 1.0),
        ("| Memory at idle", "rss_idle_bytes", 1e6),
        ("| Binary size", "binary_bytes", 1e6),
        ("| Tools in", "tools", 1.0),
    ] {
        let c = cellules(bloc, ligne);
        let go: Vec<&str> = c[1].split(" / ").collect();
        let lectures = [
            ("go", go[0]),
            ("go_all", go.get(1).copied().unwrap_or(go[0])),
            ("rust", c[2]),
        ];
        for (col, cell) in lectures {
            let attendu = r[col][cle].as_f64().unwrap() / div;
            let lu = nombre(cell);
            let ok = if cle == "tools" {
                lu == attendu
            } else {
                lu == arrondi(attendu)
            };
            assert!(ok, "{ligne} {col}: README {lu} != results.json {attendu}");
        }
    }
}

#[test]
fn readme_cite_chaque_outil_et_ouvre_sur_l_image() {
    let readme = lire("README.md");
    let tools: BTreeSet<String> = ghmcp::registry().iter().map(|t| t.name.clone()).collect();
    let manquants: Vec<_> = tools
        .iter()
        .filter(|t| !readme.contains(&format!("`{t}`")))
        .collect();
    assert!(
        manquants.is_empty(),
        "outils absents du README : {manquants:?}"
    );
    let premiere = readme.lines().find(|l| !l.trim().is_empty()).unwrap();
    assert!(
        premiere.contains("assets/discord-apercu.png"),
        "1re ligne : {premiere}"
    );
}
