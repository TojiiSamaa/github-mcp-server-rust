//! Définition d'un outil, métadonnées de permissions, contexte d'appel.

use crate::config::Config;
use crate::error::{GhError, Result};
use crate::event::Bus;
use crate::net::Gh;
use serde::Serialize;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type Handler = Arc<dyn Fn(Ctx, Value) -> BoxFuture<Result<Value>> + Send + Sync>;
/// Demande de confirmation à l'humain (élicitation MCP) : Ok(true) accepté, Ok(false) refusé, Err(Timeout)…
pub type Elicit = Arc<dyn Fn(String) -> BoxFuture<Result<bool>> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Access {
    Read,
    Write,
}

/// Niveau de jeton nécessaire (jetons par rôle : lecture, dev, cicd, securite, admin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Level {
    Lecture,
    Dev,
    Cicd,
    Securite,
    Admin,
}

pub struct Tool {
    pub name: String,
    pub description: String,
    /// Schéma propre à l'outil, SANS `account` (ajouté par `published_schema`).
    pub input_schema: Value,
    pub annotations: Value,
    pub classic_scopes: Vec<&'static str>,
    pub fine_perms: Vec<(&'static str, Access)>,
    pub app_events: Vec<&'static str>,
    pub level: Level,
    pub needs_approval: bool,
    pub handler: Handler,
}

/// Métadonnées sérialisables d'un outil (sans handler) : `ctx.registry()`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolMeta {
    pub name: String,
    pub description: String,
    pub annotations: Value,
    pub classic_scopes: Vec<&'static str>,
    pub fine_perms: Vec<(&'static str, Access)>,
    pub app_events: Vec<&'static str>,
    pub level: Level,
    pub needs_approval: bool,
    pub official: bool,
}

fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    Arc::new(move |c, v| Box::pin(f(c, v)))
}

impl Tool {
    /// Outil officiel : description, schéma et annotations repris du snap `name`.
    /// Panique si `name` n'est pas un outil officiel (erreur de programmation, vue par les tests).
    pub fn official<F, Fut>(name: &str, f: F) -> Tool
    where
        F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        let snap =
            crate::schema::snap(name).unwrap_or_else(|| panic!("outil officiel inconnu: {name}"));
        let annotations = snap
            .get("annotations")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let read_only = annotations
            .get("readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Tool {
            name: name.into(),
            description: snap
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            input_schema: snap
                .get("inputSchema")
                .cloned()
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            annotations,
            classic_scopes: vec![],
            fine_perms: vec![],
            app_events: vec![],
            level: if read_only {
                Level::Lecture
            } else {
                Level::Dev
            },
            needs_approval: false,
            handler: handler(f),
        }
    }

    /// Outil étendu (nom préfixé `gh_`). `input_schema` = objet JSON Schema (`type: object`).
    /// Par défaut : lecture seule, Level::Lecture ; changer avec les méthodes ci-dessous.
    pub fn new<F, Fut>(name: &str, description: &str, input_schema: Value, f: F) -> Tool
    where
        F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Tool {
            name: name.into(),
            description: description.into(),
            input_schema,
            annotations: json!({ "readOnlyHint": true }),
            classic_scopes: vec![],
            fine_perms: vec![],
            app_events: vec![],
            level: Level::Lecture,
            needs_approval: false,
            handler: handler(f),
        }
    }

    pub fn scopes(mut self, s: &[&'static str]) -> Self {
        self.classic_scopes = s.to_vec();
        self
    }
    pub fn perm(mut self, p: &'static str, a: Access) -> Self {
        self.fine_perms.push((p, a));
        self
    }
    pub fn events(mut self, e: &[&'static str]) -> Self {
        self.app_events = e.to_vec();
        self
    }
    pub fn level(mut self, l: Level) -> Self {
        self.level = l;
        self
    }
    /// Marque l'outil « à validation » : son handler DOIT appeler `ctx.confirm` avant d'écrire.
    pub fn approval(mut self) -> Self {
        self.needs_approval = true;
        self
    }
    pub fn annotations(mut self, a: Value) -> Self {
        self.annotations = a;
        self
    }

    pub fn is_official(&self) -> bool {
        crate::schema::snap(&self.name).is_some()
    }

    /// Schéma publié dans tools/list : celui de l'outil + `account` optionnel.
    pub fn published_schema(&self) -> Value {
        let mut s = self.input_schema.clone();
        if !s.is_object() {
            s = json!({"type": "object"});
        }
        if !s.get("properties").is_some_and(Value::is_object) {
            s["properties"] = json!({});
        }
        s["properties"]["account"] = json!({
            "type": "string",
            "description": "Configured GitHub account to use (omit for the default account)."
        });
        s
    }

    pub fn meta(&self) -> ToolMeta {
        ToolMeta {
            name: self.name.clone(),
            description: self.description.clone(),
            annotations: self.annotations.clone(),
            classic_scopes: self.classic_scopes.clone(),
            fine_perms: self.fine_perms.clone(),
            app_events: self.app_events.clone(),
            level: self.level,
            needs_approval: self.needs_approval,
            official: self.is_official(),
        }
    }
}

/// Contexte d'un appel : clonable, bon marché.
#[derive(Clone)]
pub struct Ctx {
    config: Arc<Config>,
    bus: Bus,
    registry: Arc<OnceLock<Vec<ToolMeta>>>,
    elicit: Option<Elicit>,
}

impl Ctx {
    pub fn new(config: Arc<Config>, bus: Bus) -> Ctx {
        Ctx {
            config,
            bus,
            registry: Arc::new(OnceLock::new()),
            elicit: None,
        }
    }
    /// Posé une fois par le serveur après assemblage du registre.
    pub fn set_registry(&self, metas: Vec<ToolMeta>) {
        let _ = self.registry.set(metas);
    }
    /// Copie du contexte pour un appel dont le client sait répondre à une élicitation.
    pub fn with_elicit(&self, e: Option<Elicit>) -> Ctx {
        Ctx {
            elicit: e,
            ..self.clone()
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn config_arc(&self) -> Arc<Config> {
        self.config.clone()
    }
    pub fn bus(&self) -> &Bus {
        &self.bus
    }
    /// Métadonnées de TOUS les outils du serveur (crate permissions).
    pub fn registry(&self) -> &[ToolMeta] {
        self.registry.get().map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn test_mode(&self) -> bool {
        crate::net::is_test()
    }

    /// Client GitHub du compte `args.account` (absent = défaut). Ne lit aucun secret ici.
    pub fn gh(&self, args: &Value) -> Result<Gh> {
        let name = args
            .get("account")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        Ok(Gh::new(self.config.account(name)?.clone()))
    }

    /// À appeler par tout outil `needs_approval` AVANT l'écriture. Échoue fermé.
    /// Réel : élicitation MCP si le client la supporte, sinon `ConfirmationRequired`.
    /// Test : `MCP_TEST_APPROVER=approve|deny|timeout|absent` (défaut deny).
    pub async fn confirm(&self, args: &Value, message: &str) -> Result<()> {
        let account = args
            .get("account")
            .and_then(Value::as_str)
            .unwrap_or("défaut");
        let msg = format!("{message} (compte: {account})");
        let r = if crate::net::is_test() {
            match crate::net::test_approver() {
                crate::net::Approver::Approve => Ok(()),
                crate::net::Approver::Deny => Err(GhError::Denied(msg)),
                crate::net::Approver::Timeout => Err(GhError::Timeout(msg)),
                crate::net::Approver::Absent => Err(GhError::ConfirmationRequired(msg)),
            }
        } else {
            match &self.elicit {
                None => Err(GhError::ConfirmationRequired(format!(
                    "{msg} : client sans élicitation MCP"
                ))),
                Some(e) => match e(msg.clone()).await {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(GhError::Denied(msg)),
                    Err(err) => Err(err),
                },
            }
        };
        crate::stats::global().approval(&r);
        r
    }
}

/// Raccourcis de lecture d'arguments.
pub fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}
pub fn req_str<'a>(args: &'a Value, k: &str) -> Result<&'a str> {
    arg_str(args, k).ok_or_else(|| GhError::InvalidArgs(format!("paramètre requis manquant: {k}")))
}
pub fn arg_i64(args: &Value, k: &str) -> Option<i64> {
    args.get(k).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str()?.parse().ok())
    })
}
pub fn arg_bool(args: &Value, k: &str) -> Option<bool> {
    args.get(k).and_then(Value::as_bool)
}
