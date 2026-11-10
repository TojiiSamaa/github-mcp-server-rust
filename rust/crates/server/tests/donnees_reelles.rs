//! Aucune donnée réelle dans le dépôt : IP privée, chemin de poste, e-mail réel, jeton, clé privée.
//! Parcourt `git ls-files` (repli : arborescence hors target/.git). Motifs alignés sur scripts/scan-donnees.sh.
use std::path::{Path, PathBuf};
use std::process::Command;

fn racine() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fichiers(root: &Path) -> Vec<PathBuf> {
    let git = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output();
    if let Ok(o) = git.as_ref().map(|o| o.clone()) {
        if o.status.success() && !o.stdout.is_empty() {
            return o
                .stdout
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| root.join(String::from_utf8_lossy(s).as_ref()))
                .collect();
        }
    }
    let mut out = vec![];
    let mut pile = vec![root.to_path_buf()];
    while let Some(d) = pile.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let nom = e.file_name();
            if p.is_dir() {
                if nom != "target" && nom != ".git" {
                    pile.push(p);
                }
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn ip_privee(a: [u32; 4]) -> bool {
    a[0] == 10 || (a[0] == 192 && a[1] == 168) || (a[0] == 172 && (16..=31).contains(&a[1]))
}

/// Découpe sur tout ce qui n'est pas chiffre ou point, cherche a.b.c.d (octets <= 255).
fn ips_privees(ligne: &str) -> Vec<String> {
    ligne
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|t| {
            let p: Vec<u32> = t
                .split('.')
                .map(|x| x.parse().ok().filter(|_| !x.is_empty() && x.len() <= 3))
                .collect::<Option<_>>()?;
            (p.len() == 4 && p.iter().all(|&n| n <= 255) && ip_privee([p[0], p[1], p[2], p[3]]))
                .then(|| t.to_string())
        })
        .collect()
}

fn chemin_poste(ligne: &str) -> bool {
    let l = ligne.to_ascii_lowercase();
    // antislashs (simples ou doublés) ramenés à « / »
    if l.replace("\\\\", "/")
        .replace('\\', "/")
        .contains(":/users/")
    {
        return true;
    }
    let suivi = |motif: &str, ok: &dyn Fn(&str) -> bool| {
        l.match_indices(motif)
            .any(|(i, _)| ok(&ligne[i + motif.len()..]))
    };
    let nom = |s: &str| {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>()
    };
    suivi("/home/", &|s| {
        let n = nom(s);
        n.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && !["user", "exemple", "example", "runner"].contains(&n.as_str())
    }) || suivi("/users/", &|s| {
        s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    })
}

fn emails_reels(ligne: &str) -> Vec<String> {
    let mut v = vec![];
    for (i, _) in ligne.match_indices('@') {
        let loc: String = ligne[..i]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || "._%+-".contains(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let dom: String = ligne[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || ".-".contains(*c))
            .collect();
        let dom = dom.trim_end_matches(['.', '-']).to_ascii_lowercase();
        let tld_ok = dom
            .rsplit_once('.')
            .is_some_and(|(_, t)| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()));
        let l = loc.to_ascii_lowercase();
        // exemptions : noreply, domaines d'exemple (RFC 2606), usage SSH `git@`
        let fictif = dom.contains("noreply")
            || l.contains("noreply")
            || dom.starts_with("example.")
            || dom == "example"
            || dom.ends_with(".example")
            || dom.ends_with(".invalid")
            || dom.ends_with(".test")
            || l == "git";
        if !loc.is_empty() && tld_ok && !fictif {
            v.push(format!("{loc}@{dom}"));
        }
    }
    v
}

fn jetons(ligne: &str) -> Vec<String> {
    let mut v = vec![];
    for (pre, min) in [
        ("ghp_", 36),
        ("gho_", 36),
        ("ghs_", 36),
        ("ghu_", 36),
        ("github_pat_", 22),
    ] {
        for (i, _) in ligne.match_indices(pre) {
            let n = ligne[i + pre.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .count();
            if n >= min {
                v.push(format!("{pre}…({n} car.)"));
            }
        }
    }
    let cle = ["-----BEGIN", " PRIVATE KEY"].concat();
    if ligne.contains(&cle[..10]) && ligne.contains(&cle[10..]) {
        v.push("clé privée".into());
    }
    v
}

#[test]
fn aucune_donnee_reelle_dans_le_depot() {
    let root = racine();
    let moi = root.join("crates/server/tests/donnees_reelles.rs");
    let mut fuites = vec![];
    for f in fichiers(&root) {
        if f.canonicalize().ok() == moi.canonicalize().ok() {
            continue;
        }
        let Some(txt) = std::fs::read(&f)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        for (n, l) in txt.lines().enumerate() {
            let mut trouve: Vec<String> = ips_privees(l)
                .into_iter()
                .map(|x| format!("IP privée {x}"))
                .collect();
            if chemin_poste(l) {
                trouve.push("chemin de poste".into());
            }
            trouve.extend(emails_reels(l).into_iter().map(|e| format!("e-mail {e}")));
            trouve.extend(jetons(l).into_iter().map(|t| format!("jeton {t}")));
            fuites.extend(trouve.into_iter().map(|t| {
                format!(
                    "{}:{}: {t}",
                    f.strip_prefix(&root).unwrap_or(&f).display(),
                    n + 1
                )
            }));
        }
    }
    assert!(
        fuites.is_empty(),
        "données réelles détectées :\n{}",
        fuites.join("\n")
    );
}

#[test]
fn detecteurs_reconnaissent_le_vrai_et_laissent_le_fictif() {
    // exemples vrais assemblés à l'exécution : ce fichier ne doit pas, lui-même, ressembler à une fuite
    let ip = |a: u8, b: u8, c: u8, d: u8| format!("{a}.{b}.{c}.{d}");
    let (p1, p2, p3) = (ip(192, 168, 1, 20), ip(10, 20, 30, 40), ip(172, 20, 0, 1));
    assert_eq!(ips_privees(&format!("a {p1} b")), [p1.as_str()]);
    assert_eq!(ips_privees(&format!("{p2}:80")), [p2.as_str()]);
    assert_eq!(ips_privees(&p3), [p3.as_str()]);
    let publiques = [
        ip(127, 0, 0, 1),
        ip(192, 0, 2, 1),
        ip(172, 32, 0, 1),
        ip(203, 0, 113, 9),
        "1.2.3".into(),
    ];
    assert!(ips_privees(&publiques.join(" ")).is_empty());
    let bs = char::from(92u8);
    let (win, win2) = (
        format!("C:{bs}Users{bs}bob{bs}x"),
        format!("C:{bs}{bs}Users{bs}{bs}bob"),
    );
    let (lin, mac) = (format!("/{}/bob/x", "home"), format!("/{}/Bob", "Users"));
    assert!(chemin_poste(&win) && chemin_poste(&win2) && chemin_poste(&lin) && chemin_poste(&mac));
    assert!(
        !chemin_poste(&format!("/{}/user/x", "home"))
            && !chemin_poste(&format!("/{}/exemple/x", "home"))
            && !chemin_poste("/usr/bin")
    );
    let at = '@';
    assert_eq!(
        emails_reels(&format!("écrire à bob{at}societe.fr.")),
        [format!("bob{at}societe.fr")]
    );
    assert!(emails_reels(&format!(
        "a{at}example.com 1+x{at}users.noreply.github.com git{at}github.com:o/r {at}v1 x{at}y"
    ))
    .is_empty());
    assert_eq!(jetons(&format!("{}{}", "ghp_", "a".repeat(36))).len(), 1);
    assert_eq!(
        jetons(&format!("{}{}", "github_pat_", "A1_".repeat(10))).len(),
        1
    );
    assert!(jetons("ghp_FAKEvaleurFictive123").is_empty()); // scan:ok
    assert_eq!(
        jetons(&format!("{} {}", "-----BEGIN", "PRIVATE KEY-----")).len(),
        1
    );
}
