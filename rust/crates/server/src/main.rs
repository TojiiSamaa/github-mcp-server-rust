//! ghmcp — stdio par défaut ; `--http <addr>` = MCP HTTP ; `serve [--http <addr>]` = MCP HTTP + routes
//! d'activité + automatisations. Options : `--config <fichier>` (ou GHMCP_CONFIG), `--test` (ou MCP_TEST_MODE=1),
//! `--db <fichier>` (base d'activité, défaut à côté de la config).

mod gen_skill;

use ghmcp::{mcp_router, registry, run_stdio, Server};
use ghmcp_core::{net, Config, Ctx};
use std::path::PathBuf;
use std::sync::Arc;

fn usage() -> ! {
    eprintln!("usage: ghmcp [serve | gen-skill [FICHIER]] [--http ADDR] [--config FICHIER] [--db FICHIER] [--test]");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("gen-skill") {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "skill/github-mcp-rust/SKILL.md".into());
        match gen_skill::run(&path) {
            Ok(n) => println!("{path} : {n} outils"),
            Err(e) => {
                eprintln!("ghmcp gen-skill: {e}");
                std::process::exit(1)
            }
        }
        return;
    }
    let mut args = std::env::args().skip(1);
    let (mut serve, mut http, mut config, mut db, mut test) = (
        false,
        None::<String>,
        None::<PathBuf>,
        None::<PathBuf>,
        false,
    );
    while let Some(a) = args.next() {
        match a.as_str() {
            "serve" => serve = true,
            "--http" => http = Some(args.next().unwrap_or_else(|| usage())),
            "--config" => config = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--db" => db = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--test" => test = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    test |= matches!(
        std::env::var("MCP_TEST_MODE").as_deref(),
        Ok("1") | Ok("true")
    );
    let approver = net::Approver::parse(&std::env::var("MCP_TEST_APPROVER").unwrap_or_default());
    // Fixé une fois, jamais modifiable par un outil.
    net::init(test, approver);

    let cfg = match Config::load(config.as_deref()) {
        Ok(c) => c,
        Err(e) if test => {
            eprintln!("ghmcp: config ignorée en mode test: {e}");
            Config::from_toml("").expect("config vide")
        }
        Err(e) => {
            eprintln!("ghmcp: {e}");
            std::process::exit(1)
        }
    };
    let cfg = Arc::new(cfg);
    let bus = ghmcp_core::bus();
    let server = Server::new(Ctx::new(cfg.clone(), bus.clone()), registry());
    if test {
        eprintln!("ghmcp: MODE TEST (aucune sortie réseau, approbateur simulé: {approver:?})");
    }

    if !serve && http.is_none() {
        run_stdio(server).await;
        return;
    }
    let addr = http.unwrap_or_else(|| "127.0.0.1:8787".into());
    // Hors boucle locale, /mcp pilote les jetons GitHub : jeton d'API obligatoire (bug GITHUB-2611100633-f4v).
    if !local(&addr) && !ghmcp_activity::api_token_set(&cfg) {
        eprintln!("ghmcp: écoute {addr} hors boucle locale refusée sans jeton d'API ([webhook] api_token_env)");
        std::process::exit(2)
    }
    let garde = cfg.clone();
    let mut app = mcp_router(server).route_layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let ok = ghmcp_activity::bearer_ok(&garde, req.headers());
            async move {
                if ok {
                    next.run(req).await
                } else {
                    use axum::response::IntoResponse;
                    (axum::http::StatusCode::UNAUTHORIZED, "jeton requis").into_response()
                }
            }
        },
    ));
    if serve {
        let db = db.unwrap_or_else(|| {
            cfg.path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.join("activity.db")))
                .unwrap_or_else(|| PathBuf::from("activity.db"))
        });
        let store = Arc::new(ghmcp_activity::Store::open(&db).unwrap_or_else(|e| {
            eprintln!("ghmcp: base d'activité {}: {e}", db.display());
            std::process::exit(1)
        }));
        ghmcp_automation::spawn(cfg.clone(), store.clone(), bus.clone());
        app = app.merge(ghmcp_activity::router(ghmcp_activity::State {
            store,
            bus,
            config: cfg,
        }));
    }
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("ghmcp: écoute {addr}: {e}");
            std::process::exit(1)
        });
    eprintln!("ghmcp: HTTP sur {addr} (POST /mcp)");
    let _ = axum::serve(listener, app).await;
}

/// `127.0.0.1:p`, `[::1]:p`, `localhost:p` ; toute autre adresse (0.0.0.0 compris) est réseau.
fn local(addr: &str) -> bool {
    addr.parse::<std::net::SocketAddr>()
        .map(|a| a.ip().is_loopback())
        .unwrap_or_else(|_| addr.rsplit_once(':').is_some_and(|(h, _)| h == "localhost"))
}
