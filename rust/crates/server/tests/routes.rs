//! Table méthode+route par outil : chaque ligne de data/routes_go.tsv (extraite du serveur Go)
//! est rejouée contre le handler Rust en mode test ; le verbe et le chemin enregistrés par
//! `core::net` doivent correspondre au gabarit Go ({x} = un segment, query ignorée, GRAPHQL = "graphql").
//! Un seul #[tokio::test] : l'enregistreur d'appels est global.
//! Arguments : minimaux (requis + owner/repo, valeurs fictives) surchargés par data/routes_args.json
//! ("outil" ou "outil/méthode" -> objet fusionné).

use ghmcp_core::{net, Config, Ctx};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Écarts voulus (extension ou choix documenté) : (outil, méthode, verbe, route Go) -> justification.
const EXCEPTIONS: &[(&str, &str, &str, &str, &str)] = &[
    ("label_write", "create", "POST", "/repos/{owner}/{repo}/labels", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("label_write", "update", "PATCH", "/repos/{owner}/{repo}/labels/{x}", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("label_write", "delete", "DELETE", "/repos/{owner}/{repo}/labels/{x}", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("ui_get", "labels", "GET", "/repos/{owner}/{repo}/labels", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("ui_get", "issue_fields", "GET", "/orgs/{o}/issue-fields", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("issue_read", "get_labels", "GET", "/repos/{owner}/{repo}/issues/{n}/labels", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("issue_read", "get_parent", "GET", "/repos/{owner}/{repo}/issues/{n}/parent", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_list", "list_project_views", "GET", "/users/{o}/projectsV2/{n}/views", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_write", "create_project_view", "POST", "/users/{o}/projectsV2/{n}/views", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_write", "update_project_view", "PATCH", "/users/{o}/projectsV2/{n}/views/{v}", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_write", "delete_project_view", "DELETE", "/users/{o}/projectsV2/{n}/views/{v}", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_write", "add_project_item", "POST", "/users/{o}/projectsV2/{n}/items", "Go passe par GraphQL, Rust par l'équivalent REST (même donnée)"),
    ("projects_write", "add_project_item", "GET", "/repos/{o}/{r}/issues/{n}", "résolution de l'id de l'issue en REST, Go la fait en GraphQL"),
    ("issue_dependency_write", "add", "GET", "/repos/{o}/{r}/issues/{n}", "lecture de l'id de l'issue bloquante avant l'ajout (API REST exige l'id)"),
    ("issue_dependency_write", "remove", "GET", "/repos/{o}/{r}/issues/{n}", "idem : id de l'issue avant retrait"),
    ("create_branch", "", "GET", "/repos/{o}/{r}/git/ref/heads/{b}", "vérifie le SHA de base via la ref de branche au lieu du dépôt par défaut"),
    ("get_tag", "", "GET", "/repos/{o}/{r}/git/ref/tags/{t}", "ref du tag (le Go l'appelle aussi) sans seconde lecture du tag annoté"),
    ("get_file_contents", "", "GET", "/repos/{o}/{r}/contents/{p}", "contents/ avec chemin vide pour la racine (le gabarit Go exige un chemin)"),
    ("create_issue", "", "POST", "/repos/{o}/{r}/issues/{n}/sub_issues", "rattachement au parent en REST (Go : mutation GraphQL)"),
    ("issue_write", "create", "POST", "/repos/{o}/{r}/issues/{n}/sub_issues", "rattachement au parent en REST (Go : mutation GraphQL)"),
    ("repository_ruleset_read", "get_rule_suite", "GET", "/orgs/{o}/rulesets/rule-suites/{i}", "niveau organization : le Go appelle orgs/{org}/rulesets/rule-suites/{id} (rulesets.go:533) ; l'extracteur ne garde que le 1er niveau (repository)"),
    ("repository_ruleset_read", "list_rule_suites", "GET", "/orgs/{o}/rulesets/rule-suites", "niveau organization : le Go appelle orgs/{org}/rulesets/rule-suites (rulesets.go:474) ; l'extracteur ne garde que le 1er niveau (repository)"),
    ("list_starred_repositories", "", "GET", "/users/{u}/starred", "paramètre username : Rust liste aussi les étoiles d'un autre utilisateur"),
];

/// Décision T8b : les lignes GRAPHQL de NON_ATTEINTES de ces groupes sont des étapes que Rust fait en REST
/// (route REST vérifiée ligne par ligne ou listée en EXCEPTIONS) ; elles sortent du dénominateur des 90 %.
/// Portés sur GraphQL comme le Go : get_label, list_label ; field_names des projets atteints par variante.
/// Exceptions CHOIX_REST ratifiées : écritures de labels (label_write) et create_issue / issue_write restent en REST, même résultat.
/// list_issues (GraphQL, field_filters inclus) et update_project_items / projectItems (GraphQL) sont portés et ne figurent plus ici.
/// Restent aussi les vues/éléments de projets : même effet REST, chaîne d'ids GraphQL non rejouée.
/// (outil, méthode, justification d'une ligne)
const CHOIX_REST: &[(&str, &str, &str)] = &[
    ("label_write", "create", "POST /labels REST au lieu de Repository(id) + CreateLabel"),
    ("label_write", "update", "PATCH /labels/{name} REST au lieu de Repository(id) + UpdateLabel"),
    ("label_write", "delete", "DELETE /labels/{name} REST au lieu de Repository(id) + DeleteLabel"),
    ("ui_get", "labels", "GET /labels REST = Repository.labels du Go"),
    ("ui_get", "issue_fields", "GET /orgs/{o}/issue-fields REST au lieu des requêtes issueFields"),
    ("issue_read", "get_labels", "GET issues/{n}/labels REST = Repository.issue.labels du Go"),
    ("issue_read", "get_parent", "GET issues/{n}/parent REST = Repository.issue.parent du Go"),
    ("create_issue", "", "POST /issues REST (+ sub_issues en EXCEPTIONS) au lieu de createIssueMutation et ses résolutions d'ids"),
    ("issue_write", "create", "POST /issues REST (+ sub_issues en EXCEPTIONS) au lieu de createIssueMutation et ses résolutions d'ids"),
    ("issue_write", "update", "PATCH issues/{n} REST (state/state_reason) au lieu de CloseIssue/ReopenIssue GraphQL"),
    ("projects_list", "list_project_views", "GET projectsV2/{n}/views REST (EXCEPTIONS) au lieu de projectViews*Query"),
    ("projects_write", "add_project_item", "POST projectsV2/{n}/items REST (EXCEPTIONS) au lieu de AddProjectV2ItemByID"),
    ("projects_write", "create_project_view", "POST projectsV2/{n}/views REST (EXCEPTIONS) au lieu de createProjectV2ViewMutation"),
    ("projects_write", "update_project_view", "PATCH projectsV2/{n}/views/{v} REST (EXCEPTIONS) au lieu de updateProjectV2ViewMutation"),
    ("projects_write", "delete_project_view", "DELETE projectsV2/{n}/views/{v} REST (EXCEPTIONS) au lieu de DeleteProjectV2View"),
    ("projects_write", "update_project_item", "PATCH projectsV2/{n}/items/{id} REST au lieu des résolutions GraphQL de champ"),
];

/// Lignes Go que Rust ne rejoue pas, jamais comptées « vérifiées » : (outil, méthode, verbe, route Go ou "*", raison).
const NON_ATTEINTES: &[(&str, &str, &str, &str, &str)] = &[
    ("assign_copilot_to_issue", "", "PATCH", "/repos/{owner}/{repo}/issues/{issue_number}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3630]"),
    ("assign_copilot_to_issue", "", "PATCH", "repos/{owner}/{repo}/issues/{issueNumber}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3635]"),
    ("assign_copilot_to_issue", "", "DELETE", "repos/{owner}/{repo}/issues/{issueNumber}/issue-field-values/{fieldID}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3511]"),
    ("assign_copilot_to_issue_with_intent", "", "PATCH", "/repos/{owner}/{repo}/issues/{issue_number}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3630]"),
    ("assign_copilot_to_issue_with_intent", "", "PATCH", "repos/{owner}/{repo}/issues/{issueNumber}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3635]"),
    ("assign_copilot_to_issue_with_intent", "", "DELETE", "repos/{owner}/{repo}/issues/{issueNumber}/issue-field-values/{fieldID}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3511]"),
    ("create_issue", "", "GRAPHQL", "createIssueParentMetadataQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3195]"),
    ("create_issue", "", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:503]"),
    ("create_issue", "", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3214]"),
    ("create_issue", "", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3237]"),
    ("create_issue", "", "GET", "repos/{owner}/{repo}/issue-types", "types d'issue : Rust ne fait pas ce GET de validation (DÉDUIT) [issues.go:3247]"),
    ("create_issue", "", "GRAPHQL", "createIssueMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3148]"),
    ("issue_read", "get_labels", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:1273]"),
    ("issue_read", "get_parent", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:1207]"),
    ("issue_write", "create", "GRAPHQL", "createIssueParentMetadataQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3195]"),
    ("issue_write", "create", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:503]"),
    ("issue_write", "create", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3214]"),
    ("issue_write", "create", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3237]"),
    ("issue_write", "create", "GET", "repos/{owner}/{repo}/issue-types", "types d'issue : Rust ne fait pas ce GET de validation (DÉDUIT) [issues.go:3247]"),
    ("issue_write", "create", "GRAPHQL", "createIssueMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3148]"),
    ("issue_write", "update", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:427]"),
    ("issue_write", "update", "DELETE", "repos/{owner}/{repo}/issues/{issueNumber}/issue-field-values/{fieldID}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [issues.go:3511]"),
    ("issue_write", "update", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:79]"),
    ("issue_write", "update", "GRAPHQL", "ReopenIssue", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3559]"),
    ("issue_write", "update", "GRAPHQL", "CloseIssue", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues.go:3589]"),
    ("label_write", "create", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:482]"),
    ("label_write", "create", "GRAPHQL", "CreateLabel", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:392]"),
    ("label_write", "delete", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:503]"),
    ("label_write", "delete", "GRAPHQL", "DeleteLabel", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:458]"),
    ("label_write", "update", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:503]"),
    ("label_write", "update", "GRAPHQL", "UpdateLabel", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [labels.go:435]"),
    ("projects_get", "get_project_field", "GET", "/orgs/{org}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1318]"),
    ("projects_get", "get_project_field", "GET", "/users/{username}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1320]"),
    ("projects_get", "get_project_item", "GET", "/orgs/{org}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1318]"),
    ("projects_get", "get_project_item", "GET", "/users/{username}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1320]"),
    ("projects_list", "list_project_fields", "GET", "/orgs/{org}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1318]"),
    ("projects_list", "list_project_fields", "GET", "/users/{username}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1320]"),
    ("projects_list", "list_project_items", "GET", "/orgs/{org}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1318]"),
    ("projects_list", "list_project_items", "GET", "/users/{username}/projectsV2/{project_number}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1320]"),
    ("projects_list", "list_project_views", "GRAPHQL", "projectViewsOrgQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1988]"),
    ("projects_list", "list_project_views", "GRAPHQL", "projectViewsUserQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1994]"),
    ("projects_write", "add_project_item", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2573]"),
    ("projects_write", "add_project_item", "GRAPHQL", "Organization", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1619]"),
    ("projects_write", "add_project_item", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1626]"),
    ("projects_write", "add_project_item", "GRAPHQL", "AddProjectV2ItemByID", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1673]"),
    ("projects_write", "create_project_view", "GRAPHQL", "projectFieldsQueryOrg", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:111]"),
    ("projects_write", "create_project_view", "GRAPHQL", "projectFieldsQueryUser", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:117]"),
    ("projects_write", "create_project_view", "GRAPHQL", "Organization", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1619]"),
    ("projects_write", "create_project_view", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1626]"),
    ("projects_write", "create_project_view", "GRAPHQL", "createProjectV2ViewMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2161]"),
    ("projects_write", "create_project_view", "GRAPHQL", "updateProjectV2ViewMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2176]"),
    ("projects_write", "create_project_view", "GRAPHQL", "DeleteProjectV2View", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2296]"),
    ("projects_write", "delete_project_view", "GRAPHQL", "Organization", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1619]"),
    ("projects_write", "delete_project_view", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1626]"),
    ("projects_write", "delete_project_view", "GRAPHQL", "projectViewParentQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2198]"),
    ("projects_write", "delete_project_view", "GRAPHQL", "DeleteProjectV2View", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2296]"),
    ("projects_write", "update_project_item", "GRAPHQL", "Organization", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1619]"),
    ("projects_write", "update_project_item", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1626]"),
    ("projects_write", "update_project_item", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:527]"),
    ("projects_write", "update_project_item", "GRAPHQL", "projectFieldsQueryOrg", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:111]"),
    ("projects_write", "update_project_item", "GRAPHQL", "projectFieldsQueryUser", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:117]"),
    ("projects_write", "update_project_item", "GRAPHQL", "projectIssueFieldMetadataQueryOrg", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:343]"),
    ("projects_write", "update_project_item", "GRAPHQL", "projectIssueFieldMetadataQueryUser", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:347]"),
    ("projects_write", "update_project_item", "GET", "/orgs/{org}/projectsV2/{project_number}/items/{item_id}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1441]"),
    ("projects_write", "update_project_item", "GET", "/users/{username}/projectsV2/{project_number}/items/{item_id}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects.go:1443]"),
    ("projects_write", "update_project_item", "GRAPHQL", "setIssueFieldValueMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issues_granular.go:966]"),
    ("projects_write", "update_project_items", "GET", "/orgs/{org}/projectsV2/{project_number}/items/{item_id}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects_batch.go:710]"),
    ("projects_write", "update_project_items", "GET", "/users/{username}/projectsV2/{project_number}/items/{item_id}", "appel annexe de résolution du type d'owner ou de l'élément : Rust utilise owner_type / l'id fourni (DÉDUIT) [projects_batch.go:712]"),
    ("projects_write", "update_project_view", "GRAPHQL", "Organization", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1619]"),
    ("projects_write", "update_project_view", "GRAPHQL", "User", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:1626]"),
    ("projects_write", "update_project_view", "GRAPHQL", "projectViewParentQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2198]"),
    ("projects_write", "update_project_view", "GRAPHQL", "projectFieldsQueryOrg", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:111]"),
    ("projects_write", "update_project_view", "GRAPHQL", "projectFieldsQueryUser", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects_resolver.go:117]"),
    ("projects_write", "update_project_view", "GRAPHQL", "updateProjectV2ViewMutation", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [projects.go:2278]"),
    ("push_files", "", "GET", "/repos/{owner}/{repo}", "chaîne d'appels git data/contents du Go : Rust enchaîne autrement et la réponse simulée du mode test arrête la suite (DÉDUIT) [repositories_helper.go:575]"),
    ("push_files", "", "POST", "/repos/{owner}/{repo}/git/refs", "chaîne d'appels git data/contents du Go : Rust enchaîne autrement et la réponse simulée du mode test arrête la suite (DÉDUIT) [repositories_helper.go:83]"),
    ("push_files", "", "GET", "/repos/{owner}/{repo}", "chaîne d'appels git data/contents du Go : Rust enchaîne autrement et la réponse simulée du mode test arrête la suite (DÉDUIT) [repositories_helper.go:25]"),
    ("push_files", "", "PUT", "/repos/{owner}/{repo}/contents/{path}", "chaîne d'appels git data/contents du Go : Rust enchaîne autrement et la réponse simulée du mode test arrête la suite (DÉDUIT) [repositories_helper.go:43]"),
    ("request_copilot_review", "", "GET", "/repos/{owner}/{repo}", "chemin Copilot/champs d'issue non atteint en mode test (réponse GraphQL simulée sans identifiants) (DÉDUIT) [copilot.go:1022]"),
    ("ui_get", "issue_fields", "GRAPHQL", "issueFieldsRepoQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issue_fields.go:181]"),
    ("ui_get", "issue_fields", "GRAPHQL", "issueFieldsOrgQuery", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [issue_fields.go:191]"),
    ("ui_get", "labels", "GRAPHQL", "Repository", "étape GraphQL de résolution/enrichissement du Go : Rust fait un appel REST ou une requête unique (DÉDUIT, lecture du code) [ui_tools.go:148]"),
];

const TSV: &str = include_str!("data/routes_go.tsv");
const ARGS: &str = include_str!("data/routes_args.json");

fn seg(t: &[&str], p: &[&str]) -> bool {
    match (t.first(), p.first()) {
        (None, None) => true,
        // {ref} et {path} peuvent contenir des "/" (heads/x, dossier/fichier) ; les autres {x} = un segment
        (Some(&"{ref}" | &"{path}"), Some(_)) => seg(&t[1..], &p[1..]) || seg(t, &p[1..]),
        (Some(a), Some(b)) => {
            ((a.starts_with('{') && !b.is_empty()) || a == b) && seg(&t[1..], &p[1..])
        }
        _ => false,
    }
}

fn matches(tpl: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or("").trim_matches('/');
    seg(
        &tpl.trim_matches('/').split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
}

fn default_args(schema: &Value, method: &str) -> Value {
    let mut a = serde_json::Map::new();
    let props = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (k, p) in &props {
        if matches!(k.as_str(), "account" | "after" | "before" | "cursor") {
            continue;
        }
        let v = match (k.as_str(), p["type"].as_str()) {
            ("owner", _) => json!("o"),
            ("repo", _) => json!("r"),
            (_, _) if p["enum"].is_array() => p["enum"][0].clone(),
            (_, Some("integer" | "number")) => json!(1),
            (_, Some("boolean")) => json!(false),
            (_, Some("array")) => json!([]),
            (_, Some("object")) => json!({}),
            (n, _) if n == "resource_id" || n.ends_with("_id") || n.ends_with("_number") => {
                json!("1")
            }
            _ => json!("x"),
        };
        a.insert(k.clone(), v);
    }
    if !method.is_empty() {
        a.insert("method".into(), json!(method));
    }
    Value::Object(a)
}

fn merge(base: &mut Value, over: Option<&Value>) {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.and_then(Value::as_object)) {
        for (k, v) in o {
            if v.is_null() {
                b.remove(k);
            } else {
                b.insert(k.clone(), v.clone());
            }
        }
    }
}

#[tokio::test]
async fn chaque_outil_appelle_la_methode_et_la_route_du_go() {
    // Réponses à la vraie forme GitHub (data/fixtures) : les chaînes d'appels vont au bout.
    std::env::set_var(
        "GHMCP_FIXTURES",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/fixtures"),
    );
    net::init(true, net::Approver::Approve);
    let cfg = "default_account = \"t\"\n[[accounts]]\nname = \"t\"\nkind = \"classic\"\ntoken_env = \"GHMCP_ROUTES_FAKE\"\n";
    let ctx = Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), ghmcp_core::bus());
    let tools = ghmcp::registry();
    let overrides: Value = serde_json::from_str(ARGS).unwrap();

    // (outil, méthode) -> lignes (verbe, route, source) ; les lignes `?` ne sont pas rejouées
    // mais leur outil doit rester au registre.
    #[allow(clippy::type_complexity)] // table locale (verbe, gabarit) -> lignes, utilisée une fois
    let mut groups: BTreeMap<(String, String), Vec<(String, String, String)>> = BTreeMap::new();
    let mut ignores = 0;
    let mut outils: BTreeSet<String> = BTreeSet::new();
    let mut ecarts: Vec<String> = vec![];
    for l in TSV.lines().skip(1).filter(|l| !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        outils.insert(c[0].into());
        if c[2] == "?" {
            ignores += 1;
            continue;
        }
        groups.entry((c[0].into(), c[1].into())).or_default().push((
            c[2].into(),
            c[3].into(),
            c[4].into(),
        ));
    }
    for o in &outils {
        if !tools.iter().any(|t| &t.name == o) {
            ecarts.push(format!("{o}: outil officiel du tsv absent du registre"));
        }
    }

    let hit = |c: &net::Call, verbe: &str, route: &str| {
        if verbe == "GRAPHQL" {
            c.method == "POST" && c.path == "graphql"
        } else {
            c.method == verbe && matches(route, &c.path)
        }
    };
    // Comptes par ligne du tsv : verifies = une variante a produit un appel Rust qui correspond à CETTE ligne ;
    // non_atteintes = ligne justifiée dans NON_ATTEINTES (jamais comptée comme vérifiée).
    let (mut verifies, mut non_atteintes, mut justifies, mut en_ecart, mut choix_rest) =
        (0, 0, 0, 0, 0);
    for ((tool, method), rows) in &groups {
        let Some(t) = tools.iter().find(|t| &t.name == tool) else {
            continue;
        };
        let mut base = default_args(&t.published_schema(), method);
        merge(&mut base, overrides.get(tool.as_str()));
        merge(&mut base, overrides.get(format!("{tool}/{method}")));
        // Variantes : "outil/méthode#v" = liste d'objets fusionnés sur les args de base (une branche chacun).
        let mut runs = vec![base.clone()];
        for v in overrides
            .get(format!("{tool}/{method}#v"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut a = base.clone();
            merge(&mut a, Some(v));
            runs.push(a);
        }
        let attendu: Vec<String> = rows
            .iter()
            .map(|(v, p, s)| format!("{v} {p} ({s})"))
            .collect();
        let mut touchee = vec![false; rows.len()];
        for args in runs {
            net::clear_recorded();
            let r = (t.handler)(ctx.clone(), args.clone()).await;
            let calls = net::recorded_calls();
            let vus: Vec<String> = calls
                .iter()
                .map(|c| format!("{} {}", c.method, c.path))
                .collect();
            if calls.is_empty() {
                let retour = r
                    .as_ref()
                    .map(|_| "ok".into())
                    .unwrap_or_else(|e| e.to_string());
                ecarts.push(format!("{tool}/{method}: aucun appel (args {args}) ; attendu {attendu:?} ; retour {retour}"));
            }
            // TOUT appel Rust doit correspondre à une ligne Go du groupe (ou à une exception) : c'est ce sens
            // qui attrape une route ou un verbe erroné.
            for c in &calls {
                if let Some(i) = rows.iter().position(|(v, p, _)| hit(c, v, p)) {
                    let _ = i;
                    for (k, (v, p, _)) in rows.iter().enumerate() {
                        touchee[k] |= hit(c, v, p);
                    }
                } else if let Some(e) = EXCEPTIONS.iter().find(|e| {
                    e.0 == tool && e.1 == method && e.2 == c.method && matches(e.3, &c.path)
                }) {
                    justifies += 1;
                    println!(
                        "EXCEPTION {tool}/{method} {} {} : {}",
                        c.method, c.path, e.4
                    );
                } else {
                    ecarts.push(format!(
                        "{tool}/{method}: appel Rust {} {} absent du Go {attendu:?} ; vu {vus:?}",
                        c.method, c.path
                    ));
                }
            }
        }
        for (k, (v, p, s)) in rows.iter().enumerate() {
            let na = NON_ATTEINTES
                .iter()
                .find(|n| n.0 == tool && n.1 == method && n.2 == v && n.3 == p);
            if touchee[k] {
                verifies += 1;
                if na.is_some() {
                    ecarts.push(format!("{tool}/{method}: ligne {v} {p} ({s}) vérifiée mais encore dans NON_ATTEINTES : la retirer"));
                }
            } else if let Some(n) = na {
                non_atteintes += 1;
                println!("NON ATTEINTE {tool}/{method} {v} {p} ({s}) : {}", n.4);
                if let Some(c) = CHOIX_REST
                    .iter()
                    .find(|c| v == "GRAPHQL" && c.0 == tool && c.1 == method)
                {
                    choix_rest += 1;
                    println!("CHOIX REST {tool}/{method} {p} ({s}) : {}", c.2);
                }
            } else {
                en_ecart += 1;
                ecarts.push(format!("{tool}/{method}: ligne Go jamais atteinte {v} {p} ({s}) ; ajouter une variante d'arguments ou NON_ATTEINTES"));
            }
        }
    }
    for c in CHOIX_REST {
        if !groups.contains_key(&(c.0.to_string(), c.1.to_string())) {
            ecarts.push(format!(
                "CHOIX_REST {}/{} : groupe absent du tsv, retirer l'entrée",
                c.0, c.1
            ));
        }
    }
    let total = verifies + non_atteintes + en_ecart;
    let base = total - choix_rest;
    println!("ROUTES : {verifies} vérifiés (ligne par ligne, sur {total} non `?`), {non_atteintes} non atteintes justifiées dont {choix_rest} choix REST, {justifies} appels en exception, {ignores} ignorés (verbe ?), {} écarts", ecarts.len());
    println!("COUVERTURE : {verifies}/{base} = {} % (hors {choix_rest} étapes GraphQL remplacées par REST)", verifies * 100 / base.max(1));
    for e in &ecarts {
        println!("ECART {e}");
    }
    assert!(ecarts.is_empty(), "{} écarts : {ecarts:#?}", ecarts.len());
    // Critère : 90 % des lignes non `?` hors CHOIX_REST (mesuré : 280/311) ; ne jamais baisser.
    assert!(
        verifies * 100 >= base * 90,
        "seulement {verifies} lignes réellement vérifiées sur {base} (< 90 %)"
    );
}
