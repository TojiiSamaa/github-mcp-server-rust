//! Registre agent ↔ projet ↔ commit ↔ PR ↔ issue ↔ déploiement, rempli par le récepteur de webhooks.
//! L'agent est lu dans le trailer `Agent-Id: <id>` (commit, corps de PR/issue, description de déploiement)
//! ou, à défaut, déduit du commit déployé / de la tête de PR. Le trailer n'est pas une preuve d'identité.

use super::{db_err, s, ts, Store};
use ghmcp_core::{Event, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS reg_commits (
  repo TEXT NOT NULL, sha TEXT NOT NULL, agent_id TEXT, auteur TEXT NOT NULL, message TEXT NOT NULL,
  at TEXT NOT NULL, PRIMARY KEY (repo, sha));
CREATE INDEX IF NOT EXISTS reg_commits_agent ON reg_commits(agent_id);
CREATE TABLE IF NOT EXISTS reg_prs (
  repo TEXT NOT NULL, numero INTEGER NOT NULL, agent_id TEXT, branche TEXT NOT NULL, etat TEXT NOT NULL,
  head_sha TEXT NOT NULL, titre TEXT NOT NULL, url TEXT NOT NULL, PRIMARY KEY (repo, numero));
CREATE INDEX IF NOT EXISTS reg_prs_sha ON reg_prs(repo, head_sha);
CREATE TABLE IF NOT EXISTS reg_issues (
  repo TEXT NOT NULL, numero INTEGER NOT NULL, agent_id TEXT, ouvert_par TEXT NOT NULL, etat TEXT NOT NULL,
  titre TEXT NOT NULL, url TEXT NOT NULL, PRIMARY KEY (repo, numero));
CREATE TABLE IF NOT EXISTS reg_deploiements (
  repo TEXT NOT NULL, id INTEGER NOT NULL, env TEXT NOT NULL, sha TEXT NOT NULL, agent_id TEXT,
  statut TEXT NOT NULL, par TEXT NOT NULL, at TEXT NOT NULL, url TEXT NOT NULL, PRIMARY KEY (repo, id));
CREATE INDEX IF NOT EXISTS reg_dep_env ON reg_deploiements(repo, env, at);
CREATE TABLE IF NOT EXISTS reg_dispatch (
  delivery_id TEXT PRIMARY KEY, repo TEXT NOT NULL, numero INTEGER NOT NULL, agent_id TEXT,
  route TEXT NOT NULL, statut TEXT NOT NULL, at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS reg_dispatch_issue ON reg_dispatch(repo, numero, at);";

/// `Agent-Id: <id>` (clé insensible à la casse). Plusieurs valeurs différentes = ambigu → None.
/// Un id hors `[A-Za-z0-9._~:/-]{1,100}` est ignoré (texte de tiers, pas un identifiant).
pub fn agent_id(text: &str) -> Option<String> {
    let mut trouve: Option<String> = None;
    for l in text.lines() {
        let l = l.trim().trim_start_matches(['>', ' ']);
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        if !k.trim().eq_ignore_ascii_case("agent-id") {
            continue;
        }
        let v = v.trim();
        let ok = !v.is_empty()
            && v.len() <= 100
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._~:/-".contains(c));
        if !ok {
            continue;
        }
        match &trouve {
            Some(t) if t != v => return None,
            _ => trouve = Some(v.to_string()),
        }
    }
    trouve
}

/// Auteur membre du dépôt (association fournie par GitHub) : son trailer `Agent-Id` est cru.
fn fiable(v: &Value) -> bool {
    matches!(
        s(v, "/author_association"),
        "OWNER" | "MEMBER" | "COLLABORATOR"
    )
}

fn label_agent(p: &Value, ptr: &str) -> Option<String> {
    p.pointer(ptr)?
        .as_array()?
        .iter()
        .filter_map(|l| l["name"].as_str()?.strip_prefix("agent:"))
        .map(str::trim)
        .find(|v| !v.is_empty())
        .map(String::from)
}

fn i(p: &Value, ptr: &str) -> i64 {
    p.pointer(ptr).and_then(Value::as_i64).unwrap_or(0)
}

impl Store {
    /// Met à jour le registre à partir d'un événement reçu (push, pull_request, issues, deployment*).
    pub fn enregistrer(&self, ev: &Event) -> Result<()> {
        let (p, repo) = (&ev.payload, ev.repo.as_str());
        if repo.is_empty() {
            return Ok(());
        }
        let c = self.conn.lock().unwrap();
        match ev.kind.as_str() {
            "push" => {
                for cm in p["commits"].as_array().into_iter().flatten() {
                    let msg = cm["message"].as_str().unwrap_or("");
                    let auteur = [s(cm, "/author/username"), s(cm, "/author/name")]
                        .into_iter()
                        .find(|v| !v.is_empty())
                        .unwrap_or("");
                    c.execute(
                        "INSERT INTO reg_commits (repo,sha,agent_id,auteur,message,at) VALUES (?1,?2,?3,?4,?5,?6)
                         ON CONFLICT(repo,sha) DO UPDATE SET agent_id=COALESCE(excluded.agent_id, agent_id)",
                        params![repo, s(cm, "/id"), agent_id(msg), auteur, msg.lines().next().unwrap_or(""),
                                s(cm, "/timestamp")],
                    )
                    .map_err(db_err)?;
                }
            }
            "pull_request" => {
                let pr = &p["pull_request"];
                let head = s(pr, "/head/sha");
                // trailer du corps cru seulement d'un membre du dépôt (un tiers ne choisit pas la session qui le lit)
                let agent = fiable(pr)
                    .then(|| agent_id(s(pr, "/body")))
                    .flatten()
                    .or_else(|| {
                        c.query_row(
                            "SELECT agent_id FROM reg_commits WHERE repo=?1 AND sha=?2",
                            params![repo, head],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .optional()
                        .ok()
                        .flatten()
                        .flatten()
                    });
                let etat = if pr["merged"].as_bool() == Some(true) {
                    "merged"
                } else {
                    s(pr, "/state")
                };
                c.execute(
                    "INSERT INTO reg_prs (repo,numero,agent_id,branche,etat,head_sha,titre,url) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT(repo,numero) DO UPDATE SET agent_id=COALESCE(agent_id, excluded.agent_id),
                       branche=excluded.branche, etat=excluded.etat, head_sha=excluded.head_sha, titre=excluded.titre, url=excluded.url",
                    params![repo, i(pr, "/number"), agent, s(pr, "/head/ref"), etat, head, s(pr, "/title"), s(pr, "/html_url")],
                )
                .map_err(db_err)?;
            }
            "issues" | "issue_comment" => {
                let is = &p["issue"];
                // label `agent:<id>` (posé par un trieur) : prioritaire ; trailer du corps : membre seulement,
                // et ne remplace jamais un agent déjà fixé
                let label = label_agent(is, "/labels");
                let corps = (ev.kind == "issues" && fiable(is))
                    .then(|| agent_id(s(is, "/body")))
                    .flatten();
                let (agent, fort) = match label {
                    Some(l) => (Some(l), true),
                    None => (corps, false),
                };
                c.execute(
                    "INSERT INTO reg_issues (repo,numero,agent_id,ouvert_par,etat,titre,url) VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(repo,numero) DO UPDATE SET
                       agent_id=CASE WHEN ?8 THEN COALESCE(excluded.agent_id, agent_id) ELSE COALESCE(agent_id, excluded.agent_id) END,
                       etat=excluded.etat, titre=excluded.titre",
                    params![repo, i(is, "/number"), agent, s(is, "/user/login"), s(is, "/state"), s(is, "/title"), s(is, "/html_url"), fort],
                )
                .map_err(db_err)?;
            }
            "deployment" | "deployment_status" => {
                let d = &p["deployment"];
                let sha = s(d, "/sha");
                let agent = agent_id(s(d, "/description"))
                    .or_else(|| {
                        d.pointer("/payload/agent_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                    })
                    .or_else(|| {
                        c.query_row(
                            "SELECT agent_id FROM reg_commits WHERE repo=?1 AND sha=?2",
                            params![repo, sha],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .optional()
                        .ok()
                        .flatten()
                        .flatten()
                    });
                let (statut, url) = if ev.kind == "deployment" {
                    ("created", s(d, "/url"))
                } else {
                    (
                        s(p, "/deployment_status/state"),
                        [
                            s(p, "/deployment_status/environment_url"),
                            s(p, "/deployment_status/target_url"),
                        ]
                        .into_iter()
                        .find(|v| !v.is_empty())
                        .unwrap_or(""),
                    )
                };
                c.execute(
                    "INSERT INTO reg_deploiements (repo,id,env,sha,agent_id,statut,par,at,url) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(repo,id) DO UPDATE SET agent_id=COALESCE(excluded.agent_id, agent_id),
                       statut=excluded.statut, at=excluded.at, url=CASE WHEN excluded.url='' THEN url ELSE excluded.url END",
                    params![repo, i(d, "/id"), s(d, "/environment"), sha, agent, statut, s(d, "/creator/login"), ts(ev.at), url],
                )
                .map_err(db_err)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Agent responsable d'une issue (ou d'une PR, l'issue d'une PR ayant le même numéro).
    pub fn agent_de(&self, repo: &str, numero: i64) -> Option<String> {
        let c = self.conn.lock().unwrap();
        let q = |sql: &str| {
            c.query_row(sql, params![repo, numero], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .ok()
            .flatten()
            .flatten()
        };
        q("SELECT agent_id FROM reg_issues WHERE repo=?1 AND numero=?2")
            .or_else(|| q("SELECT agent_id FROM reg_prs WHERE repo=?1 AND numero=?2"))
    }

    /// Fixe l'agent d'une issue (session lancée par le dispatch) s'il n'en a pas encore.
    pub fn fixer_agent_issue(&self, repo: &str, numero: i64, agent: &str) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE reg_issues SET agent_id=COALESCE(agent_id, ?3) WHERE repo=?1 AND numero=?2",
                params![repo, numero, agent],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Note un envoi au Centre ; `false` si cette livraison a déjà été traitée.
    pub fn noter_dispatch(
        &self,
        id: &str,
        repo: &str,
        numero: i64,
        agent: Option<&str>,
        route: &str,
        statut: &str,
    ) -> Result<bool> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO reg_dispatch (delivery_id,repo,numero,agent_id,route,statut,at) VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(delivery_id) DO UPDATE SET agent_id=excluded.agent_id, route=excluded.route, statut=excluded.statut
                 WHERE statut='en_cours'",
                params![id, repo, numero, agent, route, statut, ts(chrono::Utc::now())],
            )
            .map_err(db_err)?;
        Ok(n > 0)
    }

    /// Envois de la dernière heure sur tout le dépôt (garde contre une rafale d'issues).
    pub fn dispatches_recents_depot(&self, repo: &str) -> usize {
        let depuis = ts(chrono::Utc::now() - chrono::Duration::hours(1));
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM reg_dispatch WHERE repo=?1 AND at>=?2",
                params![repo, depuis],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
    }

    /// Envois de la dernière heure pour une issue (garde anti-boucle).
    pub fn dispatches_recents(&self, repo: &str, numero: i64) -> usize {
        let depuis = ts(chrono::Utc::now() - chrono::Duration::hours(1));
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM reg_dispatch WHERE repo=?1 AND numero=?2 AND at>=?3",
                params![repo, numero, depuis],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
    }

    /// Qui a déployé quoi : derniers déploiements (20 max), avec commit, agent et PR liés.
    pub fn qui_a_deploye(&self, repo: &str, env: Option<&str>) -> Result<Vec<Value>> {
        let c = self.conn.lock().unwrap();
        let mut st = c
            .prepare(
                "SELECT d.id, d.env, d.sha, COALESCE(d.agent_id, cm.agent_id), d.statut, d.par, d.at, d.url,
                        cm.message, cm.auteur, (SELECT numero FROM reg_prs p WHERE p.repo=d.repo AND p.head_sha=d.sha)
                 FROM reg_deploiements d LEFT JOIN reg_commits cm ON cm.repo=d.repo AND cm.sha=d.sha
                 WHERE d.repo=?1 AND (?2 IS NULL OR d.env=?2) ORDER BY d.at DESC, d.id DESC LIMIT 20",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![repo, env], |r| {
                Ok(json!({
                    "deploiement": r.get::<_, i64>(0)?, "env": r.get::<_, String>(1)?, "sha": r.get::<_, String>(2)?,
                    "agent_id": r.get::<_, Option<String>>(3)?, "statut": r.get::<_, String>(4)?,
                    "declenche_par": r.get::<_, String>(5)?, "at": r.get::<_, String>(6)?, "url": r.get::<_, String>(7)?,
                    "commit": r.get::<_, Option<String>>(8)?, "auteur_git": r.get::<_, Option<String>>(9)?,
                    "pr": r.get::<_, Option<i64>>(10)?,
                }))
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Fiche du registre : par agent (tout son travail), par sha, par PR ou par issue.
    pub fn fiche(
        &self,
        repo: Option<&str>,
        agent: Option<&str>,
        sha: Option<&str>,
        numero: Option<i64>,
    ) -> Result<Value> {
        let c = self.conn.lock().unwrap();
        let liste = |sql: &str, cols: &[&str]| -> Result<Vec<Value>> {
            let mut st = c.prepare(sql).map_err(db_err)?;
            let rows = st
                .query_map(params![repo, agent, sha, numero], |r| {
                    let mut o = serde_json::Map::new();
                    for (n, k) in cols.iter().enumerate() {
                        let v: rusqlite::types::Value = r.get(n)?;
                        o.insert(
                            k.to_string(),
                            match v {
                                rusqlite::types::Value::Integer(x) => json!(x),
                                rusqlite::types::Value::Text(t) => json!(t),
                                _ => Value::Null,
                            },
                        );
                    }
                    Ok(Value::Object(o))
                })
                .map_err(db_err)?;
            rows.collect::<std::result::Result<_, _>>().map_err(db_err)
        };
        // ?1 repo, ?2 agent, ?3 sha, ?4 numéro (PR ou issue) ; NULL = pas de filtre.
        const W: &str = "(?1 IS NULL OR repo=?1) AND (?2 IS NULL OR agent_id=?2)";
        Ok(json!({
            "commits": liste(&format!("SELECT repo,sha,agent_id,auteur,message,at FROM reg_commits WHERE {W} AND (?3 IS NULL OR sha=?3) AND ?4 IS NULL ORDER BY at DESC LIMIT 50"),
                             &["repo", "sha", "agent_id", "auteur_git", "message", "at"])?,
            "prs": liste(&format!("SELECT repo,numero,agent_id,branche,etat,head_sha,titre,url FROM reg_prs WHERE {W} AND (?3 IS NULL OR head_sha=?3) AND (?4 IS NULL OR numero=?4) ORDER BY numero DESC LIMIT 50"),
                         &["repo", "numero", "agent_id", "branche", "etat", "head_sha", "titre", "url"])?,
            "issues": liste(&format!("SELECT repo,numero,agent_id,ouvert_par,etat,titre,url FROM reg_issues WHERE {W} AND ?3 IS NULL AND (?4 IS NULL OR numero=?4) ORDER BY numero DESC LIMIT 50"),
                            &["repo", "numero", "agent_id", "ouvert_par", "etat", "titre", "url"])?,
            "deploiements": liste(&format!("SELECT repo,id,env,sha,agent_id,statut,par,at FROM reg_deploiements WHERE {W} AND (?3 IS NULL OR sha=?3) AND ?4 IS NULL ORDER BY at DESC LIMIT 50"),
                                  &["repo", "deploiement", "env", "sha", "agent_id", "statut", "declenche_par", "at"])?,
            "dispatch": liste(&format!("SELECT repo,numero,agent_id,route,statut,at FROM reg_dispatch WHERE {W} AND ?3 IS NULL AND (?4 IS NULL OR numero=?4) ORDER BY at DESC LIMIT 50"),
                              &["repo", "numero", "agent_id", "route", "statut", "at"])?,
        }))
    }
}
