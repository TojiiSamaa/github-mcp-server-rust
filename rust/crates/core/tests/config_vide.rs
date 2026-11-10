//! Bug GITHUB-2611100048-m2u : `GHMCP_CONFIG` défini mais vide/blanc = pas de config explicite.
//! Processus de test dédié (modifie l'environnement).
use ghmcp_core::config::Config;

#[test]
fn ghmcp_config_vide_ou_blanc_retombe_sur_le_defaut() {
    for v in ["", "   "] {
        std::env::set_var("GHMCP_CONFIG", v);
        let c = Config::load(None)
            .unwrap_or_else(|e| panic!("GHMCP_CONFIG={v:?} doit être ignoré: {e}"));
        assert!(!c.accounts.is_empty());
    }
}
