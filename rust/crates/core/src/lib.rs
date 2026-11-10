//! # ghmcp-core — AGENTS-CONTRAT
//!
//! Contrat stable que chaque crate d'outils remplit EN PARALLÈLE. Ne pas changer ces signatures
//! sans passer par l'étage du dessus.
//!
//! ## Règles
//! - Chaque crate expose `pub fn tools() -> Vec<ghmcp_core::Tool>` ; le serveur concatène tout.
//! - Dépendances : uniquement `x.workspace = true` (déjà toutes déclarées dans `[workspace.dependencies]`
//!   et dans le Cargo.toml de votre crate). Ne JAMAIS toucher Cargo.lock ni ajouter de version locale.
//! - **Egress unique** : aucun `reqwest`, `std::process`, `tokio::process` ou socket hors de `core::net`.
//!   GitHub : `ctx.gh(&args)?.rest / rest_raw / graphql / paginate`. Destinations : `net::post_json`,
//!   `net::run_command`. Contrôle : `rg -n "reqwest[:][:]" crates --glob '!crates/core/src/net*'` doit être vide.
//! - **Outils officiels** : `Tool::official("nom_du_snap", handler)` reprend description, schéma et
//!   annotations du snap embarqué (`schemas/official/*.snap`). Le schéma publié = celui du snap + `account`.
//!   Test de parité : `cargo test -p ghmcp --test parity`.
//! - **Outils étendus** : `Tool::new("gh_...", desc, schema, handler)`, préfixe `gh_` obligatoire.
//! - Le paramètre `account` (string, optionnel) est ajouté AUTOMATIQUEMENT à tout schéma : ne pas le déclarer.
//!   `ctx.gh(&args)?` le résout (absent = défaut ; inconnu = `compte inconnu: X (connus: ...)`).
//! - Écritures sensibles : `.approval()` sur l'outil ET `ctx.confirm(&args, "message").await?` dans le
//!   handler AVANT l'appel d'écriture. Réel = élicitation MCP (sinon `ConfirmationRequired`) ;
//!   test = `MCP_TEST_APPROVER=approve|deny|timeout|absent` (défaut deny).
//! - Mode test (`MCP_TEST_MODE=1` / `--test`, au démarrage seulement) : `rest` rend
//!   `{"test_mode":true,"account","method","path","query","body"}` ou la fixture
//!   `crates/core/fixtures/<net::fixture_name(method, path)>` ; aucun secret n'est lu.
//! - Erreurs : `GhError` (`code()` stable). `Err` = résultat `isError: true` côté MCP.
//! - Métadonnées de permissions : `.scopes(&["repo"])`, `.perm("pull_requests", Access::Write)`,
//!   `.events(&["pull_request"])`, `.level(Level::Dev)`. `ctx.registry()` les liste toutes (crate permissions).
//! - Activité : `Event` + `Bus` (`tokio::sync::broadcast`) ; `ctx.bus().send(ev)`.
//! - Config brute réservée : `ctx.config().webhook / poller / rules / destinations / actions` (`toml::Value`).
//!
//! ## Exemple : outil officiel
//! ```no_run
//! use ghmcp_core::{Tool, Ctx, req_str, Result};
//! use serde_json::Value;
//!
//! pub fn tools() -> Vec<Tool> {
//!     vec![Tool::official("get_me", get_me)
//!         .scopes(&[])
//!         .perm("metadata", ghmcp_core::Access::Read)]
//! }
//!
//! async fn get_me(ctx: Ctx, args: Value) -> Result<Value> {
//!     ctx.gh(&args)?.rest("GET", "/user", &[], None).await
//! }
//! # async fn get_file(ctx: Ctx, args: Value) -> Result<Value> {
//! #     let owner = req_str(&args, "owner")?;
//! #     ctx.gh(&args)?.rest("GET", &format!("/repos/{owner}"), &[], None).await
//! # }
//! ```
//!
//! ## Exemple : outil étendu avec validation
//! ```no_run
//! use ghmcp_core::{Tool, Ctx, Access, Level, req_str, Result};
//! use serde_json::{json, Value};
//!
//! pub fn tools() -> Vec<Tool> {
//!     vec![Tool::new(
//!         "gh_repo_delete",
//!         "Delete a repository (irreversible). Requires human approval.",
//!         json!({"type": "object",
//!                "properties": {"owner": {"type": "string"}, "repo": {"type": "string"}},
//!                "required": ["owner", "repo"]}),
//!         repo_delete,
//!     )
//!     .annotations(json!({"readOnlyHint": false, "destructiveHint": true}))
//!     .scopes(&["delete_repo"])
//!     .perm("administration", Access::Write)
//!     .level(Level::Admin)
//!     .approval()]
//! }
//!
//! async fn repo_delete(ctx: Ctx, args: Value) -> Result<Value> {
//!     let (owner, repo) = (req_str(&args, "owner")?, req_str(&args, "repo")?);
//!     let gh = ctx.gh(&args)?; // compte résolu AVANT la confirmation (compte inconnu = erreur claire)
//!     ctx.confirm(&args, &format!("Supprimer définitivement {owner}/{repo} ?")).await?;
//!     gh.rest("DELETE", &format!("/repos/{owner}/{repo}"), &[], None).await?;
//!     Ok(json!({"deleted": format!("{owner}/{repo}")}))
//! }
//! ```

pub mod config;
pub mod error;
pub mod event;
pub mod net;
pub mod redact;
pub mod schema;
mod snaps;
pub mod stats;
pub mod tool;

pub use config::{AccountCfg, Config, Kind};
pub use error::{GhError, Result};
pub use event::{bus, Bus, Event};
pub use net::{clear_recorded, recorded_calls, token_from_cmd, Approver, Call, Gh, Raw, ReqOpts};
pub use tool::{
    arg_bool, arg_i64, arg_str, req_str, Access, BoxFuture, Ctx, Elicit, Handler, Level, Tool,
    ToolMeta,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    const CFG: &str = r#"
default_account = "alice-test"
[[accounts]]
name = "alice-test"
kind = "classic"
token_env = "GHMCP_TEST_ALICE"
[[accounts]]
name = "bob-test"
kind = "fine"
token_file = "bob-token.txt"
api_url = "https://ghe.example.invalid/api/v3"
"#;

    fn ctx() -> Ctx {
        Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus())
    }

    #[test]
    fn compte_par_defaut() {
        let gh = ctx().gh(&json!({})).unwrap();
        assert_eq!(gh.account_name(), "alice-test");
        assert_eq!(gh.account.api_url, config::DEFAULT_API);
    }

    #[test]
    fn compte_choisi_par_account() {
        let gh = ctx().gh(&json!({"account": "bob-test"})).unwrap();
        assert_eq!(gh.account_name(), "bob-test");
        assert_eq!(gh.account.kind, Kind::Fine);
    }

    #[test]
    fn compte_inconnu_erreur_claire() {
        let e = ctx().gh(&json!({"account": "mallory"})).unwrap_err();
        assert_eq!(e.code(), "UNKNOWN_ACCOUNT");
        assert_eq!(
            e.to_string(),
            "compte inconnu: mallory (connus: alice-test, bob-test)"
        );
    }

    #[tokio::test]
    async fn mode_test_simule_sans_lire_de_secret() {
        std::env::set_var("GHMCP_TEST_ALICE", "ghp_FAKE0000000000");
        // non initialisé = mode test (fermé par défaut)
        let v = ctx()
            .gh(&json!({}))
            .unwrap()
            .rest("GET", "/user", &[], None)
            .await
            .unwrap();
        assert_eq!(v["test_mode"], true);
        assert_eq!(v["path"], "/user");
        assert!(!v.to_string().contains("ghp_FAKE"));
        let e = net::post_json("https://hooks.example.invalid/x", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(e.code(), "TEST_MODE_EGRESS_BLOCKED");
        assert!(
            stats::global().snapshot()["egress_blocked"]
                .as_u64()
                .unwrap()
                >= 1
        );
    }

    #[tokio::test]
    async fn confirm_refuse_par_defaut_en_test() {
        let e = ctx().confirm(&json!({}), "écrire ?").await.unwrap_err();
        assert_eq!(e.code(), "DENIED");
    }

    #[test]
    fn snaps_officiels() {
        assert_eq!(snaps::SNAPS.len(), 143);
        let names = schema::official_names();
        assert!(names.contains("list_tags") && names.contains("find_duplicate"));
        // variante ff : le schéma vient du snap *_ff_*
        assert!(!schema::snap("find_duplicate").unwrap()["inputSchema"].is_null());
        // list_tags : le snap list_tags.snap (pas list_tags_output.snap sans inputSchema)
        assert!(!schema::snap("list_tags").unwrap()["inputSchema"].is_null());
        let t = Tool::official("get_me", |_c, _a| async { Ok(json!({})) });
        assert!(t.published_schema()["properties"]["account"].is_object());
    }
}
