//! Registre agent ↔ commit ↔ PR ↔ issue ↔ déploiement et outil gh_qui_a_deploye (sans réseau).
use chrono::Utc;
use ghmcp_activity::{agent_id, router, tools, State, Store};
use ghmcp_core::{bus, Config, Ctx, Event};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

const R: &str = "exemple-org/projet-test";

fn ev(id: &str, kind: &str, action: &str, payload: Value) -> Event {
    Event {
        id: id.into(),
        account: "a".into(),
        repo: R.into(),
        kind: kind.into(),
        action: action.into(),
        actor: "alice-test".into(),
        title: String::new(),
        url: String::new(),
        at: Utc::now(),
        payload,
    }
}

fn push(sha: &str, msg: &str) -> Event {
    ev(
        &format!("p-{sha}"),
        "push",
        "",
        json!({"commits": [{"id": sha, "message": msg, "timestamp": "2026-11-11T10:00:00Z",
                             "author": {"username": "alice-test"}}]}),
    )
}

fn deploiement(id: i64, env: &str, sha: &str, etat: &str) -> Event {
    ev(
        &format!("d-{id}-{etat}"),
        "deployment_status",
        "created",
        json!({"deployment": {"id": id, "environment": env, "sha": sha, "description": "", "creator": {"login": "robot-ci"}},
               "deployment_status": {"state": etat, "environment_url": "https://exemple.invalid/app"}}),
    )
}

#[test]
fn trailer_agent_id() {
    assert_eq!(
        agent_id("feat: x\n\nAgent-Id: hermes-42"),
        Some("hermes-42".into())
    );
    assert_eq!(
        agent_id("fix\n\nagent-id:  c_a~x/1 "),
        Some("c_a~x/1".into()),
        "clé insensible à la casse"
    );
    assert_eq!(agent_id("rien ici"), None, "absent");
    assert_eq!(
        agent_id("Agent-Id: a1\nAgent-Id: a1"),
        Some("a1".into()),
        "doublé identique"
    );
    assert_eq!(
        agent_id("Agent-Id: a1\nAgent-Id: a2"),
        None,
        "doublé contradictoire = ambigu"
    );
    assert_eq!(
        agent_id("Agent-Id: rm -rf /; echo"),
        None,
        "valeur hors alphabet = ignorée"
    );
    assert_eq!(
        agent_id("> Agent-Id: cite-1"),
        Some("cite-1".into()),
        "citation Markdown"
    );
}

#[test]
fn qui_a_deploye_quoi() {
    let s = Store::open(std::path::Path::new(":memory:")).unwrap();
    s.enregistrer(&push("aaa111", "feat: v1\n\nAgent-Id: agent-un"))
        .unwrap();
    s.enregistrer(&push("bbb222", "fix: v2\n\nAgent-Id: agent-deux"))
        .unwrap();
    s.enregistrer(&push("ccc333", "chore: v3 sans trailer"))
        .unwrap();
    s.enregistrer(&ev("pr-5", "pull_request", "closed", json!({"pull_request": {
        "number": 5, "merged": true, "state": "closed", "body": "corps sans trailer",
        "head": {"ref": "feat/v2", "sha": "bbb222"}, "title": "v2", "html_url": "https://github.com/exemple-org/projet-test/pull/5"}}))).unwrap();
    s.enregistrer(&deploiement(1, "production", "aaa111", "success"))
        .unwrap();
    s.enregistrer(&deploiement(2, "staging", "bbb222", "success"))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.enregistrer(&deploiement(3, "production", "bbb222", "in_progress"))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.enregistrer(&deploiement(3, "production", "bbb222", "success"))
        .unwrap();
    s.enregistrer(&deploiement(4, "production", "ccc333", "failure"))
        .unwrap();

    let t = Instant::now();
    let d = s.qui_a_deploye(R, Some("production")).unwrap();
    assert!(t.elapsed().as_millis() < 50, "lecture indexée < 50 ms");
    let resume: Vec<(i64, &str, Option<&str>, &str)> = d
        .iter()
        .map(|x| {
            (
                x["deploiement"].as_i64().unwrap(),
                x["sha"].as_str().unwrap(),
                x["agent_id"].as_str(),
                x["statut"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        resume,
        [
            (4, "ccc333", None, "failure"),
            (3, "bbb222", Some("agent-deux"), "success"),
            (1, "aaa111", Some("agent-un"), "success")
        ]
    );
    assert_eq!(d[1]["pr"], 5, "PR liée par head_sha");
    assert_eq!(d[1]["commit"], "fix: v2");
    assert_eq!(s.qui_a_deploye(R, None).unwrap().len(), 4);
    // la PR hérite de l'agent de son commit de tête
    let f = s.fiche(Some(R), Some("agent-deux"), None, None).unwrap();
    assert_eq!(f["prs"][0]["numero"], 5);
    assert_eq!(f["commits"][0]["sha"], "bbb222");
    assert_eq!(f["deploiements"].as_array().unwrap().len(), 2);
}

#[test]
fn issue_agent_par_trailer_ou_label() {
    let s = Store::open(std::path::Path::new(":memory:")).unwrap();
    let issue = |n: i64, body: &str, labels: Value| {
        ev(
            &format!("i-{n}"),
            "issues",
            "opened",
            json!({"issue": {"number": n, "body": body, "labels": labels, "author_association": "MEMBER",
            "user": {"login": "bob-test"}, "state": "open", "title": "t", "html_url": ""}}),
        )
    };
    s.enregistrer(&issue(1, "Agent-Id: agent-un", json!([])))
        .unwrap();
    s.enregistrer(&issue(
        2,
        "rien",
        json!([{"name": "bug"}, {"name": "agent:agent-deux"}]),
    ))
    .unwrap();
    s.enregistrer(&issue(3, "rien", json!([]))).unwrap();
    // un tiers (non membre) ne peut pas désigner la session qui lira son issue
    let mut tiers = issue(4, "Agent-Id: session-exemple", json!([]));
    tiers.payload["issue"]["author_association"] = json!("NONE");
    s.enregistrer(&tiers).unwrap();
    assert_eq!(s.agent_de(R, 4), None, "trailer d'un tiers ignoré");
    // un label agent:<id> (posé par un trieur) réattribue ; un trailer ne remplace pas un agent fixé
    s.enregistrer(&issue(1, "Agent-Id: intrus", json!([])))
        .unwrap();
    assert_eq!(s.agent_de(R, 1).as_deref(), Some("agent-un"));
    assert_eq!(s.agent_de(R, 1).as_deref(), Some("agent-un"));
    assert_eq!(s.agent_de(R, 2).as_deref(), Some("agent-deux"));
    assert_eq!(s.agent_de(R, 3), None);
    s.fixer_agent_issue(R, 3, "session-x").unwrap();
    s.fixer_agent_issue(R, 1, "autre").unwrap();
    assert_eq!(s.agent_de(R, 3).as_deref(), Some("session-x"));
    assert_eq!(
        s.agent_de(R, 1).as_deref(),
        Some("agent-un"),
        "un agent déjà fixé n'est pas remplacé"
    );
}

#[tokio::test]
async fn outil_gh_qui_a_deploye() {
    let config = Arc::new(Config::from_toml("").unwrap());
    let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
    let _ = router(State {
        store: store.clone(),
        bus: bus(),
        config: config.clone(),
    }); // pose le magasin des outils
    store
        .enregistrer(&push("aaa111", "feat\n\nAgent-Id: agent-un"))
        .unwrap();
    store
        .enregistrer(&deploiement(9, "production", "aaa111", "success"))
        .unwrap();
    let t = tools();
    let qui = t.iter().find(|t| t.name == "gh_qui_a_deploye").unwrap();
    let ctx = Ctx::new(config, bus());
    let r = (qui.handler)(ctx.clone(), json!({"repo": R, "env": "production"}))
        .await
        .unwrap();
    assert_eq!(r["dernier"]["agent_id"], "agent-un");
    assert!((qui.handler)(ctx.clone(), json!({"repo": "pas-un-depot"}))
        .await
        .is_err());
    let reg = t.iter().find(|t| t.name == "gh_registre").unwrap();
    assert!(
        (reg.handler)(ctx.clone(), json!({})).await.is_err(),
        "au moins un filtre"
    );
    let f = (reg.handler)(ctx, json!({"sha": "aaa111"})).await.unwrap();
    assert_eq!(f["deploiements"][0]["deploiement"], 9);
}
