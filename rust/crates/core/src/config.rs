//! Configuration multi-comptes (TOML). Aucun secret n'est stocké ici : seulement OÙ le lire
//! (`token_env`, `token_file`…) ; la lecture se fait au moment d'un vrai appel (`net`), jamais en mode test.

use crate::error::{GhError, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_API: &str = "https://api.github.com";

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Classic,
    Fine,
    App,
}

#[derive(Clone, Deserialize, PartialEq)]
pub struct AccountCfg {
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    pub token_env: Option<String>,
    pub token_file: Option<PathBuf>,
    /// Programme + arguments (sans shell) dont la sortie standard est le jeton (`["bw", "get", "password", "x"]`).
    pub token_cmd: Option<Vec<String>>,
    pub app_id: Option<u64>,
    pub installation_id: Option<u64>,
    pub private_key_env: Option<String>,
    pub private_key_file: Option<PathBuf>,
    #[serde(default = "default_api")]
    pub api_url: String,
}

// Debug maison : les arguments de token_cmd peuvent être sensibles, on ne montre que le programme.
impl std::fmt::Debug for AccountCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCfg")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("token_env", &self.token_env)
            .field("token_file", &self.token_file)
            .field("token_cmd", &self.token_cmd.as_ref().map(|c| c.first()))
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .field("private_key_env", &self.private_key_env)
            .field("private_key_file", &self.private_key_file)
            .field("api_url", &self.api_url)
            .finish()
    }
}

fn default_api() -> String {
    DEFAULT_API.into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<AccountCfg>,
    /// Sections brutes réservées : ext-hooks / activity (`webhook`), activity (`poller`),
    /// automation (`rules`, `destinations`, `actions`). Chaque crate les désérialise elle-même.
    pub webhook: Option<toml::Value>,
    pub poller: Option<toml::Value>,
    /// Règles inline. Fichier séparé : `rules_file` (chemin relatif au dossier du config.toml) ou
    /// variable `GHMCP_RULES` (prioritaire sur la clé ; vide = ignorée). Le fichier contient des
    /// `[[rules]]` (et optionnellement `[destinations.*]` / `[actions]`, repris seulement s'ils
    /// manquent dans la config). Si `rules` est aussi défini inline : règles inline PUIS celles du fichier.
    pub rules: Option<toml::Value>,
    pub rules_file: Option<PathBuf>,
    pub destinations: Option<toml::Value>,
    pub actions: Option<toml::Value>,
    /// activity : envoi des issues des dépôts suivis à l'agent responsable via l'API du Centre.
    pub dispatch: Option<toml::Value>,
    /// serveur : `mode = "borne"` (défaut) ou `"libre"` ; voir crates/server/src/ecriture.rs.
    pub ecriture: Option<toml::Value>,
    /// permissions : `ghmcp_connecter` / `ghmcp_autotest` (Centre, propriétaire, dépôt de test) ; voir crates/permissions/src/connecter.rs.
    pub connecter: Option<toml::Value>,
    /// Fichier d'où vient la config (None = défauts).
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config> {
        let mut c: Config = toml::from_str(text).map_err(|e| GhError::Config(e.to_string()))?;
        if c.accounts.is_empty() {
            // Compatible avec le serveur officiel : un seul compte lu dans GITHUB_PERSONAL_ACCESS_TOKEN.
            c.accounts.push(AccountCfg {
                name: "default".into(),
                kind: Kind::Classic,
                token_env: Some("GITHUB_PERSONAL_ACCESS_TOKEN".into()),
                token_file: None,
                token_cmd: None,
                app_id: None,
                installation_id: None,
                private_key_env: None,
                private_key_file: None,
                api_url: std::env::var("GITHUB_API_URL").unwrap_or_else(|_| default_api()),
            });
        }
        for a in &c.accounts {
            if a.kind == Kind::App && (a.app_id.is_none() || a.installation_id.is_none()) {
                return Err(GhError::Config(format!(
                    "compte {} : kind = app exige app_id et installation_id",
                    a.name
                )));
            }
        }
        Ok(c)
    }

    /// `path` explicite (`--config`) > `GHMCP_CONFIG` > `default_path()`. Fichier absent = défauts
    /// (sauf chemin explicite : erreur).
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let explicit = path.map(PathBuf::from).or_else(|| {
            std::env::var_os("GHMCP_CONFIG")
                .filter(|v| !v.to_string_lossy().trim().is_empty())
                .map(PathBuf::from)
        });
        let p = explicit.clone().or_else(default_path);
        let text = match &p {
            Some(p) if p.exists() => std::fs::read_to_string(p)
                .map_err(|e| GhError::Config(format!("{}: {e}", p.display())))?,
            Some(p) if explicit.is_some() => {
                return Err(GhError::Config(format!(
                    "fichier de config introuvable: {}",
                    p.display()
                )))
            }
            _ => String::new(),
        };
        let mut c = Config::from_toml(&text)?;
        c.path = p.filter(|p| p.exists());
        c.load_rules_file()?;
        Ok(c)
    }

    /// Fusionne le fichier de règles (voir le champ `rules`). Fichier indiqué mais absent = erreur.
    fn load_rules_file(&mut self) -> Result<()> {
        let env = std::env::var_os("GHMCP_RULES")
            .filter(|v| !v.to_string_lossy().trim().is_empty())
            .map(PathBuf::from);
        let base = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let Some(file) = env.or_else(|| self.rules_file.as_ref().map(|f| base.join(f))) else {
            return Ok(());
        };
        let text = std::fs::read_to_string(&file)
            .map_err(|e| GhError::Config(format!("fichier de règles {}: {e}", file.display())))?;
        let mut v: toml::Table = toml::from_str(&text)
            .map_err(|e| GhError::Config(format!("fichier de règles {}: {e}", file.display())))?;
        if let Some(extra) = v.remove("rules") {
            match (&mut self.rules, extra) {
                (Some(toml::Value::Array(a)), toml::Value::Array(b)) => a.extend(b),
                (slot, extra) => *slot = Some(extra),
            }
        }
        for (key, slot) in [
            ("destinations", &mut self.destinations),
            ("actions", &mut self.actions),
        ] {
            if slot.is_none() {
                *slot = v.remove(key);
            }
        }
        Ok(())
    }

    pub fn account_names(&self) -> Vec<String> {
        self.accounts.iter().map(|a| a.name.clone()).collect()
    }

    /// `None` = compte par défaut (`default_account`, sinon le premier).
    pub fn account(&self, name: Option<&str>) -> Result<&AccountCfg> {
        let wanted = name.or(self.default_account.as_deref());
        let found = match wanted {
            Some(n) => self.accounts.iter().find(|a| a.name == n),
            None => self.accounts.first(),
        };
        found.ok_or_else(|| GhError::UnknownAccount {
            name: wanted.unwrap_or("").into(),
            known: self.account_names(),
        })
    }
}

/// Dossier de config utilisateur, hors dépôt : `%APPDATA%\ghmcp\config.toml` ou `$XDG_CONFIG_HOME|~/.config/ghmcp/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("ghmcp").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wfae08d1-t5-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn nb(c: &Config) -> usize {
        c.rules
            .as_ref()
            .and_then(|r| r.as_array())
            .map_or(0, |a| a.len())
    }

    #[test]
    fn rules_file_relatif_charge_et_concatene() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = dir("rel");
        std::fs::write(
            d.join("r.toml"),
            "[[rules]]
name = \"b\"
[[rules]]
name = \"c\"
",
        )
        .unwrap();
        std::fs::write(
            d.join("config.toml"),
            "rules_file = \"r.toml\"
[[rules]]
name = \"a\"
",
        )
        .unwrap();
        let c = Config::load(Some(&d.join("config.toml"))).unwrap();
        assert_eq!(nb(&c), 3);
        assert_eq!(c.rules.unwrap()[0]["name"].as_str(), Some("a"));
    }

    #[test]
    fn ghmcp_rules_prioritaire_sur_la_cle() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = dir("env");
        std::fs::write(
            d.join("cle.toml"),
            "[[rules]]
name = \"cle\"
",
        )
        .unwrap();
        std::fs::write(
            d.join("env.toml"),
            "[[rules]]
name = \"e1\"
[[rules]]
name = \"e2\"
",
        )
        .unwrap();
        std::fs::write(
            d.join("config.toml"),
            "rules_file = \"cle.toml\"
",
        )
        .unwrap();
        std::env::set_var("GHMCP_RULES", d.join("env.toml"));
        let c = Config::load(Some(&d.join("config.toml")));
        std::env::remove_var("GHMCP_RULES");
        assert_eq!(nb(&c.unwrap()), 2);
    }

    #[test]
    fn rules_file_absent_est_une_erreur() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = dir("abs");
        std::fs::write(
            d.join("config.toml"),
            "rules_file = \"nope.toml\"
",
        )
        .unwrap();
        let e = Config::load(Some(&d.join("config.toml"))).unwrap_err();
        assert!(
            matches!(e, GhError::Config(ref m) if m.contains("nope.toml")),
            "{e:?}"
        );
    }
}
