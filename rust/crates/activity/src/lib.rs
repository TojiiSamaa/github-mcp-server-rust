//! ghmcp-activity : suivi d'activité GitHub (magasin SQLite d'`Event`, routes HTTP, outils `gh_activity_*`).
//! Contrat : voir l'en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//! SIGNATURES APPELÉES PAR LE SERVEUR : ne pas les changer, seulement remplir les corps.

use axum::{
    body::Bytes,
    extract::{Path as UrlPath, Query, State as AxState},
    http::{HeaderMap, StatusCode},
    response::{sse, IntoResponse, Response, Sse},
    routing::{get, post},
    Json,
};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use ghmcp_core::{arg_i64, arg_str, Bus, Config, Ctx, Event, GhError, Result, Tool};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use subtle::ConstantTimeEq;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

fn db_err(e: rusqlite::Error) -> GhError {
    GhError::Other(format!("base d'activité: {e}"))
}

/// Filtre du journal (tous les champs optionnels).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Filter {
    pub account: Option<String>,
    pub repo: Option<String>,
    #[serde(alias = "type")]
    pub kind: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
}

/// Magasin des événements et des dépôts/PR suivis (SQLite).
pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY, account TEXT NOT NULL, repo TEXT NOT NULL, kind TEXT NOT NULL, action TEXT NOT NULL,
  actor TEXT NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, at TEXT NOT NULL, payload TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS events_at ON events(at);
CREATE INDEX IF NOT EXISTS events_repo ON events(account, repo, at);
CREATE INDEX IF NOT EXISTS events_kind ON events(kind, at);
CREATE TABLE IF NOT EXISTS watches (
  account TEXT NOT NULL, repo TEXT NOT NULL, pr INTEGER NOT NULL DEFAULT 0, UNIQUE(account, repo, pr));";

fn ts(t: DateTime<Utc>) -> String {
    // Format fixe : l'ordre lexicographique SQLite = l'ordre chronologique.
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Store {
    /// Ouvre (crée) la base à `path` (`:memory:` accepté).
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// Insère l'événement ; `false` si son id existait déjà (livraison rejouée).
    pub fn insert(&self, e: &Event) -> Result<bool> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO events (id,account,repo,kind,action,actor,title,url,at,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![e.id, e.account, e.repo, e.kind, e.action, e.actor, e.title, e.url, ts(e.at), e.payload.to_string()],
            )
            .map_err(db_err)?;
        Ok(n > 0)
    }

    /// Événements filtrés, du plus récent au plus ancien (limite 100 par défaut, 1000 max).
    pub fn query(&self, f: &Filter) -> Result<Vec<Event>> {
        let (since, until) = (f.since.map(ts), f.until.map(ts));
        let limit = f.limit.unwrap_or(100).clamp(1, 1000);
        let conn = self.conn.lock().unwrap();
        let mut st = conn
            .prepare(
                "SELECT id,account,repo,kind,action,actor,title,url,at,payload FROM events
                 WHERE (?1 IS NULL OR account=?1) AND (?2 IS NULL OR repo=?2) AND (?3 IS NULL OR kind=?3)
                   AND (?4 IS NULL OR at>=?4) AND (?5 IS NULL OR at<=?5)
                 ORDER BY at DESC, id LIMIT ?6",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(
                params![f.account, f.repo, f.kind, since, until, limit],
                |r| {
                    let at: String = r.get(8)?;
                    let payload: String = r.get(9)?;
                    Ok(Event {
                        id: r.get(0)?,
                        account: r.get(1)?,
                        repo: r.get(2)?,
                        kind: r.get(3)?,
                        action: r.get(4)?,
                        actor: r.get(5)?,
                        title: r.get(6)?,
                        url: r.get(7)?,
                        at: at.parse().unwrap_or_default(),
                        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                    })
                },
            )
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Suit un dépôt (`pr` = None) ou une PR ; idempotent.
    pub fn watch_add(&self, account: &str, repo: &str, pr: Option<i64>) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO watches (account,repo,pr) VALUES (?1,?2,?3)",
                params![account, repo, pr.unwrap_or(0)],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// `true` si une entrée a été retirée.
    pub fn watch_remove(&self, account: &str, repo: &str, pr: Option<i64>) -> Result<bool> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM watches WHERE account=?1 AND repo=?2 AND pr=?3",
                params![account, repo, pr.unwrap_or(0)],
            )
            .map_err(db_err)?;
        Ok(n > 0)
    }

    pub fn watch_list(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn
            .prepare("SELECT account,repo,pr FROM watches WHERE (?1 IS NULL OR account=?1) ORDER BY account,repo,pr")
            .map_err(db_err)?;
        let rows = st
            .query_map(params![account], |r| {
                let pr: i64 = r.get(2)?;
                Ok(json!({"account": r.get::<_, String>(0)?, "repo": r.get::<_, String>(1)?, "pr": if pr == 0 { Value::Null } else { json!(pr) }}))
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }
}

/// État partagé des routes HTTP d'activité (webhook entrant, relevé, règles…).
#[derive(Clone)]
pub struct State {
    pub store: Arc<Store>,
    pub bus: Bus,
    pub config: Arc<Config>,
}

/// Magasin vu par les outils MCP (`tools()` n'a pas d'état) ; posé par `router`.
static STORE: OnceLock<Arc<Store>> = OnceLock::new();

fn webhook_str(c: &Config, key: &str) -> Option<String> {
    c.webhook.as_ref()?.get(key)?.as_str().map(String::from)
}

/// Valeur de l'env nommée par `webhook.<key>` ; vide ou absente = None.
fn webhook_env(c: &Config, key: &str) -> Option<String> {
    std::env::var(webhook_str(c, key)?)
        .ok()
        .filter(|v| !v.is_empty())
}

/// `x-hub-signature-256: sha256=<hex>` contre le HMAC du corps brut, à temps constant. (scan:ok)
pub fn verify_signature(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(sig) = header
        .and_then(|h| h.strip_prefix("sha256="))
        .and_then(|h| hex::decode(h).ok())
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

fn s<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or("")
}

/// Normalise une livraison GitHub (`X-GitHub-Event` + payload) en `Event`.
pub fn normalize(account: &str, id: String, kind: &str, p: Value) -> Event {
    let first = |ptrs: &[&str]| {
        ptrs.iter()
            .map(|q| s(&p, q))
            .find(|v| !v.is_empty())
            .unwrap_or("")
            .to_string()
    };
    Event {
        id,
        account: account.into(),
        repo: s(&p, "/repository/full_name").into(),
        kind: kind.into(),
        action: s(&p, "/action").into(),
        actor: first(&["/sender/login", "/pusher/name"]),
        title: first(&[
            "/pull_request/title",
            "/issue/title",
            "/workflow_run/name",
            "/check_run/name",
            "/release/name",
            "/head_commit/message",
            "/alert/security_advisory/summary",
            "/deployment_status/description",
        ]),
        url: first(&[
            "/pull_request/html_url",
            "/comment/html_url",
            "/issue/html_url",
            "/workflow_run/html_url",
            "/release/html_url",
            "/check_run/html_url",
            "/alert/html_url",
            "/compare",
            "/repository/html_url",
        ]),
        at: Utc::now(),
        payload: p,
    }
}

async fn webhook(
    state: &State,
    account: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let account = match state.config.account(account.as_deref()) {
        Ok(a) => a.name.clone(),
        Err(_) => return (StatusCode::NOT_FOUND, "compte inconnu").into_response(),
    };
    // Fermé par défaut : sans secret configuré on refuse tout.
    let Some(secret) = webhook_env(&state.config, "secret_env") else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "secret webhook non configuré",
        )
            .into_response();
    };
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    if !verify_signature(secret.as_bytes(), &body, h("x-hub-signature-256")) {
        return (StatusCode::UNAUTHORIZED, "signature invalide").into_response();
    }
    let Some(kind) = h("x-github-event") else {
        return (StatusCode::BAD_REQUEST, "X-GitHub-Event manquant").into_response();
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "JSON invalide").into_response();
    };
    let id = h("x-github-delivery")
        .map(String::from)
        .unwrap_or_else(|| hex::encode(<Sha256 as sha2::Digest>::digest(&body)));
    let ev = normalize(&account, id, kind, payload);
    match state.store.insert(&ev) {
        Ok(true) => {
            let _ = state.bus.send(ev); // aucun abonné = Err, sans importance
            StatusCode::ACCEPTED.into_response()
        }
        Ok(false) => StatusCode::OK.into_response(), // livraison déjà reçue
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "erreur interne").into_response(),
    }
}

/// Jeton d'API configuré (`webhook.api_token_env` nommant une variable non vide).
pub fn api_token_set(c: &Config) -> bool {
    webhook_env(c, "api_token_env").is_some()
}

/// `Authorization: Bearer <jeton>` comparé à temps constant ; sans jeton configuré, toujours vrai.
pub fn bearer_ok(c: &Config, headers: &HeaderMap) -> bool {
    let Some(tok) = webhook_env(c, "api_token_env") else {
        return true;
    };
    let got = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    bool::from(got.as_bytes().ct_eq(tok.as_bytes()))
}

/// `webhook.api_token_env` défini et non vide = jeton Bearer exigé sur /api/* (et /mcp, voir le binaire) ; sinon ouvert.
#[allow(clippy::result_large_err)] // Err = la réponse HTTP prête à renvoyer (401), appelée une fois par requête
fn api_auth(state: &State, headers: &HeaderMap) -> std::result::Result<(), Response> {
    if bearer_ok(&state.config, headers) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "jeton requis").into_response())
    }
}

fn json_or_500<T: serde::Serialize>(r: Result<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "erreur interne").into_response(),
    }
}

#[derive(Deserialize)]
struct WatchQ {
    account: Option<String>,
}

/// Routes montées par `ghmcp serve` à côté de `/mcp`.
pub fn router(state: State) -> axum::Router {
    let _ = STORE.set(state.store.clone());
    axum::Router::new()
        .route(
            "/webhook",
            post(
                |AxState(st): AxState<State>, h: HeaderMap, b: Bytes| async move {
                    webhook(&st, None, h, b).await
                },
            ),
        )
        .route(
            "/webhook/{account}",
            post(
                |AxState(st): AxState<State>,
                 UrlPath(a): UrlPath<String>,
                 h: HeaderMap,
                 b: Bytes| async move { webhook(&st, Some(a), h, b).await },
            ),
        )
        .route(
            "/api/events",
            get(
                |AxState(st): AxState<State>, h: HeaderMap, Query(f): Query<Filter>| async move {
                    match api_auth(&st, &h) {
                        Ok(()) => json_or_500(st.store.query(&f)),
                        Err(r) => r,
                    }
                },
            ),
        )
        .route(
            "/api/watches",
            get(
                |AxState(st): AxState<State>, h: HeaderMap, Query(q): Query<WatchQ>| async move {
                    match api_auth(&st, &h) {
                        Ok(()) => json_or_500(st.store.watch_list(q.account.as_deref())),
                        Err(r) => r,
                    }
                },
            ),
        )
        .route(
            "/api/stream",
            get(|AxState(st): AxState<State>, h: HeaderMap| async move {
                if let Err(r) = api_auth(&st, &h) {
                    return r;
                }
                let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(|m| async move {
                    match m {
                        Ok(ev) => Some(Ok::<_, std::convert::Infallible>(
                            sse::Event::default()
                                .event(ev.kind.clone())
                                .id(ev.id.clone())
                                .json_data(&ev)
                                .unwrap_or_default(),
                        )),
                        Err(BroadcastStreamRecvError::Lagged(_)) => None,
                    }
                });
                Sse::new(stream)
                    .keep_alive(sse::KeepAlive::default())
                    .into_response()
            }),
        )
        .with_state(state)
}

fn store() -> Result<&'static Arc<Store>> {
    STORE
        .get()
        .ok_or_else(|| GhError::Config("journal d'activité inactif : lancer `ghmcp serve`".into()))
}

fn parse_time(args: &Value, k: &str) -> Result<Option<DateTime<Utc>>> {
    arg_str(args, k)
        .map(|v| {
            v.parse().map_err(|_| {
                GhError::InvalidArgs(format!(
                    "{k}: date RFC 3339 attendue (2026-01-31T12:00:00Z)"
                ))
            })
        })
        .transpose()
}

async fn activity_feed(ctx: Ctx, args: Value) -> Result<Value> {
    let f = Filter {
        account: arg_str(&args, "account")
            .map(|_| ctx.gh(&args).map(|g| g.account_name().to_string()))
            .transpose()?,
        repo: arg_str(&args, "repo").map(String::from),
        kind: arg_str(&args, "type").map(String::from),
        since: parse_time(&args, "since")?,
        until: parse_time(&args, "until")?,
        limit: arg_i64(&args, "limit").map(|n| n.clamp(1, 1000) as u32),
    };
    Ok(json!({ "events": store()?.query(&f)? }))
}

async fn watch(ctx: Ctx, args: Value) -> Result<Value> {
    let account = ctx.gh(&args)?.account_name().to_string();
    let action = arg_str(&args, "action").unwrap_or("list");
    if action == "list" {
        return Ok(json!({ "watches": store()?.watch_list(Some(&account))? }));
    }
    let repo = arg_str(&args, "repo")
        .filter(|r| r.split('/').count() == 2 && !r.contains(' '))
        .ok_or_else(|| GhError::InvalidArgs("repo: `owner/repo` attendu".into()))?;
    let pr = arg_i64(&args, "pr").filter(|n| *n > 0);
    match action {
        "add" => store()?
            .watch_add(&account, repo, pr)
            .map(|_| json!({"watching": {"account": account, "repo": repo, "pr": pr}})),
        "remove" => store()?
            .watch_remove(&account, repo, pr)
            .map(|r| json!({"removed": r})),
        o => Err(GhError::InvalidArgs(format!(
            "action: add, remove ou list attendu (reçu {o})"
        ))),
    }
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "gh_activity_feed",
            "Journal d'activité GitHub (webhooks reçus), du plus récent au plus ancien, filtrable par compte, dépôt, type, dates.",
            json!({"type":"object","properties":{
                "repo":{"type":"string","description":"owner/repo"},
                "type":{"type":"string","description":"pull_request, workflow_run, issue_comment, release…"},
                "since":{"type":"string","description":"RFC 3339"},"until":{"type":"string","description":"RFC 3339"},
                "limit":{"type":"integer","description":"1-1000, défaut 100"}}}),
            activity_feed,
        ),
        Tool::new(
            "gh_watch",
            "Dépôts ou PR suivis : action add / remove / list (repo=owner/repo, pr optionnel).",
            json!({"type":"object","properties":{
                "action":{"type":"string","enum":["add","remove","list"]},
                "repo":{"type":"string","description":"owner/repo"},
                "pr":{"type":"integer","description":"numéro de PR ; absent = tout le dépôt"}},"required":["action"]}),
            watch,
        )
        .annotations(json!({"readOnlyHint": false, "destructiveHint": false})),
    ]
}
