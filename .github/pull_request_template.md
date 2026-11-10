## Description

<!-- Ce que change cette PR et pourquoi. -->

## Vérifications

- [ ] `cargo test --workspace` passe
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` et `cargo fmt --check` passent
- [ ] `bash scripts/scan-donnees.sh` ne sort rien (aucune donnée réelle : IP, jeton, chemin, nom)
- [ ] Un test rejoue le cas corrigé ou ajouté
- [ ] Si un outil change : `skill/github-mcp-rust/SKILL.md` et le README sont à jour
