//! Masque les jetons GitHub dans tout texte qui sort (erreurs, logs).

const PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some((i, p)) = PREFIXES
        .iter()
        .filter_map(|p| rest.find(p).map(|i| (i, *p)))
        .min_by_key(|(i, _)| *i)
    {
        out.push_str(&rest[..i]);
        out.push_str(p);
        out.push_str("***");
        let tail = &rest[i + p.len()..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod t {
    #[test]
    fn masque() {
        assert_eq!(
            super::redact("x ghp_FAKEabc123 y github_pat_FAKE_1"),
            "x ghp_*** y github_pat_***"
        );
    }
}
