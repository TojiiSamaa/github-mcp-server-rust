//! ghmcp-automation : règles (`[[rules]]`), destinations (`[destinations.<nom>]`) et actions (`[actions]`)
//! déclenchées par les `Event` du bus, relevé périodique (`[poller]`) et outil `gh_pr_dashboard`.
//! Sorties UNIQUEMENT par `ghmcp_core::net::{post_json, run_command}` et `ctx.gh`/`Gh`.
//! Contrat : voir l'en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//! SIGNATURES APPELÉES PAR LE SERVEUR : ne pas les changer, seulement remplir les corps.
//!
//! ```toml
//! [poller]                        # relevé (invitations, notifications, runs en échec des dépôts suivis)
//! interval_secs = 300
//! [destinations.alerte]
//! type = "discord"                # webhook | discord | command ; url ou url_env ; program + args
//! url_env = "DISCORD_HOOK"
//! [[rules]]
//! name = "ci-rouge"
//! on = "ci_failure"               # préréglage ou type d'événement brut (workflow_run, ...)
//! filters = { repo = "monorg/*", branch = "main" }   # action conclusion repo account actor label branch mention
//! to = ["alerte"]
//! actions = ["label:ci-failed"]   # label:X | reviewers:a,b | merge_if_green | accept_invitation
//! [actions]
//! enabled = false                 # défaut : aucune action automatique
//! invitation_allow = ["monorg"]   # liste blanche org ou org/dépôt
//! ```

use ghmcp_activity::Store;
use ghmcp_core::{net, stats, Access, Bus, Config, Ctx, Event, Gh, GhError, Result, Tool};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

// ---------- configuration ----------

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Filters {
    pub action: Option<String>,
    pub conclusion: Option<String>,
    pub repo: Option<String>,
    pub account: Option<String>,
    pub actor: Option<String>,
    pub label: Option<String>,
    pub branch: Option<String>,
    pub mention: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Rule {
    pub name: String,
    pub on: String,
    pub filters: Filters,
    pub to: Vec<String>,
    pub actions: Vec<String>,
}
impl Default for Rule {
    fn default() -> Self {
        Rule {
            name: "règle".into(),
            on: String::new(),
            filters: Filters::default(),
            to: vec![],
            actions: vec![],
        }
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Dest {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: Option<String>,
    pub url_env: Option<String>,
    pub program: Option<String>,
    pub args: Vec<String>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct ActionsCfg {
    pub enabled: bool,
    pub merge_method: String,
    pub invitation_allow: Vec<String>,
}
impl Default for ActionsCfg {
    fn default() -> Self {
        ActionsCfg {
            enabled: false,
            merge_method: "squash".into(),
            invitation_allow: vec![],
        }
    }
}

#[derive(Clone)]
pub struct Auto {
    pub cfg: Arc<Config>,
    pub rules: Vec<Rule>,
    pub dests: HashMap<String, Dest>,
    pub acts: ActionsCfg,
}

fn section<T: serde::de::DeserializeOwned + Default>(v: &Option<toml::Value>) -> Result<T> {
    match v {
        None => Ok(T::default()),
        Some(v) => v
            .clone()
            .try_into()
            .map_err(|e| GhError::Config(format!("automation: {e}"))),
    }
}

impl Auto {
    pub fn from_config(cfg: Arc<Config>) -> Result<Auto> {
        Ok(Auto {
            rules: section(&cfg.rules)?,
            dests: section(&cfg.destinations)?,
            acts: section(&cfg.actions)?,
            cfg,
        })
    }
}

// ---------- appariement des règles ----------

/// Glob minimal : `*` (toute suite) et `?` (un caractère), insensible à la casse.
pub fn glob(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|i| go(&p[1..], &s[i..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    let (p, s): (Vec<char>, Vec<char>) = (
        pat.to_lowercase().chars().collect(),
        s.to_lowercase().chars().collect(),
    );
    go(&p, &s)
}

fn st<'a>(v: &'a Value, ptrs: &[&str]) -> &'a str {
    ptrs.iter()
        .find_map(|p| {
            v.pointer(p)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
}

fn conclusion(ev: &Event) -> &str {
    st(
        &ev.payload,
        &[
            "/workflow_run/conclusion",
            "/check_suite/conclusion",
            "/check_run/conclusion",
            "/conclusion",
        ],
    )
}

fn branch(ev: &Event) -> String {
    let p = &ev.payload;
    let b = st(
        p,
        &[
            "/workflow_run/head_branch",
            "/check_suite/head_branch",
            "/pull_request/head/ref",
            "/ref",
        ],
    );
    b.strip_prefix("refs/heads/").unwrap_or(b).to_string()
}

fn labels(ev: &Event) -> Vec<String> {
    let p = &ev.payload;
    let mut out: Vec<String> = ["/pull_request/labels", "/issue/labels"]
        .iter()
        .filter_map(|q| p.pointer(q).and_then(Value::as_array))
        .flatten()
        .filter_map(|l| l["name"].as_str().map(String::from))
        .collect();
    if let Some(l) = p.pointer("/label/name").and_then(Value::as_str) {
        out.push(l.into());
    }
    out
}

fn body(ev: &Event) -> &str {
    st(
        &ev.payload,
        &[
            "/comment/body",
            "/review/body",
            "/pull_request/body",
            "/issue/body",
        ],
    )
}

/// `@login` présent dans `text` (frontières de mot, insensible à la casse).
pub fn mentions(text: &str, login: &str) -> bool {
    let (t, l) = (text.to_lowercase(), format!("@{}", login.to_lowercase()));
    t.match_indices(&l).any(|(i, _)| {
        let after = t[i + l.len()..].chars().next();
        !after.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
    })
}

/// Préréglage (ou type brut) de `on` contre l'événement.
pub fn preset(on: &str, ev: &Event) -> bool {
    let p = &ev.payload;
    let k = ev.kind.as_str();
    let state = st(p, &["/deployment_status/state"]);
    match on {
        "ci_failure" => {
            matches!(k, "workflow_run" | "check_suite" | "check_run") && conclusion(ev) == "failure"
        }
        "package_failed" | "release_unpublished" => {
            (k == "release" && p.pointer("/release/draft").and_then(Value::as_bool) == Some(true))
                || (matches!(k, "package" | "registry_package" | "release")
                    && matches!(conclusion(ev), "failure" | "cancelled"))
                || (matches!(k, "package" | "registry_package") && ev.action == "failed")
        }
        "deploy_error" => k == "deployment_status" && matches!(state, "failure" | "error"),
        "pr_opened" => k == "pull_request" && ev.action == "opened",
        "pr_review" => k == "pull_request_review",
        "pr_merged" => {
            k == "pull_request"
                && ev.action == "closed"
                && p.pointer("/pull_request/merged").and_then(Value::as_bool) == Some(true)
        }
        "pr" => matches!(
            k,
            "pull_request" | "pull_request_review" | "pull_request_review_comment"
        ),
        "comment" => matches!(
            k,
            "issue_comment" | "commit_comment" | "pull_request_review_comment"
        ),
        "mention" => !body(ev).is_empty() && body(ev).contains('@'),
        "invitation" => {
            matches!(
                k,
                "invitation" | "repository_invitation" | "organization_invitation"
            ) || ev.action == "member_invited"
        }
        "security_alert" => {
            matches!(
                k,
                "dependabot_alert"
                    | "secret_scanning_alert"
                    | "code_scanning_alert"
                    | "repository_vulnerability_alert"
                    | "security_alert"
                    | "security_advisory"
            )
        }
        raw => k == raw,
    }
}

pub fn rule_matches(r: &Rule, ev: &Event) -> bool {
    if !preset(&r.on, ev) {
        return false;
    }
    let f = &r.filters;
    let eq = |want: &Option<String>, got: &str| {
        want.as_deref().is_none_or(|w| w.eq_ignore_ascii_case(got))
    };
    eq(&f.action, &ev.action)
        && eq(&f.conclusion, conclusion(ev))
        && eq(&f.account, &ev.account)
        && eq(&f.actor, &ev.actor)
        && f.repo.as_deref().is_none_or(|g| glob(g, &ev.repo))
        && f.branch.as_deref().is_none_or(|g| glob(g, &branch(ev)))
        && f.label.as_deref().is_none_or(|l| labels(ev).iter().any(|x| x.eq_ignore_ascii_case(l)))
        // `on = "mention"` sans filtre : on cherche le compte de l'événement.
        && match (&f.mention, r.on.as_str()) {
            (Some(m), _) => mentions(body(ev), m),
            (None, "mention") => mentions(body(ev), &ev.account),
            _ => true,
        }
}

// ---------- destinations ----------

fn embed(rule: &str, ev: &Event) -> Value {
    let bad = matches!(conclusion(ev), "failure" | "timed_out")
        || matches!(
            st(&ev.payload, &["/deployment_status/state"]),
            "failure" | "error"
        );
    let mut desc = format!(
        "{} {} sur {}",
        ev.kind,
        ev.action,
        if ev.repo.is_empty() {
            "(sans dépôt)"
        } else {
            &ev.repo
        }
    );
    if !ev.actor.is_empty() {
        desc += &format!(" par {}", ev.actor);
    }
    json!({"embeds": [{
        "title": ev.title.chars().take(250).collect::<String>(),
        "description": desc.chars().take(2000).collect::<String>(),
        "url": if ev.url.is_empty() { Value::Null } else { json!(ev.url) },
        "color": if bad { 0xE74C3C } else { 0x3498DB },
        "timestamp": ev.at.to_rfc3339(),
        "footer": {"text": format!("règle {rule} · compte {}", ev.account)},
    }]})
}

/// Envoie l'événement à une destination. Commande : le JSON de l'événement remplace `{event}` dans les
/// arguments, sinon il est ajouté en dernier argument (shortcut : `net::run_command` n'a pas de stdin).
pub async fn deliver(rule: &str, d: &Dest, ev: &Event) -> Result<()> {
    let url = || {
        d.url
            .clone()
            .or_else(|| d.url_env.as_ref().and_then(|e| std::env::var(e).ok()))
            .filter(|u| !u.is_empty())
            .ok_or_else(|| GhError::Config("destination sans url (url ou url_env)".into()))
    };
    match d.kind.as_str() {
        "webhook" => net::post_json(&url()?, &json!({"rule": rule, "event": ev}))
            .await
            .map(|_| ()),
        "discord" => net::post_json(&url()?, &embed(rule, ev)).await.map(|_| ()),
        "command" => {
            let program = d
                .program
                .as_deref()
                .ok_or_else(|| GhError::Config("destination command sans program".into()))?;
            let j = json!({"rule": rule, "event": ev}).to_string();
            let mut args: Vec<String> = d.args.iter().map(|a| a.replace("{event}", &j)).collect();
            if !d.args.iter().any(|a| a.contains("{event}")) {
                args.push(j);
            }
            net::run_command(program, &args).await.map(|_| ())
        }
        o => Err(GhError::Config(format!(
            "type de destination inconnu: {o:?} (webhook, discord, command)"
        ))),
    }
}

// ---------- actions automatiques ----------

fn pr_number(ev: &Event) -> Option<i64> {
    let p = &ev.payload;
    [
        "/pull_request/number",
        "/issue/number",
        "/number",
        "/workflow_run/pull_requests/0/number",
        "/check_suite/pull_requests/0/number",
    ]
    .iter()
    .find_map(|q| p.pointer(q).and_then(Value::as_i64))
}

/// Liste blanche : `org` ou `org/dépôt`, insensible à la casse. Liste vide = rien d'accepté.
pub fn invitation_allowed(allow: &[String], repo_or_org: &str) -> bool {
    let owner = repo_or_org.split('/').next().unwrap_or("");
    allow
        .iter()
        .any(|a| a.eq_ignore_ascii_case(repo_or_org) || a.eq_ignore_ascii_case(owner))
}

/// (état global, total, en échec, en cours) d'après combined status + check runs.
pub fn ci_summary(status: &Value, checks: &Value) -> (String, u64, u64, u64) {
    let runs = checks["check_runs"].as_array().cloned().unwrap_or_default();
    let (mut failed, mut pending) = (0, 0);
    for r in &runs {
        match (r["status"].as_str(), r["conclusion"].as_str()) {
            (Some("completed"), Some("success" | "neutral" | "skipped")) => {}
            (Some("completed"), _) => failed += 1,
            _ => pending += 1,
        }
    }
    let s = status["state"].as_str().unwrap_or("");
    let no_status = status["total_count"].as_u64() == Some(0);
    failed += matches!(s, "failure" | "error") as u64;
    pending += (s == "pending" && !no_status) as u64;
    let state = if failed > 0 {
        "failure"
    } else if pending > 0 {
        "pending"
    } else if runs.is_empty() && no_status {
        "none"
    } else {
        "success"
    };
    (
        state.into(),
        runs.len() as u64 + status["statuses"].as_array().map_or(0, |a| a.len() as u64),
        failed,
        pending,
    )
}

async fn ci_of(gh: &Gh, repo: &str, sha: &str) -> (String, u64, u64, u64) {
    let (p1, p2) = (
        format!("/repos/{repo}/commits/{sha}/status"),
        format!("/repos/{repo}/commits/{sha}/check-runs"),
    );
    let pp = [("per_page", "100".to_string())];
    let (s, c) = tokio::join!(
        gh.rest("GET", &p1, &[], None),
        gh.rest("GET", &p2, &pp, None)
    );
    ci_summary(&s.unwrap_or(Value::Null), &c.unwrap_or(Value::Null))
}

async fn merge_if_green(gh: &Gh, repo: &str, n: i64, method: &str) -> Result<Value> {
    let pr = gh
        .rest("GET", &format!("/repos/{repo}/pulls/{n}"), &[], None)
        .await?;
    if pr["state"] != "open" || pr["draft"] == true {
        return Err(GhError::Other(format!(
            "{repo}#{n} : fermée ou brouillon, pas de fusion"
        )));
    }
    let sha = pr
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .ok_or_else(|| GhError::Other("sha de tête introuvable".into()))?;
    let (state, total, _, _) = ci_of(gh, repo, sha).await;
    if state != "success" || total == 0 {
        return Err(GhError::Other(format!(
            "{repo}#{n} : CI {state}, pas de fusion"
        )));
    }
    gh.rest(
        "PUT",
        &format!("/repos/{repo}/pulls/{n}/merge"),
        &[],
        Some(json!({"merge_method": method, "sha": sha})),
    )
    .await
}

async fn accept_invitation(gh: &Gh, ev: &Event, allow: &[String]) -> Result<Value> {
    let p = &ev.payload;
    if let Some(id) = p.pointer("/invitation/id").and_then(Value::as_i64) {
        if !invitation_allowed(allow, &ev.repo) {
            return Err(GhError::Denied(format!(
                "invitation {} hors liste blanche",
                ev.repo
            )));
        }
        return gh
            .rest(
                "PATCH",
                &format!("/user/repository_invitations/{id}"),
                &[],
                None,
            )
            .await;
    }
    let org = st(p, &["/organization/login"]);
    if org.is_empty() || !invitation_allowed(allow, org) {
        return Err(GhError::Denied(format!(
            "invitation {org:?} hors liste blanche ou sans identifiant"
        )));
    }
    gh.rest(
        "PATCH",
        &format!("/user/memberships/orgs/{org}"),
        &[],
        Some(json!({"state": "active"})),
    )
    .await
}

async fn run_action(auto: &Auto, spec: &str, ev: &Event) -> Result<Value> {
    if !auto.acts.enabled {
        return Err(GhError::Denied(
            "actions automatiques désactivées ([actions] enabled = false)".into(),
        ));
    }
    let gh = Gh::new(auto.cfg.account(Some(&ev.account))?.clone());
    let (verb, arg) = spec.split_once(':').unwrap_or((spec, ""));
    let need_pr = || {
        pr_number(ev)
            .ok_or_else(|| GhError::Other(format!("{spec}: pas de numéro de PR dans l'événement")))
    };
    match verb {
        "merge_if_green" => {
            merge_if_green(&gh, &ev.repo, need_pr()?, &auto.acts.merge_method).await
        }
        "label" => {
            gh.rest(
                "POST",
                &format!("/repos/{}/issues/{}/labels", ev.repo, need_pr()?),
                &[],
                Some(json!({"labels": [arg]})),
            )
            .await
        }
        "reviewers" => {
            let who: Vec<&str> = arg
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            gh.rest(
                "POST",
                &format!(
                    "/repos/{}/pulls/{}/requested_reviewers",
                    ev.repo,
                    need_pr()?
                ),
                &[],
                Some(json!({"reviewers": who})),
            )
            .await
        }
        "accept_invitation" => accept_invitation(&gh, ev, &auto.acts.invitation_allow).await,
        o => Err(GhError::Config(format!(
            "action inconnue: {o} (label:X, reviewers:a,b, merge_if_green, accept_invitation)"
        ))),
    }
}

/// Applique toutes les règles à un événement ; rend (libellé, résultat) pour chaque destination/action.
/// Chaque sortie est comptée dans `stats` (`rule:<nom>:<destination>` / `auto:<verbe>`).
pub async fn process(auto: &Auto, ev: &Event) -> Vec<(String, Result<()>)> {
    let mut out = vec![];
    for r in auto.rules.iter().filter(|r| rule_matches(r, ev)) {
        for to in &r.to {
            let t = Instant::now();
            let res = match auto.dests.get(to) {
                Some(d) => deliver(&r.name, d, ev).await,
                None => Err(GhError::Config(format!("destination inconnue: {to}"))),
            };
            let label = format!("rule:{}:{to}", r.name);
            stats::global().record(&label, res.as_ref().err(), t.elapsed().as_millis() as u64);
            out.push((label, res));
        }
        for a in &r.actions {
            let t = Instant::now();
            let res = run_action(auto, a, ev).await.map(|_| ());
            let label = format!("auto:{}", a.split(':').next().unwrap_or(a));
            stats::global().record(&label, res.as_ref().err(), t.elapsed().as_millis() as u64);
            out.push((format!("{label}:{}", r.name), res));
        }
    }
    out
}

// ---------- relevé périodique ----------

#[allow(clippy::too_many_arguments)] // champs d'un Event, un appel par relevé
fn polled(
    account: &str,
    id: String,
    kind: &str,
    action: &str,
    repo: &str,
    title: &str,
    url: &str,
    payload: Value,
) -> Event {
    Event {
        id,
        account: account.into(),
        repo: repo.into(),
        kind: kind.into(),
        action: action.into(),
        actor: String::new(),
        title: title.into(),
        url: url.into(),
        at: chrono::Utc::now(),
        payload,
    }
}

/// Un passage du relevé : invitations (dépôt et organisation), notifications, runs en échec des dépôts suivis.
/// Les événements nouveaux (id absent du Store) sont publiés sur le bus ; rend leur nombre.
pub async fn poll_once(cfg: &Config, store: &Store, bus: &Bus) -> usize {
    let mut evs = vec![];
    let watched = store.watch_list(None).unwrap_or_default();
    for a in &cfg.accounts {
        let gh = Gh::new(a.clone());
        let name = a.name.as_str();
        for i in gh
            .paginate("/user/repository_invitations", &[], None, 1)
            .await
            .unwrap_or_default()
        {
            let Some(id) = i["id"].as_i64() else { continue };
            let repo = i
                .pointer("/repository/full_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut e = polled(
                name,
                format!("inv:{name}:{id}"),
                "invitation",
                "created",
                repo,
                &format!("Invitation à {repo}"),
                st(&i, &["/html_url"]),
                json!({"invitation": i}),
            );
            e.actor = st(&i, &["/inviter/login"]).into();
            evs.push(e);
        }
        for m in gh
            .paginate(
                "/user/memberships/orgs",
                &[("state", "pending".into())],
                None,
                1,
            )
            .await
            .unwrap_or_default()
        {
            let Some(org) = m.pointer("/organization/login").and_then(Value::as_str) else {
                continue;
            };
            evs.push(polled(
                name,
                format!("orginv:{name}:{org}"),
                "invitation",
                "created",
                "",
                &format!("Invitation à l'organisation {org}"),
                "",
                json!({"organization": {"login": org}, "membership": m}),
            ));
        }
        for n in gh
            .paginate("/notifications", &[], None, 1)
            .await
            .unwrap_or_default()
        {
            let Some(id) = n["id"].as_str() else { continue };
            let (repo, t) = (
                st(&n, &["/repository/full_name"]),
                st(&n, &["/subject/title"]),
            );
            evs.push(polled(
                name,
                format!(
                    "notif:{name}:{id}:{}",
                    n["updated_at"].as_str().unwrap_or("")
                ),
                "notification",
                st(&n, &["/reason"]),
                repo,
                t,
                st(&n, &["/subject/url"]),
                n.clone(),
            ));
        }
    }
    for w in watched {
        let (Some(account), Some(repo)) = (w["account"].as_str(), w["repo"].as_str()) else {
            continue;
        };
        let Ok(a) = cfg.account(Some(account)) else {
            continue;
        };
        let q = [("status", "failure".into()), ("per_page", "20".into())];
        for run in Gh::new(a.clone())
            .paginate(
                &format!("/repos/{repo}/actions/runs"),
                &q,
                Some("workflow_runs"),
                1,
            )
            .await
            .unwrap_or_default()
        {
            let Some(id) = run["id"].as_i64() else {
                continue;
            };
            let title = st(&run, &["/display_title", "/name"]).to_string();
            let mut e = polled(
                account,
                format!("run:{account}:{id}:{}", run["run_attempt"]),
                "workflow_run",
                "completed",
                repo,
                &title,
                st(&run, &["/html_url"]),
                json!({"workflow_run": run, "repository": {"full_name": repo}}),
            );
            e.actor = st(&run, &["/actor/login"]).into();
            evs.push(e);
        }
    }
    let mut n = 0;
    for e in evs {
        if store.insert(&e).unwrap_or(false) {
            n += 1;
            let _ = bus.send(e);
        }
    }
    n
}

// ---------- tâches de fond ----------

static STORE: OnceLock<Arc<Store>> = OnceLock::new();

/// Lance les tâches de fond : abonnement au bus (règles) et relevé périodique si `[poller]` est présent.
pub fn spawn(cfg: Arc<Config>, store: Arc<Store>, bus: Bus) {
    let _ = STORE.set(store.clone());
    let auto = match Auto::from_config(cfg.clone()) {
        Ok(a) => a,
        Err(e) => return eprintln!("ghmcp: {e} (règles désactivées)"),
    };
    if !auto.rules.is_empty() {
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        for (label, r) in process(&auto, &ev).await {
                            if let Err(e) = r {
                                eprintln!("ghmcp: {label}: {e}");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(p) = cfg.poller.as_ref().and_then(|p| p.as_table()) {
        if p.get("enabled").and_then(toml::Value::as_bool) != Some(false) {
            let secs = p
                .get("interval_secs")
                .and_then(toml::Value::as_integer)
                .unwrap_or(300)
                .max(30) as u64;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(secs));
                loop {
                    tick.tick().await;
                    poll_once(&cfg, &store, &bus).await;
                }
            });
        }
    }
}

// ---------- gh_pr_dashboard ----------

async fn pr_state(gh: &Gh, repo: &str, n: i64) -> Value {
    let (p1, p2) = (
        format!("/repos/{repo}/pulls/{n}"),
        format!("/repos/{repo}/pulls/{n}/reviews"),
    );
    let (pr, reviews) = tokio::join!(
        gh.rest("GET", &p1, &[], None),
        gh.paginate(&p2, &[], None, 1)
    );
    let pr = match pr {
        Ok(p) => p,
        Err(e) => return json!({"repo": repo, "number": n, "error": e.to_string()}),
    };
    let sha = pr
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (ci, total, failed, pending) = if sha.is_empty() {
        ("unknown".into(), 0, 0, 0)
    } else {
        ci_of(gh, repo, sha).await
    };
    // dernier avis par relecteur ; un simple commentaire ne remplace pas une décision.
    let mut last: HashMap<String, String> = HashMap::new();
    for r in reviews.unwrap_or_default() {
        let (u, s) = (
            r.pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            r["state"].as_str().unwrap_or("").to_string(),
        );
        if s != "COMMENTED" || !last.contains_key(&u) {
            last.insert(u, s);
        }
    }
    let count = |s: &str| last.values().filter(|v| v.as_str() == s).count();
    let ms = pr["mergeable_state"].as_str().unwrap_or("unknown");
    let (approved, changes) = (count("APPROVED"), count("CHANGES_REQUESTED"));
    json!({
        "repo": repo, "number": n, "title": pr["title"], "author": pr.pointer("/user/login"), "url": pr["html_url"],
        "draft": pr["draft"], "state": pr["state"], "mergeable": pr["mergeable"], "mergeable_state": ms, "conflicts": ms == "dirty",
        "ci": {"state": ci, "checks": total, "failed": failed, "pending": pending},
        "reviews": {"approved": approved, "changes_requested": changes, "commented": count("COMMENTED"), "pending": count("PENDING")},
        "ready_to_merge": ci == "success" && changes == 0 && ms == "clean" && pr["draft"] != true,
    })
}

async fn pr_dashboard(ctx: Ctx, args: Value) -> Result<Value> {
    let repo = ghmcp_core::arg_str(&args, "repo").map(String::from);
    let acct = ghmcp_core::arg_str(&args, "account");
    // (compte, dépôt, PR?) : arguments explicites, sinon les suivis (gh_watch).
    let targets: Vec<(String, String, Option<i64>)> = match repo {
        Some(r) => vec![(
            ctx.config().account(acct)?.name.clone(),
            r,
            ghmcp_core::arg_i64(&args, "pr"),
        )],
        None => {
            let store = STORE.get().ok_or_else(|| {
                GhError::Other(
                    "journal d'activité inactif : passer repo/pr ou lancer `ghmcp serve`".into(),
                )
            })?;
            store
                .watch_list(acct)?
                .iter()
                .filter_map(|w| {
                    Some((
                        w["account"].as_str()?.to_string(),
                        w["repo"].as_str()?.to_string(),
                        w["pr"].as_i64(),
                    ))
                })
                .collect()
        }
    };
    let mut jobs = vec![];
    for (account, repo, pr) in targets {
        let gh = Gh::new(ctx.config().account(Some(&account))?.clone());
        let nums = match pr {
            Some(n) => vec![n],
            None => gh
                .paginate(
                    &format!("/repos/{repo}/pulls"),
                    &[("state", "open".into())],
                    None,
                    1,
                )
                .await?
                .iter()
                .filter_map(|p| p["number"].as_i64())
                .collect(),
        };
        jobs.extend(nums.into_iter().take(30).map(|n| {
            let (gh, repo, account) = (gh.clone(), repo.clone(), account.clone());
            async move {
                let mut v = pr_state(&gh, &repo, n).await;
                v["account"] = json!(account);
                v
            }
        }));
    }
    let prs = futures::future::join_all(jobs).await;
    let ready = prs.iter().filter(|p| p["ready_to_merge"] == true).count();
    let red = prs
        .iter()
        .filter(|p| p.pointer("/ci/state").and_then(Value::as_str) == Some("failure"))
        .count();
    Ok(
        json!({"count": prs.len(), "ready_to_merge": ready, "ci_failing": red, "conflicts": prs.iter().filter(|p| p["conflicts"] == true).count(), "pull_requests": prs}),
    )
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    vec![Tool::new(
        "gh_pr_dashboard",
        "État de toutes les PR suivies (gh_watch) ou d'un dépôt/PR : CI (statuts + check runs), revues, conflits, prête à fusionner.",
        json!({"type":"object","properties":{
            "repo":{"type":"string","description":"owner/repo ; absent = tous les dépôts/PR suivis"},
            "pr":{"type":"integer","description":"numéro de PR ; absent = toutes les PR ouvertes du dépôt"}}}),
        pr_dashboard,
    )
    .scopes(&["repo"])
    .perm("pull_requests", Access::Read)
    .perm("checks", Access::Read)
    .perm("statuses", Access::Read)]
}
