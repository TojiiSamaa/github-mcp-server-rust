//! ghmcp-tools-security : code/secret scanning, dependabot, advisories, code quality, rulesets, custom properties, copilot, ui_get.
//! Contrat : voir l en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
//! Porté de github-mcp-server (code_scanning.go, secret_scanning.go, dependabot.go, security_advisories.go,
//! code_quality.go, rulesets.go, custom_properties.go, copilot.go, ui_tools.go).
mod alerts;
mod copilot;
mod rulesets;

use ghmcp_core::{GhError, Result, Tool};
use serde_json::Value;

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    [alerts::tools(), rulesets::tools(), copilot::tools()]
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) fn bad(m: impl Into<String>) -> GhError {
    GhError::Other(m.into())
}

/// Paramètre numérique obligatoire (message identique au Go : `missing required parameter: x`).
pub(crate) fn req_num(args: &Value, k: &str) -> Result<i64> {
    ghmcp_core::arg_i64(args, k)
        .filter(|n| *n != 0)
        .ok_or_else(|| bad(format!("missing required parameter: {k}")))
}

/// Ajoute `to=valeur` à la requête si l argument chaîne `from` est présent et non vide.
pub(crate) fn opt(q: &mut Vec<(&'static str, String)>, args: &Value, from: &str, to: &'static str) {
    if let Some(v) = ghmcp_core::arg_str(args, from).filter(|s| !s.is_empty()) {
        q.push((to, v.into()));
    }
}

/// Pagination REST (page, perPage -> page, per_page).
pub(crate) fn page_q(args: &Value) -> Vec<(&'static str, String)> {
    let mut q = vec![];
    if let Some(p) = ghmcp_core::arg_i64(args, "page").filter(|p| *p > 0) {
        q.push(("page", p.to_string()));
    }
    if let Some(p) = ghmcp_core::arg_i64(args, "perPage").filter(|p| *p > 0) {
        q.push(("per_page", p.to_string()));
    }
    q
}

pub(crate) fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config, Ctx};
    use serde_json::json;
    use std::sync::Arc;

    const CFG: &str = "default_account = \"alice-test\"\n[[accounts]]\nname = \"alice-test\"\nkind = \"classic\"\ntoken_env = \"GHMCP_TEST_SEC\"\n";

    const NOMS: [&str; 19] = [
        "get_code_scanning_alert",
        "list_code_scanning_alerts",
        "get_secret_scanning_alert",
        "list_secret_scanning_alerts",
        "get_dependabot_alert",
        "list_dependabot_alerts",
        "get_global_security_advisory",
        "list_global_security_advisories",
        "list_org_repository_security_advisories",
        "list_repository_security_advisories",
        "get_code_quality_finding",
        "create_repository_ruleset",
        "repository_ruleset_read",
        "custom_properties_read",
        "custom_properties_write",
        "assign_copilot_to_issue",
        "assign_copilot_to_issue_with_intent",
        "request_copilot_review",
        "ui_get",
    ];

    fn ctx() -> Ctx {
        Ctx::new(Arc::new(Config::from_toml(CFG).unwrap()), bus())
    }

    async fn call(n: &str, a: Value) -> ghmcp_core::Result<Value> {
        let t = tools()
            .into_iter()
            .find(|t| t.name == n)
            .unwrap_or_else(|| panic!("manque {n}"));
        (t.handler)(ctx(), a).await
    }

    #[test]
    fn noms_presents() {
        let names: Vec<String> = tools().iter().map(|t| t.name.clone()).collect();
        for n in NOMS {
            assert!(names.iter().any(|x| x == n), "manque {n}");
        }
        assert_eq!(names.len(), 19);
    }

    /// Chaque outil répond en mode test (non initialisé = fermé) avec des arguments minimaux valides.
    #[tokio::test]
    async fn chaque_outil_repond_en_mode_test() {
        let or = json!({"owner":"o","repo":"r"});
        let cas: Vec<(&str, Value)> = vec![
            (
                "get_code_scanning_alert",
                json!({"owner":"o","repo":"r","alertNumber":1}),
            ),
            ("list_code_scanning_alerts", or.clone()),
            (
                "get_secret_scanning_alert",
                json!({"owner":"o","repo":"r","alertNumber":1}),
            ),
            ("list_secret_scanning_alerts", or.clone()),
            (
                "get_dependabot_alert",
                json!({"owner":"o","repo":"r","alertNumber":1}),
            ),
            ("list_dependabot_alerts", or.clone()),
            (
                "get_global_security_advisory",
                json!({"ghsaId":"GHSA-xxxx-xxxx-xxxx"}),
            ),
            ("list_global_security_advisories", json!({"cwes":["79"]})),
            (
                "list_org_repository_security_advisories",
                json!({"org":"o"}),
            ),
            ("list_repository_security_advisories", or.clone()),
            (
                "get_code_quality_finding",
                json!({"owner":"o","repo":"r","findingNumber":3}),
            ),
            (
                "create_repository_ruleset",
                json!({"level":"repository","owner":"o","repo":"r","name":"n","enforcement":"active","rules":[{"type":"deletion"}]}),
            ),
            (
                "repository_ruleset_read",
                json!({"level":"repository","method":"list","owner":"o","repo":"r"}),
            ),
            (
                "custom_properties_read",
                json!({"level":"repository","owner":"o","repo":"r"}),
            ),
            (
                "custom_properties_write",
                json!({"level":"repository","owner":"o","repo":"r","properties":[{"property_name":"team","value":"x"}]}),
            ),
            (
                "assign_copilot_to_issue",
                json!({"owner":"o","repo":"r","issue_number":1}),
            ),
            (
                "assign_copilot_to_issue_with_intent",
                json!({"owner":"o","repo":"r","issue_number":1,"rationale":"x","confidence":"HIGH","is_suggestion":true}),
            ),
            (
                "request_copilot_review",
                json!({"owner":"o","repo":"r","pullNumber":2}),
            ),
            ("ui_get", json!({"method":"labels","owner":"o","repo":"r"})),
        ];
        assert_eq!(cas.len(), 19);
        for (n, a) in cas {
            let v = call(n, a).await.unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(!v.is_null(), "{n} vide");
        }
    }

    #[tokio::test]
    async fn chemins_et_requetes_exacts() {
        let v = call(
            "list_code_scanning_alerts",
            json!({"owner":"o","repo":"r","severity":"high"}),
        )
        .await
        .unwrap();
        assert_eq!(v["path"], "/repos/o/r/code-scanning/alerts");
        let q = v["query"].to_string();
        assert!(q.contains("high") && q.contains("open"), "{q}");
        let v = call(
            "repository_ruleset_read",
            json!({"level":"organization","method":"get","org":"acme","ruleset_id":7}),
        )
        .await
        .unwrap();
        assert_eq!(v["path"], "/orgs/acme/rulesets/7");
        let v = call("create_repository_ruleset", json!({"level":"organization","org":"acme","name":"n","enforcement":"active","rules":[{"type":"deletion"}]})).await.unwrap();
        assert_eq!(
            (v["method"].as_str(), v["path"].as_str()),
            (Some("POST"), Some("/orgs/acme/rulesets"))
        );
        assert_eq!(v["body"]["target"], "branch");
        let v = call(
            "request_copilot_review",
            json!({"owner":"o","repo":"r","pullNumber":2}),
        )
        .await
        .unwrap();
        assert_eq!(v, json!(""));
    }

    #[tokio::test]
    async fn parametres_invalides_refuses() {
        let e = call("get_code_scanning_alert", json!({"owner":"o","repo":"r"}))
            .await
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("missing required parameter: alertNumber"),
            "{e}"
        );
        let dup = json!({"level":"repository","owner":"o","repo":"r","name":"n","enforcement":"active","rules":[{"type":"deletion"},{"type":"deletion"}]});
        assert!(call("create_repository_ruleset", dup)
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate rule type"));
        let e = call(
            "repository_ruleset_read",
            json!({"level":"enterprise","method":"get_rule_suite","enterprise":"e"}),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("not supported for level"), "{e}");
        let bad_conf = json!({"owner":"o","repo":"r","issue_number":1,"rationale":"x","confidence":"BAD","is_suggestion":false});
        assert!(call("assign_copilot_to_issue_with_intent", bad_conf)
            .await
            .unwrap_err()
            .to_string()
            .contains("confidence must be one of"));
    }
}
