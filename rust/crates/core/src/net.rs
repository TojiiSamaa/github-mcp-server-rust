//! EGRESS UNIQUE : seul module du projet qui construit un client HTTP, envoie une requête ou lance
//! une commande locale. En mode test, le vrai client n'est JAMAIS construit : l'API GitHub répond
//! par une simulation (ou une fixture), toute autre sortie est coupée (`TEST_MODE_EGRESS_BLOCKED`).

use crate::config::{AccountCfg, Kind};
use crate::error::{GhError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Approbateur simulé du mode test (`MCP_TEST_APPROVER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approver {
    Approve,
    Deny,
    Timeout,
    Absent,
}

impl Approver {
    pub fn parse(s: &str) -> Approver {
        match s {
            "approve" => Approver::Approve,
            "timeout" => Approver::Timeout,
            "absent" => Approver::Absent,
            _ => Approver::Deny, // fermé par défaut
        }
    }
}

static MODE: OnceLock<(bool, Approver)> = OnceLock::new();
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// Second client : ne suit jamais les redirections (lecture de `Location`).
static CLIENT_NR: OnceLock<reqwest::Client> = OnceLock::new();
static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

/// Appel du backend simulé (mode test), pour vérifier ce qui serait parti vers GitHub.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub accept: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub fn recorded_calls() -> Vec<Call> {
    CALLS.lock().unwrap().clone()
}

pub fn clear_recorded() {
    CALLS.lock().unwrap().clear();
}

fn record(method: &str, path: &str, accept: Option<&String>, headers: &[(String, String)]) {
    CALLS.lock().unwrap().push(Call {
        method: method.into(),
        path: path.into(),
        accept: accept.cloned(),
        headers: headers.to_vec(),
    });
}

/// Options d'une requête : `Accept` remplaçant le défaut, en-têtes en plus, redirections non suivies.
#[derive(Default, Clone)]
pub struct ReqOpts {
    pub accept: Option<String>,
    pub headers: Vec<(String, String)>,
    pub no_redirect: bool,
}
/// compte -> (jeton d'installation, expiration unix)
static APP_TOKENS: Mutex<Option<HashMap<String, (String, i64)>>> = Mutex::new(None);
/// compte + commande -> jeton rendu par token_cmd. La commande (coffre) coûte ~50 s et échoue lancée
/// en parallèle : un seul lancement par processus, le verrou async fait attendre les appels concurrents.
/// shortcut: verrou global (premières lectures de comptes différents sérialisées), à scinder par compte si gênant.
static CMD_TOKENS: tokio::sync::Mutex<Option<HashMap<String, String>>> =
    tokio::sync::Mutex::const_new(None);

fn cmd_key(account: &str, cmd: &[String]) -> String {
    format!("{account}|{}", cmd.join("|"))
}

/// Jeton en cache pour `key`, sinon `fetch` (lancé une seule fois même sous appels concurrents). Une erreur n'est pas gardée.
async fn cached_cmd_token<F, Fut>(key: String, fetch: F) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let mut g = CMD_TOKENS.lock().await;
    if let Some(t) = g.as_ref().and_then(|m| m.get(&key)) {
        return Ok(t.clone());
    }
    let t = fetch().await?;
    g.get_or_insert_with(HashMap::new).insert(key, t.clone());
    Ok(t)
}

/// Fixé UNE fois au démarrage (main). Un second appel est ignoré : on ne sort pas du mode test en cours de route.
pub fn init(test_mode: bool, approver: Approver) {
    let _ = MODE.set((test_mode, approver));
}

/// Non initialisé = mode test (fermé par défaut).
pub fn is_test() -> bool {
    MODE.get().map(|m| m.0).unwrap_or(true)
}

pub fn test_approver() -> Approver {
    MODE.get().map(|m| m.1).unwrap_or(Approver::Deny)
}

fn client() -> Result<&'static reqwest::Client> {
    client_with(false)
}

fn client_with(no_redirect: bool) -> Result<&'static reqwest::Client> {
    if is_test() {
        return Err(blocked("construction du client HTTP réel"));
    }
    let cell = if no_redirect { &CLIENT_NR } else { &CLIENT };
    if let Some(c) = cell.get() {
        return Ok(c);
    }
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("ghmcp/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60));
    if no_redirect {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    let c = b.build().map_err(|e| GhError::Network(e.to_string()))?;
    Ok(cell.get_or_init(|| c))
}

fn blocked(what: &str) -> GhError {
    crate::stats::global().egress_blocked();
    GhError::TestModeEgressBlocked(format!("mode test : {what} refusé"))
}

/// Réponse brute (texte, statut, en-têtes) : journaux, contenus non JSON.
#[derive(Debug, Clone)]
pub struct Raw {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub text: String,
}

/// Client GitHub d'un compte. Obtenu par `ctx.gh(&args)?`.
#[derive(Debug, Clone)]
pub struct Gh {
    pub account: AccountCfg,
}

impl Gh {
    pub fn new(account: AccountCfg) -> Gh {
        Gh { account }
    }
    pub fn account_name(&self) -> &str {
        &self.account.name
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.into()
        } else {
            format!(
                "{}/{}",
                self.account.api_url.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
    }

    fn graphql_url(&self) -> String {
        let base = self.account.api_url.trim_end_matches('/');
        match base.strip_suffix("/api/v3") {
            Some(host) => format!("{host}/api/graphql"),
            None => format!("{base}/graphql"),
        }
    }

    /// Appel REST JSON. `path` relatif à l'api_url du compte (`/repos/o/r`) ou URL absolue.
    /// Réponse vide (204) = `null`.
    pub async fn rest(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        if is_test() {
            record(method, path, None, &[]);
            return Ok(simulate(
                &self.account.name,
                method,
                path,
                query,
                body.as_ref(),
            ));
        }
        let raw = self
            .send(method, path, query, body, &ReqOpts::default())
            .await?;
        if raw.text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&raw.text)
            .map_err(|e| GhError::Other(format!("réponse non JSON: {e}")))
    }

    /// Comme `rest` mais rend le texte brut (journaux de jobs, diff…) ; suit les redirections.
    pub async fn rest_raw(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Raw> {
        self.rest_raw_with(method, path, query, body, &ReqOpts::default())
            .await
    }

    /// `rest_raw` avec options. `no_redirect` : un 3xx est un succès (statut + en-têtes dont `location`).
    pub async fn rest_raw_with(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        opts: &ReqOpts,
    ) -> Result<Raw> {
        if is_test() {
            record(method, path, opts.accept.as_ref(), &opts.headers);
            let text = simulate(&self.account.name, method, path, query, body.as_ref()).to_string();
            return Ok(if opts.no_redirect {
                let location =
                    format!("https://test-mode.invalid/{}", path.trim_start_matches('/'));
                Raw {
                    status: 302,
                    headers: vec![("location".into(), location)],
                    text,
                }
            } else {
                Raw {
                    status: 200,
                    headers: vec![],
                    text,
                }
            });
        }
        self.send(method, path, query, body, opts).await
    }

    /// GraphQL : rend `data`, ou `GhError::GraphQl(errors)`.
    pub async fn graphql(&self, query: &str, vars: Value) -> Result<Value> {
        self.graphql_with(query, vars, &[]).await
    }

    /// `graphql` avec en-têtes en plus (`GraphQL-Features`…).
    pub async fn graphql_with(
        &self,
        query: &str,
        vars: Value,
        headers: &[(String, String)],
    ) -> Result<Value> {
        if is_test() {
            record("POST", "graphql", None, headers);
            return Ok(
                json!({ "test_mode": true, "account": self.account.name, "graphql": query, "variables": vars }),
            );
        }
        let url = self.graphql_url();
        let opts = ReqOpts {
            headers: headers.to_vec(),
            ..Default::default()
        };
        let raw = self
            .send(
                "POST",
                &url,
                &[],
                Some(json!({ "query": query, "variables": vars })),
                &opts,
            )
            .await?;
        let v: Value = if raw.text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&raw.text)
                .map_err(|e| GhError::Other(format!("réponse non JSON: {e}")))?
        };
        match v.get("errors") {
            Some(e) if !e.is_null() => Err(GhError::GraphQl(e.clone())),
            _ => Ok(v.get("data").cloned().unwrap_or(Value::Null)),
        }
    }

    /// Pagination REST via l'en-tête `Link: rel="next"`. `items_key` : champ tableau de la réponse
    /// (`workflow_runs`, `items`…) ou None si la réponse est elle-même un tableau. `max_pages` >= 1.
    pub async fn paginate(
        &self,
        path: &str,
        query: &[(&str, String)],
        items_key: Option<&str>,
        max_pages: usize,
    ) -> Result<Vec<Value>> {
        let pick = |v: Value| -> Vec<Value> {
            let arr = match items_key {
                Some(k) => v.get(k).cloned().unwrap_or(Value::Null),
                None => v,
            };
            match arr {
                Value::Array(a) => a,
                Value::Null => vec![],
                other => vec![other],
            }
        };
        if is_test() {
            record("GET", path, None, &[]);
            return Ok(pick(simulate(&self.account.name, "GET", path, query, None)));
        }
        let mut q: Vec<(&str, String)> = query.to_vec();
        if !q.iter().any(|(k, _)| *k == "per_page") {
            q.push(("per_page", "100".into()));
        }
        let mut out = vec![];
        let mut next = Some(self.url(path));
        let mut first = true;
        for _ in 0..max_pages.max(1) {
            let Some(url) = next.take() else { break };
            let raw = self
                .send(
                    "GET",
                    &url,
                    if first { &q } else { &[] },
                    None,
                    &ReqOpts::default(),
                )
                .await?;
            first = false;
            let v: Value = serde_json::from_str(&raw.text)
                .map_err(|e| GhError::Other(format!("réponse non JSON: {e}")))?;
            out.extend(pick(v));
            next = raw
                .headers
                .iter()
                .find(|(k, _)| k == "link")
                .and_then(|(_, l)| next_link(l));
        }
        Ok(out)
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        opts: &ReqOpts,
    ) -> Result<Raw> {
        let c = client_with(opts.no_redirect)?;
        let token = self.token().await?;
        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| GhError::InvalidArgs(format!("méthode HTTP invalide: {method}")))?;
        let mut req = c
            .request(m, self.url(path))
            .bearer_auth(token)
            .header(
                "Accept",
                opts.accept
                    .as_deref()
                    .unwrap_or("application/vnd.github+json"),
            )
            .header("X-GitHub-Api-Version", "2022-11-28");
        for (k, v) in &opts.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GhError::Network(crate::redact::redact(&e.to_string())))?;
        let status = resp.status().as_u16();
        if status == 401 {
            // Jeton révoqué ou tourné : la prochaine lecture relance token_cmd.
            if let (Some(cmd), Some(m)) =
                (&self.account.token_cmd, CMD_TOKENS.lock().await.as_mut())
            {
                m.remove(&cmd_key(&self.account.name, cmd));
            }
        }
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let text = resp
            .text()
            .await
            .map_err(|e| GhError::Network(e.to_string()))?;
        if !(200..300).contains(&status) && !(opts.no_redirect && (300..400).contains(&status)) {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from))
                .unwrap_or_else(|| text.chars().take(500).collect());
            return Err(GhError::Http {
                status,
                message: crate::redact::redact(&message),
            });
        }
        Ok(Raw {
            status,
            headers,
            text,
        })
    }

    /// Lit le secret du compte au moment de l'appel réel (jamais en mode test).
    async fn token(&self) -> Result<String> {
        let a = &self.account;
        match a.kind {
            Kind::Classic | Kind::Fine => {
                // token_env non vide > token_file > token_cmd
                let env_set = a
                    .token_env
                    .as_deref()
                    .and_then(|e| std::env::var(e).ok())
                    .is_some_and(|v| !v.trim().is_empty());
                match &a.token_cmd {
                    Some(cmd) if !env_set && a.token_file.is_none() => {
                        cached_cmd_token(cmd_key(&a.name, cmd), || token_from_cmd(&a.name, cmd))
                            .await
                    }
                    _ => read_secret(
                        &a.name,
                        a.token_env.as_deref(),
                        a.token_file.as_ref(),
                        "jeton",
                    ),
                }
            }
            Kind::App => self.installation_token().await,
        }
    }

    async fn installation_token(&self) -> Result<String> {
        let a = &self.account;
        let now = unix_now();
        if let Some((t, exp)) = APP_TOKENS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get(&a.name).cloned())
        {
            if exp - 60 > now {
                return Ok(t);
            }
        }
        let pem = read_secret(
            &a.name,
            a.private_key_env.as_deref(),
            a.private_key_file.as_ref(),
            "clé privée",
        )?;
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).map_err(|e| {
            GhError::Config(format!("compte {} : clé privée invalide: {e}", a.name))
        })?;
        let claims =
            json!({ "iat": now - 60, "exp": now + 540, "iss": a.app_id.unwrap_or(0).to_string() });
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| GhError::Config(format!("JWT: {e}")))?;
        let url = self.url(&format!(
            "/app/installations/{}/access_tokens",
            a.installation_id.unwrap_or(0)
        ));
        let resp = client()?
            .post(url)
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| GhError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| GhError::Network(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(GhError::Http {
                status,
                message: v["message"]
                    .as_str()
                    .unwrap_or("jeton d'installation refusé")
                    .into(),
            });
        }
        let token = v["token"]
            .as_str()
            .ok_or_else(|| GhError::Other("jeton d'installation absent".into()))?
            .to_string();
        let exp = v["expires_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
            .unwrap_or(now + 3000);
        APP_TOKENS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(a.name.clone(), (token.clone(), exp));
        Ok(token)
    }
}

/// Jeton lu depuis une commande locale (via `run_command`, sans shell). Les erreurs ne reprennent
/// JAMAIS la sortie de la commande ; le jeton n'est ni journalisé ni conservé.
pub async fn token_from_cmd(account: &str, cmd: &[String]) -> Result<String> {
    let Some((program, args)) = cmd.split_first() else {
        return Err(GhError::Config(format!(
            "compte {account} : token_cmd vide"
        )));
    };
    let out = match run_command(program, args).await {
        Ok(o) => o,
        Err(e @ GhError::TestModeEgressBlocked(_)) => return Err(e),
        Err(_) => {
            return Err(GhError::Config(format!(
                "compte {account} : token_cmd en échec (programme introuvable ou code non nul)"
            )))
        }
    };
    let t = out.trim();
    if t.is_empty() {
        return Err(GhError::Config(format!(
            "compte {account} : token_cmd n'a rien écrit sur la sortie standard"
        )));
    }
    Ok(t.to_string())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn read_secret(
    account: &str,
    env: Option<&str>,
    file: Option<&PathBuf>,
    what: &str,
) -> Result<String> {
    if let Some(e) = env {
        if let Ok(v) = std::env::var(e) {
            if !v.trim().is_empty() {
                return Ok(v.trim().to_string());
            }
        }
    }
    if let Some(f) = file {
        return std::fs::read_to_string(f)
            .map(|s| s.trim().to_string())
            .map_err(|e| {
                GhError::Config(format!(
                    "compte {account} : {what} illisible ({}): {e}",
                    f.display()
                ))
            });
    }
    Err(GhError::Config(format!(
        "compte {account} : {what} absent (variable {} vide)",
        env.unwrap_or("-")
    )))
}

fn next_link(link: &str) -> Option<String> {
    link.split(',')
        .find(|p| p.contains("rel=\"next\""))
        .and_then(|p| {
            Some(
                p.split(';')
                    .next()?
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
            )
        })
}

fn fixtures_dir() -> PathBuf {
    std::env::var_os("GHMCP_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures")))
}

/// Nom de fixture : `get_repos_o_r.json` pour `GET /repos/o/r` (caractères hors [a-z0-9] -> `_`).
pub fn fixture_name(method: &str, path: &str) -> String {
    let p: String = path
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}_{}.json", method.to_ascii_lowercase(), p)
}

fn simulate(
    account: &str,
    method: &str,
    path: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Value {
    if let Ok(text) = std::fs::read_to_string(fixtures_dir().join(fixture_name(method, path))) {
        if let Ok(mut v) = serde_json::from_str::<Value>(&text) {
            if let Some(o) = v.as_object_mut() {
                o.insert("test_mode".into(), json!(true));
            }
            return v;
        }
    }
    let q: serde_json::Map<String, Value> = query
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    json!({ "test_mode": true, "account": account, "method": method, "path": path, "query": q, "body": body })
}

/// Destinations : POST JSON vers une URL (webhook Discord, Slack…). Coupé en mode test.
pub async fn post_json(url: &str, body: &Value) -> Result<Value> {
    if is_test() {
        return Err(blocked(&format!(
            "POST {}",
            url.split('?').next().unwrap_or("")
        )));
    }
    let resp = client()?
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| GhError::Network(crate::redact::redact(&e.to_string())))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(GhError::Http {
            status,
            message: text.chars().take(300).collect(),
        });
    }
    Ok(serde_json::from_str(&text).unwrap_or(json!({ "status": status })))
}

/// Destinations : commande locale (programme + arguments, sans shell). Coupé en mode test.
pub async fn run_command(program: &str, args: &[String]) -> Result<String> {
    if is_test() {
        return Err(blocked(&format!("commande {program}")));
    }
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| GhError::Other(format!("{program}: {e}")))?;
    if !out.status.success() {
        return Err(cmd_failure(
            program,
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Erreur d'une commande en échec : la sortie d'erreur peut contenir un jeton (URL, en-tête), on la masque.
fn cmd_failure(program: &str, code: Option<i32>, stderr: &str) -> GhError {
    GhError::Other(crate::redact::redact(&format!(
        "{program}: code {code:?}: {stderr}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "ghp_FAKEjetonOrdre999";

    fn compte(env: Option<&str>, file: Option<&PathBuf>, cmd: Option<Vec<String>>) -> Gh {
        Gh::new(AccountCfg {
            name: "alice-test".into(),
            kind: Kind::Classic,
            token_env: env.map(String::from),
            token_file: file.cloned(),
            token_cmd: cmd,
            app_id: None,
            installation_id: None,
            private_key_env: None,
            private_key_file: None,
            api_url: "https://api.invalid".into(),
        })
    }

    /// Mode test (non initialisé) : une commande n'est pas lancée mais refusée, ce qui prouve qu'elle a été choisie.
    fn cmd_choisie(r: &Result<String>) -> bool {
        matches!(r, Err(GhError::TestModeEgressBlocked(_)))
    }

    #[tokio::test]
    async fn token_cmd_lance_une_seule_fois_sous_appels_concurrents() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let key = cmd_key("cache-test", &["prog-fictif".into()]);
        let fetch = || async {
            N.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok::<_, GhError>("ghp_FAKEcache".to_string())
        };
        let r =
            futures::future::join_all((0..8).map(|_| cached_cmd_token(key.clone(), fetch))).await;
        assert!(r.iter().all(|t| t.as_deref().ok() == Some("ghp_FAKEcache")));
        assert_eq!(
            N.load(Ordering::SeqCst),
            1,
            "token_cmd relancé à chaque appel"
        );
        // une erreur n'est pas gardée : l'appel suivant relance
        let k2 = cmd_key("cache-test-err", &["x".into()]);
        assert!(cached_cmd_token(k2.clone(), || async {
            Err::<String, _>(GhError::Config("échec".into()))
        })
        .await
        .is_err());
        assert_eq!(
            cached_cmd_token(k2, || async { Ok::<_, GhError>("ghp_FAKEretry".into()) })
                .await
                .unwrap(),
            "ghp_FAKEretry"
        );
    }

    #[tokio::test]
    async fn ordre_sources_jeton() {
        let f = std::env::temp_dir().join(format!("ghmcp-ordre-{}.tok", std::process::id()));
        std::fs::write(
            &f,
            "ghp_FAKEfichier
",
        )
        .unwrap();
        let cmd = Some(vec!["prog-fictif".to_string()]);
        let (e1, e2) = ("GHMCP_TEST_ORDRE_ENV", "GHMCP_TEST_ORDRE_VIDE");
        std::env::set_var(e1, " ghp_FAKEenv ");
        std::env::set_var(e2, "   ");
        let absent = "GHMCP_TEST_ORDRE_ABSENT";

        // env défini > fichier > cmd
        assert_eq!(
            compte(Some(e1), Some(&f), cmd.clone())
                .token()
                .await
                .unwrap(),
            "ghp_FAKEenv"
        );
        // env défini + cmd, sans fichier : l'env gagne, la commande n'est pas tentée
        assert_eq!(
            compte(Some(e1), None, cmd.clone()).token().await.unwrap(),
            "ghp_FAKEenv"
        );
        // env vide ou absent + fichier + cmd : fichier
        assert_eq!(
            compte(Some(e2), Some(&f), cmd.clone())
                .token()
                .await
                .unwrap(),
            "ghp_FAKEfichier"
        );
        assert_eq!(
            compte(Some(absent), Some(&f), cmd.clone())
                .token()
                .await
                .unwrap(),
            "ghp_FAKEfichier"
        );
        assert_eq!(
            compte(None, Some(&f), cmd.clone()).token().await.unwrap(),
            "ghp_FAKEfichier"
        );
        // seule la commande (ou env vide/absent + cmd) : cmd
        assert!(cmd_choisie(&compte(None, None, cmd.clone()).token().await));
        assert!(cmd_choisie(
            &compte(Some(e2), None, cmd.clone()).token().await
        ));
        assert!(cmd_choisie(&compte(Some(absent), None, cmd).token().await));
        // aucune source : erreur de configuration, pas de commande
        assert_eq!(
            compte(Some(absent), None, None)
                .token()
                .await
                .unwrap_err()
                .code(),
            "CONFIG"
        );

        std::env::remove_var(e1);
        std::env::remove_var(e2);
        let _ = std::fs::remove_file(&f);
    }

    #[tokio::test]
    async fn jeton_jamais_dans_erreurs() {
        let propre = |s: &str| assert!(!s.contains(SECRET), "fuite du jeton: {s}");

        // token_cmd refusé/en échec, jeton en argument : ni Display ni Debug de l'erreur
        let cmd = vec!["prog-fictif".to_string(), "--token".into(), SECRET.into()];
        let e = compte(None, None, Some(cmd.clone()))
            .token()
            .await
            .unwrap_err();
        propre(&e.to_string());
        propre(&format!("{e:?}"));
        for vide in [
            vec![],
            vec!["ghmcp-programme-absent".to_string(), SECRET.into()],
        ] {
            if let Err(e) = token_from_cmd("alice-test", &vide).await {
                propre(&e.to_string());
                propre(&format!("{e:?}"));
            }
        }

        // source absente / fichier illisible : le message nomme la variable ou le chemin, jamais une valeur
        let e = compte(Some("GHMCP_TEST_ORDRE_ABSENT2"), None, None)
            .token()
            .await
            .unwrap_err();
        propre(&e.to_string());
        let inconnu = PathBuf::from("ghmcp-fichier-absent.tok");
        propre(
            &compte(None, Some(&inconnu), None)
                .token()
                .await
                .unwrap_err()
                .to_string(),
        );

        // code non nul avec le jeton sur stderr : masqué (Display et Debug)
        let e = cmd_failure(
            "git",
            Some(128),
            &format!("fatal: https://x:{SECRET}@github.invalid/o/r"),
        );
        propre(&e.to_string());
        propre(&format!("{e:?}"));
        assert!(e.to_string().contains("ghp_***"), "{e}");

        // Debug de la config et du client : les arguments de token_cmd sont masqués
        let g = compte(None, None, Some(cmd));
        propre(&format!("{g:?}"));
        propre(&format!("{:?}", g.account));

        // ce que redact produit pour les logs
        propre(&crate::redact::redact(&format!(
            "Authorization: token {SECRET} ; erreur {SECRET}"
        )));
    }
}
