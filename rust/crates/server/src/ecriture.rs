//! Écriture bornée (`[ecriture] mode = "borne"`, défaut) : un agent peut ouvrir une branche et une PR ;
//! fusion, suppression, écriture sur la branche par défaut et toute écriture de niveau cicd / securite /
//! admin (protections, rulesets, variables, secrets, environnements, déploiements, webhooks, réglages)
//! sont refusées. La fusion passe par la CI (`fusion-auto`). `mode = "libre"` lève la borne.

use ghmcp_core::tool::{Level, Tool};
use ghmcp_core::{arg_str, Ctx, GhError, Result};
use serde_json::Value;

/// Outils refusés d'office en mode borné (niveau dev mais hors du rôle d'un agent).
pub const INTERDITS: &[&str] = &[
    "merge_pull_request",
    "gh_repo_transfer",
    "update_pull_request_branch", // pousse un commit de fusion sur la branche de la PR
    "create_repository_ruleset",
    "custom_properties_write",
];

/// Écritures de niveau élevé permises : relancer la CI ne change ni le code ni les portes.
pub const PERMIS: &[&str] = &["gh_actions_run_rerun", "gh_actions_run_rerun_failed"];

/// Outils qui écrivent des fichiers sur une branche (`branch`) : jamais la branche par défaut.
pub const SUR_BRANCHE: &[&str] = &["push_files", "create_or_update_file"];

pub fn borne(ctx: &Ctx) -> bool {
    ctx.config()
        .ecriture
        .as_ref()
        .and_then(|e| e.get("mode"))
        .and_then(|m| m.as_str())
        != Some("libre")
}

fn refus(m: String) -> GhError {
    GhError::Denied(format!("écriture bornée : {m}"))
}

/// Opération choisie par `method` / `action` / `operation` (outils multi-actions).
fn operation(args: &Value) -> Option<&str> {
    ["method", "action", "operation"]
        .into_iter()
        .find_map(|k| arg_str(args, k))
}

/// Appel en lecture seule : annotation, nom d'outil de lecture, ou opération get/list.
fn lecture(tool: &Tool, args: &Value) -> bool {
    let n = tool.name.as_str();
    tool.annotations.get("readOnlyHint") == Some(&Value::Bool(true))
        || n.starts_with("get_")
        || n.starts_with("list_")
        || ["_list", "_get", "_logs", "_latest", "_deliveries"]
            .iter()
            .any(|s| n.ends_with(s))
        || operation(args).is_some_and(|o| {
            ["get", "list", "read", "show"]
                .iter()
                .any(|p| o.starts_with(p))
        })
}

/// `Ok` si l'appel est permis ; refus = `GhError::Denied` (définitif, ne pas contourner).
pub async fn verifier(ctx: &Ctx, tool: &Tool, args: &Value) -> Result<()> {
    if !borne(ctx) {
        return Ok(());
    }
    let name = tool.name.as_str();
    let suppression = name.contains("delete") && name != "delete_pending_pull_request_review";
    if INTERDITS.contains(&name) || suppression {
        return Err(refus(format!(
            "`{name}` interdit (fusion par la CI, aucune suppression par un agent)"
        )));
    }
    if let Some(o) = operation(args).filter(|_| name != "gh_watch") {
        if o.starts_with("delete") || o.starts_with("remove") {
            return Err(refus(format!(
                "`{name}` {o} interdit (aucune suppression par un agent)"
            )));
        }
        // workflow_dispatch = livraison possible (prod.yml) : réservé à la CI et aux humains
        if name == "actions_run_trigger" && o == "run_workflow" {
            return Err(refus(format!(
                "`{name}` {o} interdit (déclencher un workflow livre)"
            )));
        }
    }
    let eleve = matches!(tool.level, Level::Cicd | Level::Securite | Level::Admin);
    if eleve && !PERMIS.contains(&name) && !lecture(tool, args) {
        return Err(refus(format!(
            "`{name}` (niveau {:?}) en écriture interdit : protections, CI, secrets et réglages restent aux humains",
            tool.level
        )));
    }
    if !SUR_BRANCHE.contains(&name) {
        return Ok(());
    }
    let Some(branche) = arg_str(args, "branch").filter(|b| !b.is_empty()) else {
        return Err(refus(format!(
            "`{name}` sans `branch` écrirait sur la branche par défaut"
        )));
    };
    // Refus si la branche par défaut ne peut pas être établie (jamais d'ouverture sur erreur).
    let (Some(o), Some(r)) = (arg_str(args, "owner"), arg_str(args, "repo")) else {
        return Err(refus(format!("`{name}` sans owner/repo")));
    };
    let v = ctx
        .gh(args)?
        .rest("GET", &format!("/repos/{o}/{r}"), &[], None)
        .await
        .map_err(|e| refus(format!("branche par défaut de {o}/{r} inconnue ({e})")))?;
    let mut defaut = vec!["main", "master"];
    defaut.extend(v["default_branch"].as_str());
    let b = branche.trim_start_matches("refs/heads/");
    if defaut
        .iter()
        .any(|d| d.trim_start_matches("refs/heads/") == b)
    {
        return Err(refus(format!(
            "pas d'écriture sur la branche par défaut ({branche}) ; ouvrir une branche puis une PR"
        )));
    }
    Ok(())
}
