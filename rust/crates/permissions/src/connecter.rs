//! `ghmcp_connecter` / `ghmcp_autotest` : liste des systèmes dont ghmcp a besoin, leur état réel, et pour un jeton
//! manquant le lien GitHub PRÉ-REMPLI + la page de dépôt du Centre (github_acces.py : le jeton va dans Vaultwarden, jamais dans un
//! chat ni chez l'agent). L'opérateur redonne le code de passage affiché par la page : `ghmcp_connecter {code}` le vérifie auprès du Centre,
//! oublie le jeton en cache du compte (le token_cmd relit le coffre) et relance l'auto-test de ce compte.
//!
//! ```toml
//! [connecter]
//! centre_url = "http://127.0.0.1:8796"      # ou centre_url_env = "NOM"
//! token_file = "/chemin/ghmcp-centre.jeton"   # ou token_env : Bearer ghmcp du Centre (creer-jeton ghmcp)
//! proprietaire = "Exemple-Org"               # login attendu de chaque jeton, propriétaire pré-rempli des jetons fins
//! depot_test = "Exemple-Org/ghmcp-autotest"  # sondes de lecture des jetons fins et écriture bornée (label, variable)
//! adresse_publique = "https://…"            # facultatif : adresse du récepteur de webhooks (listée, non sondée)
//! ```

use super::{
    acc, audit_classic, classic_url, fine_url, parse_level, parse_scopes, probe_path, up_to, Needs,
};
use ghmcp_core::{
    arg_bool, arg_str, net, Access, AccountCfg, Config, Ctx, Gh, GhError, Kind, Result, ToolMeta,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Cfg {
    pub centre_url: Option<String>,
    pub centre_url_env: Option<String>,
    pub token_env: Option<String>,
    pub token_file: Option<PathBuf>,
    pub proprietaire: Option<String>,
    pub depot_test: Option<String>,
    pub adresse_publique: Option<String>,
}

fn cfg(c: &Config) -> Cfg {
    c.connecter
        .clone()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default()
}

const LABEL_TEST: &str = "ghmcp-autotest";
const VARIABLE_TEST: &str = "GHMCP_AUTOTEST";
/// Jeton classique des paquets (le jeton fin ne couvre pas Packages).
const GHCR_SCOPES: [&str; 1] = ["write:packages"];

pub fn role(compte: &str) -> String {
    format!("gh-{compte}")
}

/// Besoins d'un compte : son nom est le niveau (lecture|dev|cicd|securite|admin) ; `ghcr` = paquets ; autre = inconnu.
fn besoins(reg: &[ToolMeta], compte: &str) -> Option<Needs> {
    if compte == "ghcr" {
        return Some(Needs {
            scopes: GHCR_SCOPES.into_iter().collect(),
            perms: BTreeMap::new(),
            events: BTreeSet::new(),
        });
    }
    parse_level(compte).ok().map(|l| up_to(reg, l))
}

/// Lien GitHub de création pré-rempli (nom, description, propriétaire, droits minimaux) pour le compte.
pub fn url_creation(
    reg: &[ToolMeta],
    a: &AccountCfg,
    proprietaire: Option<&str>,
) -> Option<String> {
    let n = besoins(reg, &a.name)?;
    let desc = format!("ghmcp {} (Centre)", role(&a.name));
    Some(match a.kind {
        Kind::Fine => fine_url(&n, &role(&a.name), &desc, proprietaire),
        Kind::Classic => classic_url(&n, &desc),
        Kind::App => return None,
    })
}

/// Réponse de `GET /user` -> (etat, cause, login, expiration). Aucune valeur de jeton n'est lue ici.
pub fn classer_user(
    r: &Result<ghmcp_core::Raw>,
    proprietaire: Option<&str>,
) -> (&'static str, String, Option<String>, Option<String>) {
    match r {
        Err(GhError::Http { status: 401, .. }) => (
            "manquant",
            "jeton refusé par GitHub (expiré, révoqué ou faux)".into(),
            None,
            None,
        ),
        Err(GhError::Network(m)) => ("erreur", format!("GitHub injoignable : {m}"), None, None),
        Err(GhError::Http { status, message }) => (
            "erreur",
            format!("GitHub HTTP {status} : {message}"),
            None,
            None,
        ),
        // token_env vide, token_file illisible, token_cmd en échec (entrée absente du coffre) : messages sans valeur.
        Err(e) => ("manquant", e.to_string(), None, None),
        Ok(raw) => {
            let login = serde_json::from_str::<Value>(&raw.text)
                .ok()
                .and_then(|v| v.get("login").and_then(Value::as_str).map(String::from));
            let expire = raw
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("github-authentication-token-expiration"))
                .map(|(_, v)| v.clone());
            match (proprietaire, &login) {
                (Some(p), Some(l)) if !l.eq_ignore_ascii_case(p) => (
                    "insuffisant",
                    format!("jeton du compte {l}, attendu {p}"),
                    login,
                    expire,
                ),
                _ => ("ok", String::new(), login, expire),
            }
        }
    }
}

/// Réponse du Centre à `ghmcp_connecter {code}` -> nom du compte ghmcp concerné.
pub fn lire_passage(v: &Value) -> Result<String> {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("");
    let compte = v
        .get("compte")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(String::from)
        .or_else(|| role.strip_prefix("gh-").map(String::from));
    match (v.get("ok").and_then(Value::as_bool), compte) {
        (Some(true), Some(c)) => Ok(c),
        _ => Err(GhError::Other(
            "réponse du Centre sans rôle confirmé : code non vérifié".into(),
        )),
    }
}

/// Erreur d'un appel au Centre -> cause lisible (le jeton ghmcp n'est jamais dans le message).
pub fn cause_centre(e: &GhError) -> String {
    match e {
        GhError::Http { status: 401, .. } => "jeton ghmcp refusé par le Centre (python github_acces.py creer-jeton ghmcp)".into(),
        GhError::Http { status: 404, message } => format!("Centre : {message} (route absente = Centre sans github-acces à jour, ou code inconnu/expiré)"),
        GhError::Network(m) => format!("Centre injoignable : {m}"),
        e => e.to_string(),
    }
}

fn centre(c: &Cfg) -> Result<(String, String)> {
    let base = c
        .centre_url
        .clone()
        .or_else(|| {
            c.centre_url_env
                .as_ref()
                .and_then(|e| std::env::var(e).ok())
        })
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| GhError::Config("[connecter] sans centre_url ni centre_url_env".into()))?;
    let jeton = net::read_secret(
        "connecter",
        c.token_env.as_deref(),
        c.token_file.as_ref(),
        "jeton ghmcp du Centre",
    )?;
    Ok((base.trim_end_matches('/').to_string(), jeton))
}

async fn appel_centre(c: &Cfg, chemin: &str, corps: &Value) -> Result<Value> {
    let (base, jeton) = centre(c)?;
    net::post_json_bearer(&format!("{base}{chemin}"), Some(&jeton), corps).await
}

fn ko(code: &GhError) -> bool {
    matches!(
        code,
        GhError::Http {
            status: 401 | 403 | 404,
            ..
        }
    )
}

/// Auto-test d'un compte : identité (`GET /user`), droits réels (scopes classiques ; sondes de lecture d'un jeton fin sur le dépôt de
/// test), puis, si `ecriture`, une écriture bornée et défaite sur le dépôt de test (label si issues:write, variable si variables:write).
async fn tester_compte(
    reg: &[ToolMeta],
    a: &AccountCfg,
    c: &Cfg,
    ecriture: bool,
    libre: bool,
) -> Value {
    let gh = Gh::new(a.clone());
    let r = gh.rest_raw("GET", "/user", &[], None).await;
    let (mut etat, mut cause, login, expire) = classer_user(&r, c.proprietaire.as_deref());
    let mut v = json!({"systeme": "github", "compte": a.name, "role": role(&a.name), "kind": format!("{:?}", a.kind).to_lowercase()});
    let mut manque: Vec<String> = Vec::new();
    let mut verifie: Vec<String> = Vec::new();
    if let (Ok(raw), Some(n)) = (&r, besoins(reg, &a.name)) {
        if etat == "ok" {
            match a.kind {
                Kind::Classic => match raw
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("x-oauth-scopes"))
                {
                    Some((_, h)) => {
                        let g = parse_scopes(h);
                        let need: BTreeSet<&str> = n.scopes.iter().copied().collect();
                        let miss: Vec<String> = match parse_level(&a.name) {
                            Ok(l) => serde_json::from_value(
                                audit_classic(reg, l, &g)["missing_scopes"].clone(),
                            )
                            .unwrap_or_default(),
                            Err(_) => need
                                .iter()
                                .filter(|s| !super::scope_granted(&g, s))
                                .map(|s| s.to_string())
                                .collect(),
                        };
                        manque.extend(miss.into_iter().map(|s| format!("scope {s}")));
                        verifie.push("scopes".into());
                    }
                    None => verifie.push("scopes non lus (en-tête X-OAuth-Scopes absent)".into()),
                },
                Kind::Fine => match &c.depot_test {
                    Some(repo) => {
                        for p in n.perms.keys() {
                            let Some(sfx) = probe_path(p) else { continue };
                            match gh
                                .rest_raw(
                                    "GET",
                                    &format!("/repos/{repo}{sfx}"),
                                    &[("per_page", "1".into())],
                                    None,
                                )
                                .await
                            {
                                Ok(_) => verifie.push(format!("{p}:read")),
                                Err(e) if ko(&e) => manque.push(format!("{p}:read")),
                                Err(e) => manque.push(format!("{p}:read non sondé ({e})")),
                            }
                        }
                    }
                    None => {
                        verifie.push("droits non sondés ([connecter] depot_test absent)".into())
                    }
                },
                Kind::App => {
                    verifie.push("GitHub App : droits sur la page de l'installation".into())
                }
            }
            if let (true, Some(repo)) = (ecriture, &c.depot_test) {
                for (perm, creer, corps, defaire) in [
                    (
                        "issues",
                        format!("/repos/{repo}/labels"),
                        json!({"name": LABEL_TEST, "color": "ededed"}),
                        format!("/repos/{repo}/labels/{LABEL_TEST}"),
                    ),
                    (
                        "variables",
                        format!("/repos/{repo}/actions/variables"),
                        json!({"name": VARIABLE_TEST, "value": "1"}),
                        format!("/repos/{repo}/actions/variables/{VARIABLE_TEST}"),
                    ),
                ] {
                    if n.perms.get(perm) != Some(&Access::Write) {
                        continue;
                    }
                    // la borne d'écriture (server/ecriture.rs) refuse aux agents les écritures cicd+ : la variable n'est pas testée
                    if perm == "variables" && !libre {
                        verifie.push("variables:write non testé (écriture bornée ; [ecriture] mode = \"libre\" pour le tester)".into());
                        continue;
                    }
                    match gh.rest_raw("POST", &creer, &[], Some(corps)).await {
                        // 422 = déjà là (test précédent interrompu) : l'écriture est permise
                        Ok(_) | Err(GhError::Http { status: 422, .. }) => {
                            let _ = gh.rest_raw("DELETE", &defaire, &[], None).await;
                            verifie
                                .push(format!("{perm}:{} (écriture défaite)", acc(Access::Write)));
                        }
                        Err(e) => manque.push(format!("{perm}:write ({e})")),
                    }
                }
            }
            if !manque.is_empty() {
                (etat, cause) = (
                    "insuffisant",
                    format!("droits manquants : {}", manque.join(", ")),
                );
            }
        }
    }
    v["etat"] = json!(etat);
    v["cause"] = json!(cause);
    v["login"] = json!(login);
    v["expire"] = json!(expire);
    v["verifie"] = json!(verifie);
    v
}

/// Tous les systèmes, testés : comptes GitHub (un rôle chacun), Centre (Bearer ghmcp), secret de webhook, adresse publique.
async fn systemes(ctx: &Ctx, compte: Option<&str>, ecriture: bool) -> Result<Vec<Value>> {
    let conf = ctx.config();
    let c = cfg(conf);
    let reg = ctx.registry();
    let mut out = Vec::new();
    for a in conf
        .accounts
        .iter()
        .filter(|a| compte.is_none() || compte == Some(a.name.as_str()))
    {
        out.push(tester_compte(reg, a, &c, ecriture, libre(conf)).await);
    }
    if compte.is_some() && out.is_empty() {
        return Err(GhError::UnknownAccount {
            name: compte.unwrap_or("").into(),
            known: conf.account_names(),
        });
    }
    // coffre : lu par le token_cmd des comptes (node jeton.mjs -> Vaultwarden) ; un échec de lecture se voit dans la cause du compte
    let par_cmd: Vec<&AccountCfg> = conf
        .accounts
        .iter()
        .filter(|a| a.token_cmd.is_some())
        .collect();
    let echecs: Vec<String> = out
        .iter()
        .filter(|x| x["cause"].as_str().is_some_and(|c| c.contains("token_cmd")))
        .filter_map(|x| x["compte"].as_str().map(String::from))
        .collect();
    out.push(match (par_cmd.is_empty(), echecs.is_empty()) {
        (true, _) => json!({"systeme": "coffre", "etat": "non_configure", "cause": "aucun compte à token_cmd"}),
        (false, true) => json!({"systeme": "coffre", "etat": "ok", "cause": ""}),
        (false, false) => json!({"systeme": "coffre", "etat": "manquant", "cause": format!("token_cmd en échec pour : {}", echecs.join(", "))}),
    });
    let centre_etat = match appel_centre(&c, "/api/github-acces/ping", &json!({})).await {
        Ok(_) => json!({"systeme": "centre", "etat": "ok", "cause": ""}),
        Err(e @ GhError::Config(_)) => {
            json!({"systeme": "centre", "etat": "manquant", "cause": e.to_string()})
        }
        Err(e) => json!({"systeme": "centre", "etat": "erreur", "cause": cause_centre(&e)}),
    };
    out.push(centre_etat);
    let secret_env = conf
        .webhook
        .as_ref()
        .and_then(|w| w.get("secret_env"))
        .and_then(|v| v.as_str());
    out.push(match secret_env {
        None => json!({"systeme": "webhook", "etat": "non_configure", "cause": "[webhook] secret_env absent (facultatif : réception des événements GitHub)"}),
        Some(e) if std::env::var(e).is_ok_and(|v| !v.is_empty()) => json!({"systeme": "webhook", "etat": "ok", "cause": ""}),
        Some(e) => json!({"systeme": "webhook", "etat": "manquant", "cause": format!("variable {e} vide (secret de signature des webhooks)")}),
    });
    // shortcut: adresse publique listée, pas sondée (un GET de l'extérieur demande un service tiers) ; sonder si le récepteur est utilisé.
    out.push(match &c.adresse_publique {
        Some(u) => json!({"systeme": "adresse_publique", "etat": "non_verifie", "cause": "", "url": u}),
        None => json!({"systeme": "adresse_publique", "etat": "non_configure", "cause": "[connecter] adresse_publique absente (facultatif)"}),
    });
    Ok(out)
}

fn libre(c: &Config) -> bool {
    c.ecriture
        .as_ref()
        .and_then(|e| e.get("mode"))
        .and_then(|m| m.as_str())
        == Some("libre")
}

fn tout_ok(s: &[Value]) -> bool {
    s.iter().all(|x| {
        matches!(
            x["etat"].as_str(),
            Some("ok" | "non_configure" | "non_verifie")
        )
    })
}

pub async fn autotest(ctx: Ctx, args: Value) -> Result<Value> {
    let ecriture = arg_bool(&args, "ecriture").unwrap_or(true);
    let s = systemes(&ctx, arg_str(&args, "compte"), ecriture).await?;
    Ok(json!({"ok": tout_ok(&s), "systemes": s}))
}

pub async fn connecter(ctx: Ctx, args: Value) -> Result<Value> {
    let c = cfg(ctx.config());
    if let Some(code) = arg_str(&args, "code").filter(|s| !s.trim().is_empty()) {
        let v = appel_centre(
            &c,
            "/api/github-acces/jeton/passage",
            &json!({"code": code}),
        )
        .await
        .map_err(|e| GhError::Other(cause_centre(&e)))?;
        let compte = lire_passage(&v)?;
        let a = ctx.config().account(Some(&compte))?.clone();
        net::forget_token(&a).await;
        let t = tester_compte(ctx.registry(), &a, &c, true, libre(ctx.config())).await;
        let ok = t["etat"] == "ok";
        return Ok(
            json!({"code": "vérifié par le Centre", "compte": compte, "autotest": t,
            "suite": if ok { "compte prêt ; rappelle ghmcp_connecter (sans code) pour les systèmes restants" } else { "le jeton rangé ne passe pas l'auto-test : voir autotest.cause, puis recommencer avec un nouveau lien" }}),
        );
    }
    let s = systemes(&ctx, arg_str(&args, "compte"), false).await?;
    let reg = ctx.registry();
    let mut a_faire = Vec::new();
    let centre_ok = s
        .iter()
        .any(|x| x["systeme"] == "centre" && x["etat"] == "ok");
    for x in s
        .iter()
        .filter(|x| x["systeme"] == "github" && x["etat"] != "ok")
    {
        let Ok(a) = ctx.config().account(x["compte"].as_str()) else {
            continue;
        };
        let creer = url_creation(reg, a, c.proprietaire.as_deref());
        let mut etape =
            json!({"compte": a.name, "role": role(&a.name), "cause": x["cause"], "creer": creer});
        if centre_ok {
            match appel_centre(
                &c,
                "/api/github-acces/jeton/demande",
                &json!({"role": role(&a.name), "compte": a.name, "url_creation": creer}),
            )
            .await
            {
                Ok(d) => etape["deposer"] = d.get("url").cloned().unwrap_or(Value::Null),
                Err(e) => etape["deposer_erreur"] = json!(cause_centre(&e)),
            }
        }
        etape["puis"] = json!("L'opérateur crée le jeton (lien creer), le colle dans la page deposer (jamais dans un chat), puis redonne le code affiché : ghmcp_connecter {code}");
        a_faire.push(etape);
    }
    Ok(json!({"ok": tout_ok(&s), "systemes": s, "a_faire": a_faire,
        "note": "aucune valeur de secret ne transite par cet outil ; un Centre en erreur se règle d'abord (jeton ghmcp : python github_acces.py creer-jeton ghmcp)"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{Level, Raw, Tool};
    use std::sync::Arc;

    fn reg() -> Vec<ToolMeta> {
        let h = |_c: Ctx, _a: Value| async { Ok(json!(null)) };
        vec![
            Tool::new("gh_a", "a", json!({}), h)
                .scopes(&["repo"])
                .perm("contents", Access::Read)
                .level(Level::Lecture)
                .meta(),
            Tool::new("gh_b", "b", json!({}), h)
                .scopes(&["repo"])
                .perm("issues", Access::Write)
                .level(Level::Dev)
                .meta(),
            Tool::new("gh_c", "c", json!({}), h)
                .scopes(&["workflow"])
                .perm("variables", Access::Write)
                .level(Level::Cicd)
                .meta(),
        ]
    }

    fn compte(name: &str, kind: Kind) -> AccountCfg {
        let k = if kind == Kind::Fine {
            "fine"
        } else {
            "classic"
        };
        let t = format!(
            "[[accounts]]
name = \"{name}\"
kind = \"{k}\"
token_env = \"GHMCP_C_NONE\"
"
        );
        Config::from_toml(&t).unwrap().accounts.remove(0)
    }

    const CONF: &str = "default_account = \"lecture\"\n[[accounts]]\nname = \"lecture\"\nkind = \"fine\"\ntoken_env = \"GHMCP_C_NONE\"\n[[accounts]]\nname = \"dev\"\nkind = \"fine\"\ntoken_env = \"GHMCP_C_NONE\"\n[[accounts]]\nname = \"ghcr\"\nkind = \"classic\"\ntoken_env = \"GHMCP_C_NONE\"\n[connecter]\ncentre_url = \"http://centre.invalid:8796\"\ntoken_env = \"GHMCP_C_CENTRE\"\nproprietaire = \"Exemple-Org\"\ndepot_test = \"Exemple-Org/ghmcp-autotest\"\n";

    fn ctx() -> Ctx {
        std::env::set_var("GHMCP_C_CENTRE", "jeton-de-test");
        let c = Ctx::new(
            Arc::new(Config::from_toml(CONF).unwrap()),
            ghmcp_core::bus(),
        );
        c.set_registry(reg());
        c
    }

    fn raw(text: &str, headers: &[(&str, &str)]) -> Raw {
        Raw {
            status: 200,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            text: text.into(),
        }
    }

    #[test]
    fn url_fine_pre_remplie_avec_droits_minimaux() {
        let u = url_creation(&reg(), &compte("dev", Kind::Fine), Some("Exemple-Org")).unwrap();
        assert!(
            u.starts_with("https://github.com/settings/personal-access-tokens/new?name=gh-dev&"),
            "{u}"
        );
        assert!(
            u.contains("&target_name=Exemple-Org")
                && u.contains("&issues=write")
                && u.contains("&contents=read"),
            "{u}"
        );
        assert!(
            !u.contains("variables"),
            "le niveau dev ne demande pas les droits cicd : {u}"
        );
        let u = url_creation(&reg(), &compte("ghcr", Kind::Classic), None).unwrap();
        assert!(
            u.starts_with(
                "https://github.com/settings/tokens/new?scopes=write%3Apackages&description="
            ),
            "{u}"
        );
        assert!(url_creation(&reg(), &compte("inconnu", Kind::Fine), None).is_none());
    }

    #[test]
    fn classement_de_get_user() {
        let e = Err(GhError::Http {
            status: 401,
            message: "Bad credentials".into(),
        });
        assert_eq!(classer_user(&e, None).0, "manquant");
        let e = Err(GhError::Config("compte dev : token_cmd en échec".into()));
        let (etat, cause, ..) = classer_user(&e, None);
        assert_eq!((etat, cause.contains("token_cmd")), ("manquant", true));
        assert_eq!(
            classer_user(&Err(GhError::Network("dns".into())), None).0,
            "erreur"
        );
        let ok = Ok(raw(
            "{\"login\":\"Exemple-Org\"}",
            &[(
                "github-authentication-token-expiration",
                "2027-02-10 00:00:00 UTC",
            )],
        ));
        let (etat, _, login, exp) = classer_user(&ok, Some("exemple-org"));
        assert_eq!(
            (etat, login.as_deref(), exp.as_deref()),
            ("ok", Some("Exemple-Org"), Some("2027-02-10 00:00:00 UTC"))
        );
        let autre = Ok(raw("{\"login\":\"quelquun\"}", &[]));
        assert_eq!(classer_user(&autre, Some("Exemple-Org")).0, "insuffisant");
    }

    #[test]
    fn passage_exige_ok_et_role() {
        assert_eq!(
            lire_passage(&json!({"ok": true, "role": "gh-dev", "compte": "dev"})).unwrap(),
            "dev"
        );
        assert_eq!(
            lire_passage(&json!({"ok": true, "role": "gh-cicd", "compte": ""})).unwrap(),
            "cicd"
        );
        assert!(lire_passage(&json!({"test_mode": true, "id": "session-test"})).is_err());
        assert!(lire_passage(&json!({"ok": false, "role": "gh-dev"})).is_err());
        assert!(cause_centre(&GhError::Http {
            status: 401,
            message: "x".into()
        })
        .contains("creer-jeton ghmcp"));
    }

    #[tokio::test]
    async fn autotest_sans_reseau_sonde_et_ecrit_borne() {
        let v = autotest(ctx(), json!({"compte": "dev"})).await.unwrap();
        let s = v["systemes"].as_array().unwrap();
        let gh = &s[0];
        assert_eq!(
            (gh["compte"].as_str(), gh["etat"].as_str()),
            (Some("dev"), Some("ok")),
            "{v}"
        );
        assert!(
            s.iter()
                .any(|x| x["systeme"] == "centre" && x["etat"] == "ok"),
            "{v}"
        );
        assert!(s
            .iter()
            .any(|x| x["systeme"] == "webhook" && x["etat"] == "non_configure"));
        let calls: Vec<String> = ghmcp_core::recorded_calls()
            .iter()
            .map(|c| format!("{} {}", c.method, c.path))
            .collect();
        for attendu in [
            "GET /user",
            "GET /repos/Exemple-Org/ghmcp-autotest/contents",
            "GET /repos/Exemple-Org/ghmcp-autotest/issues",
            "POST /repos/Exemple-Org/ghmcp-autotest/labels",
            "DELETE /repos/Exemple-Org/ghmcp-autotest/labels/ghmcp-autotest",
        ] {
            assert!(
                calls.iter().any(|c| c == attendu),
                "{attendu} absent de {calls:?}"
            );
        }
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("ghmcp-autotest/actions/variables")),
            "dev n'écrit pas de variable : {calls:?}"
        );
        assert!(autotest(ctx(), json!({"compte": "absent"})).await.is_err());
    }

    #[tokio::test]
    async fn connecter_liste_et_code() {
        let v = connecter(ctx(), json!({})).await.unwrap();
        assert!(v["systemes"].as_array().unwrap().len() >= 7, "{v}"); // 3 comptes + coffre + centre + webhook + adresse
        assert!(
            v["a_faire"].as_array().unwrap().is_empty(),
            "mode test : tout répond ok : {v}"
        );
        // le Centre simulé ne confirme aucun rôle : un code n'est jamais « vérifié » sans sa réponse
        let e = connecter(ctx(), json!({"code": "ABCD-EFGH"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("code non vérifié"), "{e}");
        let posts = net::recorded_posts();
        let p = posts
            .iter()
            .find(|(u, _)| u.ends_with("/api/github-acces/jeton/passage"))
            .expect("passage envoyé au Centre");
        assert_eq!(
            (p.0.as_str(), &p.1),
            (
                "http://centre.invalid:8796/api/github-acces/jeton/passage",
                &json!({"code": "ABCD-EFGH"})
            )
        );
        assert!(
            !format!("{posts:?}").contains("jeton-de-test"),
            "le Bearer n'est jamais enregistré"
        );
    }

    #[tokio::test]
    async fn variable_testee_seulement_en_ecriture_libre() {
        for (mode, depot, attendu) in [
            ("borne", "Exemple-Org/autotest-borne", false),
            ("libre", "Exemple-Org/autotest-libre", true),
        ] {
            let conf = format!(
                "[[accounts]]
name = \"cicd\"
kind = \"fine\"
token_env = \"GHMCP_C_NONE\"
[ecriture]
mode = \"{mode}\"
[connecter]
depot_test = \"{depot}\"
"
            );
            let c = Ctx::new(
                Arc::new(Config::from_toml(&conf).unwrap()),
                ghmcp_core::bus(),
            );
            c.set_registry(reg());
            let v = autotest(c, json!({"compte": "cicd"})).await.unwrap();
            let ecrit = ghmcp_core::recorded_calls().iter().any(|x| {
                x.method == "POST" && x.path == format!("/repos/{depot}/actions/variables")
            });
            assert_eq!(ecrit, attendu, "{mode} : {v}");
            assert_eq!(v["systemes"][0]["etat"], "ok", "{v}");
        }
    }

    #[test]
    fn centre_sans_config_est_manquant() {
        let e = centre(&Cfg::default()).unwrap_err();
        assert!(matches!(e, GhError::Config(ref m) if m.contains("[connecter]")));
    }
}
