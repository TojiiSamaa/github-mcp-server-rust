//! ghmcp-tools-repos : outils officiels repos (portage de github-mcp-server `repositories.go`, MIT).
//! Contrat : voir l en-tête AGENTS-CONTRAT de crates/core/src/lib.rs.
use base64::Engine;
use ghmcp_core::{arg_bool, arg_i64, arg_str, req_str, Access, Ctx, GhError, Level, Result, Tool};
use serde_json::{json, Value};

type Q = Vec<(&'static str, String)>;

/// Valeur texte requise d'une réponse ; en mode test (réponse simulée) on rend la réponse telle quelle.
macro_rules! sel {
    ($v:expr, $p:expr) => {
        match $v.pointer($p).and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None if $v.get("test_mode").is_some() => return Ok($v),
            None => return Err(GhError::Other(format!("réponse inattendue: {} absent", $p))),
        }
    };
}

fn or(args: &Value) -> Result<(&str, &str)> {
    Ok((req_str(args, "owner")?, req_str(args, "repo")?))
}
fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}
/// Chemin de fichier relatif, sans `..` (repository_path.go validateRelativePath).
fn rel_path(p: &str) -> Result<String> {
    let p = p.trim_start_matches('/');
    if p.split('/').any(|s| s == "..") || p.contains('\\') {
        return Err(GhError::InvalidArgs(format!("chemin invalide: {p}")));
    }
    Ok(p.split('/').map(enc).collect::<Vec<_>>().join("/"))
}
/// page / perPage (défaut 30, 1..=100).
fn paging(args: &Value) -> Result<Q> {
    let mut q: Q = vec![("per_page", "30".into())];
    if let Some(n) = arg_i64(args, "perPage") {
        if !(1..=100).contains(&n) {
            return Err(GhError::InvalidArgs(
                "perPage must be between 1 and 100".into(),
            ));
        }
        q[0].1 = n.to_string();
    }
    if let Some(n) = arg_i64(args, "page") {
        q.push(("page", n.max(1).to_string()));
    }
    Ok(q)
}
fn opt(q: &mut Q, args: &Value, k: &'static str, api: &'static str) {
    if let Some(s) = arg_str(args, k).filter(|s| !s.is_empty()) {
        q.push((api, s.into()));
    }
}
/// Ne garde que les champs demandés (paramètre `fields`) sur un objet ou chaque objet d'un tableau.
fn project(v: Value, args: &Value) -> Value {
    let Some(f) = args
        .get("fields")
        .and_then(Value::as_array)
        .filter(|f| !f.is_empty())
    else {
        return v;
    };
    let keep = |o: &Value| match o {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| f.iter().any(|x| x.as_str() == Some(k)))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        o => o.clone(),
    };
    match v {
        Value::Array(a) => Value::Array(a.iter().map(keep).collect()),
        o => keep(&o),
    }
}
fn msg_of(r: Value, ok: Value) -> Value {
    if r.get("test_mode").is_some() {
        r
    } else {
        ok
    }
}
async fn default_branch(gh: &ghmcp_core::Gh, o: &str, r: &str) -> Result<Value> {
    gh.rest("GET", &format!("/repos/{o}/{r}"), &[], None).await
}
async fn branch_sha(gh: &ghmcp_core::Gh, o: &str, r: &str, b: &str) -> Result<Value> {
    gh.rest(
        "GET",
        &format!(
            "/repos/{o}/{r}/git/ref/heads/{}",
            b.trim_start_matches("refs/heads/")
        ),
        &[],
        None,
    )
    .await
}

async fn create_branch(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let branch = req_str(&a, "branch")?;
    let gh = ctx.gh(&a)?;
    let from = match arg_str(&a, "from_branch").filter(|s| !s.is_empty()) {
        Some(f) => f.to_string(),
        None => {
            let d = default_branch(&gh, o, r).await?;
            sel!(d, "/default_branch")
        }
    };
    let base = branch_sha(&gh, o, r, &from).await?;
    let sha = sel!(base, "/object/sha");
    gh.rest(
        "POST",
        &format!("/repos/{o}/{r}/git/refs"),
        &[],
        Some(json!({"ref": format!("refs/heads/{branch}"), "sha": sha})),
    )
    .await
}

async fn create_or_update_file(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let (path, content, message, branch) = (
        req_str(&a, "path")?,
        req_str(&a, "content")?,
        req_str(&a, "message")?,
        req_str(&a, "branch")?,
    );
    let p = rel_path(path)?;
    let gh = ctx.gh(&a)?;
    let mut body = json!({"message": message, "branch": branch,
        "content": base64::engine::general_purpose::STANDARD.encode(content)});
    let url = format!("/repos/{o}/{r}/contents/{p}");
    // Comme repositories.go:559-638 : lecture du fichier existant (404 = création), sha périmé refusé,
    // fichier existant sans sha refusé (pas d'écrasement à l'aveugle), écriture à travers un lien symbolique bloquée.
    let sha = arg_str(&a, "sha").filter(|s| !s.is_empty());
    let cur = match gh.rest("GET", &url, &[("ref", branch.into())], None).await {
        Err(GhError::Http { status: 404, .. }) => None,
        Err(e) => return Err(e),
        Ok(Value::Array(_)) => {
            return Err(GhError::Other(format!(
                "Path {path} is a directory, not a file. This tool only works with files."
            )))
        }
        Ok(v) => v
            .get("sha")
            .and_then(Value::as_str)
            .map(String::from)
            .map(|s| (s, v)),
    };
    match (sha, cur) {
        (None, Some(_)) => return Err(GhError::Other(format!("File already exists at {path}. You must provide the current file's SHA when updating. Call get_file_contents with owner={o:?}, repo={r:?}, path={path:?}, and ref={branch:?} to read the file you are about to overwrite, then retry with the sha parameter set to the blob SHA that call reports."))),
        (Some(s), Some((cs, _))) if cs != s => return Err(GhError::Other(format!("SHA mismatch: provided SHA {s} is stale. Current file SHA is {cs}. Call get_file_contents with owner={o:?}, repo={r:?}, path={path:?}, and ref={branch:?} and retry with the SHA it reports."))),
        (Some(_), Some((_, v))) if !arg_bool(&a, "allow_symlink_write").unwrap_or(false) => {
            let target = match v["type"].as_str() {
                Some("symlink") => Some(v["target"].as_str().unwrap_or("").to_string()),
                _ => match tree_entry(&gh, o, r, branch, path).await? {
                    Some(e) if e["mode"].as_str() == Some(SYMLINK_MODE) => Some(blob_text(&gh, o, r, e["sha"].as_str().unwrap_or("")).await?),
                    _ => None,
                },
            };
            if let Some(t) = target {
                return Err(GhError::Other(format!("{path} is a symbolic link to {t:?} : write blocked. Edit target: create_or_update_file path={:?}. Retarget link: allow_symlink_write=true. Replace with a file: push_files path={path:?}.", symlink_resolved(path, &t))));
            }
        }
        _ => {}
    }
    if let Some(s) = sha {
        body["sha"] = json!(s);
    }
    gh.rest("PUT", &url, &[], Some(body)).await
}

const SYMLINK_MODE: &str = "120000";

/// Entrée de l'arbre `treeish` au chemin `path`, segment par segment (repositories_helper.go:304-345 getTreeEntry).
async fn tree_entry(
    gh: &ghmcp_core::Gh,
    o: &str,
    r: &str,
    treeish: &str,
    path: &str,
) -> Result<Option<Value>> {
    let segs: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segs.len() > 64 {
        return Err(GhError::Other(format!(
            "path {path:?} exceeds Git tree traversal limit"
        )));
    }
    let mut t = treeish.split('/').map(enc).collect::<Vec<_>>().join("/");
    for (i, s) in segs.iter().enumerate() {
        let tree = gh
            .rest("GET", &format!("/repos/{o}/{r}/git/trees/{t}"), &[], None)
            .await?;
        if tree["truncated"].as_bool() == Some(true) {
            return Err(GhError::Other(format!("git tree {t:?} is truncated")));
        }
        let Some(e) = tree["tree"]
            .as_array()
            .and_then(|v| v.iter().find(|e| e["path"].as_str() == Some(s)))
        else {
            return Ok(None);
        };
        if i == segs.len() - 1 {
            return Ok(Some(e.clone()));
        }
        if e["type"].as_str() != Some("tree") {
            return Ok(None);
        }
        t = e["sha"].as_str().unwrap_or("").to_string();
    }
    Ok(None)
}

/// Texte d'un blob git (cible d'un lien symbolique), repositories_helper.go:235 getVerifiedBlob.
async fn blob_text(gh: &ghmcp_core::Gh, o: &str, r: &str, sha: &str) -> Result<String> {
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(GhError::Other(format!("malformed Git blob SHA {sha:?}")));
    }
    let b = gh
        .rest("GET", &format!("/repos/{o}/{r}/git/blobs/{sha}"), &[], None)
        .await?;
    let raw = b["content"].as_str().unwrap_or("").replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| GhError::Other(format!("blob base64 invalide: {e}")))?;
    // shortcut: le Go revérifie sha1("blob n\0"+contenu) == sha ; pas de sha1 dans le dépôt, à ajouter avec la dépendance.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Chemin visé par un lien relatif, "" s'il sort du dépôt ou est absolu (repositories_helper.go resolveRepositorySymlinkTarget).
fn symlink_resolved(link: &str, target: &str) -> String {
    if target.starts_with('/') {
        return String::new();
    }
    let mut out: Vec<&str> = link.trim_matches('/').split('/').collect();
    out.pop();
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return String::new();
                }
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

async fn create_repository(ctx: Ctx, a: Value) -> Result<Value> {
    let name = req_str(&a, "name")?;
    let mut body = json!({"name": name, "private": arg_bool(&a, "private").unwrap_or(true)});
    if let Some(d) = arg_str(&a, "description") {
        body["description"] = json!(d);
    }
    if let Some(b) = arg_bool(&a, "autoInit") {
        body["auto_init"] = json!(b);
    }
    let path = match arg_str(&a, "organization").filter(|s| !s.is_empty()) {
        Some(org) => format!("/orgs/{org}/repos"),
        None => "/user/repos".into(),
    };
    ctx.gh(&a)?.rest("POST", &path, &[], Some(body)).await
}

async fn delete_file(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let (path, message, branch) = (
        req_str(&a, "path")?,
        req_str(&a, "message")?,
        req_str(&a, "branch")?,
    );
    let p = rel_path(path)?;
    let gh = ctx.gh(&a)?;
    ctx.confirm(&a, &format!("Supprimer {path} de {o}/{r}@{branch} ?"))
        .await?;
    // Comme le Go (repositories.go:1405-1510) : suppression par git data, arbre avec sha null, puis ref déplacée.
    let base = format!("/repos/{o}/{r}/git");
    let b = branch.trim_start_matches("refs/heads/");
    let rf = branch_sha(&gh, o, r, branch).await?;
    let head = sel!(rf, "/object/sha");
    let commit = gh
        .rest("GET", &format!("{base}/commits/{head}"), &[], None)
        .await?;
    let base_tree = sel!(commit, "/tree/sha");
    let tree = json!([{"path": p, "mode": "100644", "type": "blob", "sha": null}]);
    let t = gh
        .rest(
            "POST",
            &format!("{base}/trees"),
            &[],
            Some(json!({"base_tree": base_tree, "tree": tree})),
        )
        .await?;
    let tsha = sel!(t, "/sha");
    let c = gh
        .rest(
            "POST",
            &format!("{base}/commits"),
            &[],
            Some(json!({"message": message, "tree": tsha, "parents": [head]})),
        )
        .await?;
    let csha = sel!(c, "/sha");
    gh.rest(
        "PATCH",
        &format!("{base}/refs/heads/{b}"),
        &[],
        Some(json!({"sha": csha, "force": false})),
    )
    .await?;
    Ok(json!({"commit": c, "content": null}))
}

async fn delete_repository(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let gh = ctx.gh(&a)?;
    ctx.confirm(&a, &format!("Supprimer définitivement {o}/{r} ?"))
        .await?;
    let res = gh
        .rest("DELETE", &format!("/repos/{o}/{r}"), &[], None)
        .await?;
    Ok(msg_of(
        res,
        json!({"message": "Successfully deleted repository", "repository": format!("{o}/{r}")}),
    ))
}

async fn fork_repository(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let body = arg_str(&a, "organization")
        .filter(|s| !s.is_empty())
        .map(|org| json!({"organization": org}));
    let res = ctx
        .gh(&a)?
        .rest("POST", &format!("/repos/{o}/{r}/forks"), &[], body)
        .await?;
    // GitHub répond 202 : la copie est asynchrone
    Ok(msg_of(
        res.clone(),
        json!({"message": "Fork is in progress", "fork": res}),
    ))
}

async fn get_commit(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let sha = req_str(&a, "sha")?;
    let mut res = ctx
        .gh(&a)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/commits/{}", enc(sha)),
            &paging(&a)?,
            None,
        )
        .await?;
    // detail=stats (défaut) : sans la liste des fichiers ; "files"/"full" : tout
    if !matches!(arg_str(&a, "detail"), Some("files" | "full")) {
        if let Some(m) = res.as_object_mut() {
            m.remove("files");
        }
    }
    Ok(res)
}

const BLAME_RANGES: &str = "ranges{startingLine endingLine age commit{oid messageHeadline committedDate url author{name email date}}}";

async fn get_file_blame(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let path = req_str(&a, "path")?.trim_start_matches('/');
    rel_path(path)?;
    let per = match arg_i64(&a, "perPage") {
        Some(n) if !(1..=100).contains(&n) => {
            return Err(GhError::InvalidArgs(
                "perPage must be between 1 and 100 when provided".into(),
            ))
        }
        Some(n) => n as usize,
        None => 30,
    };
    let after: usize = match arg_str(&a, "after").filter(|s| !s.is_empty()) {
        Some(s) => s
            .parse()
            .map_err(|_| GhError::InvalidArgs("invalid cursor".into()))?,
        None => 0,
    };
    let (sl, el) = (
        arg_i64(&a, "start_line").unwrap_or(0),
        arg_i64(&a, "end_line").unwrap_or(0),
    );
    let ref_ = arg_str(&a, "ref")
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD");
    let q = "query($owner:String!,$repo:String!,$ref:String!,$path:String!){repository(owner:$owner,name:$repo){defaultBranchRef{name} object(expression:$ref){__typename ... on Commit{blame(path:$path){RR}} ... on Tag{target{__typename ... on Commit{blame(path:$path){RR}}}}}}}".replace("RR", BLAME_RANGES);
    let d = ctx
        .gh(&a)?
        .graphql(
            &q,
            json!({"owner": o, "repo": r, "ref": ref_, "path": path}),
        )
        .await?;
    if d.get("test_mode").is_some() {
        return Ok(d);
    }
    let repo = &d["repository"];
    let obj = &repo["object"];
    let commit = match obj["__typename"].as_str() {
        Some("Commit") => obj,
        Some("Tag") if obj["target"]["__typename"] == "Commit" => &obj["target"],
        Some(t) => {
            return Err(GhError::Other(format!(
                "ref {ref_:?} did not resolve to a commit in {o}/{r} (resolved to {t})"
            )))
        }
        None => {
            return Err(GhError::Other(format!(
                "ref {ref_:?} was not found in {o}/{r}"
            )))
        }
    };
    let (mut ranges, mut commits, mut total, mut next) =
        (vec![], serde_json::Map::new(), 0usize, None);
    for rg in commit["blame"]["ranges"].as_array().into_iter().flatten() {
        let (mut s, mut e) = (
            rg["startingLine"].as_i64().unwrap_or(0),
            rg["endingLine"].as_i64().unwrap_or(0),
        );
        if (sl > 0 && e < sl) || (el > 0 && s > el) {
            continue;
        }
        s = s.max(sl);
        if el > 0 {
            e = e.min(el);
        }
        total += 1;
        if total - 1 < after {
            continue;
        }
        if ranges.len() >= per {
            next.get_or_insert(total - 1);
            continue;
        }
        let c = &rg["commit"];
        let oid = c["oid"].as_str().unwrap_or_default().to_string();
        ranges.push(
            json!({"starting_line": s, "ending_line": e, "age": rg["age"], "commit_sha": oid}),
        );
        commits.entry(oid).or_insert_with(|| json!({"sha": c["oid"], "message": c["messageHeadline"], "date": c["committedDate"], "url": c["url"], "author": c["author"]}));
    }
    let name = repo["defaultBranchRef"]["name"].as_str().unwrap_or(ref_);
    Ok(
        json!({"ref": if ref_ == "HEAD" { name } else { ref_ }, "path": path, "ranges": ranges, "commits": commits,
        "total_ranges": total, "next_cursor": next.map(|n| n.to_string())}),
    )
}

async fn get_file_contents(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let path = arg_str(&a, "path").unwrap_or("/");
    let p = rel_path(path)?;
    let gh = ctx.gh(&a)?;
    // Comme resolveGitReference du Go (repositories_helper.go:480-600) : sha donné, sinon ref (branche puis tag),
    // sinon branche par défaut ; le contenu est lu au sha résolu.
    let sha = match (
        arg_str(&a, "sha").filter(|s| !s.is_empty()),
        arg_str(&a, "ref").filter(|s| !s.is_empty()),
    ) {
        (Some(s), _) => Some(s.to_string()),
        (None, Some(rf)) => {
            let full = if rf.starts_with("refs/") {
                rf.trim_start_matches("refs/").to_string()
            } else {
                format!("heads/{rf}")
            };
            let got = match gh
                .rest("GET", &format!("/repos/{o}/{r}/git/ref/{full}"), &[], None)
                .await
            {
                Err(_) if !rf.starts_with("refs/") => {
                    gh.rest(
                        "GET",
                        &format!("/repos/{o}/{r}/git/ref/tags/{rf}"),
                        &[],
                        None,
                    )
                    .await?
                }
                x => x?,
            };
            Some(
                got.pointer("/object/sha")
                    .and_then(Value::as_str)
                    .unwrap_or(rf)
                    .to_string(),
            )
        }
        (None, None) => {
            let repo = gh
                .rest("GET", &format!("/repos/{o}/{r}"), &[], None)
                .await?;
            match repo.pointer("/default_branch").and_then(Value::as_str) {
                Some(db) => gh
                    .rest(
                        "GET",
                        &format!("/repos/{o}/{r}/git/ref/heads/{db}"),
                        &[],
                        None,
                    )
                    .await?
                    .pointer("/object/sha")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                None => None, // mode test sans fixture : lecture sans ref
            }
        }
    };
    let q: Q = sha.clone().map(|x| vec![("ref", x)]).unwrap_or_default();
    let v = gh
        .rest("GET", &format!("/repos/{o}/{r}/contents/{p}"), &q, None)
        .await?;
    match &v {
        Value::Array(items) => {
            let min: Vec<Value> = items.iter().map(|i| json!({"name": i["name"], "path": i["path"], "type": i["type"], "size": i["size"], "sha": i["sha"], "html_url": i["html_url"]})).collect();
            Ok(project(Value::Array(min), &a))
        }
        // Contenu non rendu (fichier >= 1 Mo, encoding "none") : l'arbre git dit si c'est un lien symbolique
        // (repositories_helper.go:193-213 inspectRepositoryFile).
        Value::Object(m)
            if m.get("type").and_then(Value::as_str) == Some("file")
                && m.get("encoding").and_then(Value::as_str) == Some("none")
                && m["size"].as_i64().unwrap_or(0) > 0 =>
        {
            let fsha = m["sha"].as_str().unwrap_or("");
            let e = tree_entry(&gh, o, r, sha.as_deref().unwrap_or("HEAD"), path).await?;
            let e = e
                .filter(|e| {
                    e["sha"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(fsha))
                })
                .ok_or_else(|| {
                    GhError::Other(format!(
                        "contents API metadata does not match the Git tree for path {path:?}"
                    ))
                })?;
            match e["mode"].as_str() {
                Some(SYMLINK_MODE) => {
                    let t = blob_text(&gh, o, r, fsha).await?;
                    Ok(
                        json!({"type": "symlink", "path": path, "sha": fsha, "target": t, "resolved_target_path": symlink_resolved(path, &t), "content": "not_returned"}),
                    )
                }
                Some("160000") => Ok(json!({"type": "submodule", "path": path, "sha": fsha})),
                _ => Ok(
                    json!({"path": m["path"], "sha": fsha, "size": m["size"], "content": "not_returned", "note": "file of 1 MB or more: content not returned", "html_url": m["html_url"], "download_url": m["download_url"]}),
                ),
            }
        }
        Value::Object(m) if m.get("type").and_then(Value::as_str) == Some("file") => {
            let raw = m
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace('\n', "");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(raw)
                .map_err(|e| GhError::Other(format!("contenu base64 invalide: {e}")))?;
            Ok(file_resource(
                o,
                r,
                sha.as_deref(),
                p.as_str(),
                m["sha"].as_str().unwrap_or(""),
                bytes,
            ))
        }
        _ => Ok(v),
    }
}

/// Comme NewToolResultResource du Go (repositories.go:1170-1190) : message + ressource embarquée `repo://` ;
/// le serveur MCP rend `_mcp_content` tel quel. shortcut: texte = UTF-8 sans NUL (Go : http.DetectContentType).
fn file_resource(
    o: &str,
    r: &str,
    sha: Option<&str>,
    path: &str,
    fsha: &str,
    bytes: Vec<u8>,
) -> Value {
    let uri = match sha {
        Some(s) => format!("repo://{o}/{r}/sha/{s}/contents/{path}"),
        None => format!("repo://{o}/{r}/contents/{path}"),
    };
    let (kind, res) = match String::from_utf8(bytes) {
        Ok(t) if !t.contains('\0') => (
            "text",
            json!({"uri": uri, "mimeType": "text/plain; charset=utf-8", "text": t}),
        ),
        Ok(t) => (
            "binary",
            json!({"uri": uri, "mimeType": "application/octet-stream", "blob": base64::engine::general_purpose::STANDARD.encode(t)}),
        ),
        Err(e) => (
            "binary",
            json!({"uri": uri, "mimeType": "application/octet-stream", "blob": base64::engine::general_purpose::STANDARD.encode(e.into_bytes())}),
        ),
    };
    json!({"_mcp_content": [{"type": "text", "text": format!("successfully downloaded {kind} file (SHA: {fsha})")}, {"type": "resource", "resource": res}]})
}

async fn latest_release(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    ctx.gh(&a)?
        .rest("GET", &format!("/repos/{o}/{r}/releases/latest"), &[], None)
        .await
}
async fn release_by_tag(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    ctx.gh(&a)?
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/releases/tags/{}", enc(req_str(&a, "tag")?)),
            &[],
            None,
        )
        .await
}

async fn get_tag(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let gh = ctx.gh(&a)?;
    let rf = gh
        .rest(
            "GET",
            &format!("/repos/{o}/{r}/git/ref/tags/{}", enc(req_str(&a, "tag")?)),
            &[],
            None,
        )
        .await?;
    let sha = sel!(rf, "/object/sha");
    if rf["object"]["type"] == "tag" {
        return gh
            .rest("GET", &format!("/repos/{o}/{r}/git/tags/{sha}"), &[], None)
            .await;
    }
    Ok(rf) // tag léger : pointe directement vers un commit
}

/// Liste paginée d'un chemin donné (page/perPage) ; `extra` ajoute des paramètres optionnels.
async fn list(
    ctx: &Ctx,
    a: &Value,
    path: String,
    extra: &[(&'static str, &'static str)],
) -> Result<Value> {
    let mut q = paging(a)?;
    for (k, api) in extra {
        opt(&mut q, a, k, api);
    }
    Ok(Value::Array(ctx.gh(a)?.paginate(&path, &q, None, 1).await?))
}
async fn list_branches(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    list(&ctx, &a, format!("/repos/{o}/{r}/branches"), &[]).await
}
async fn list_tags(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    list(&ctx, &a, format!("/repos/{o}/{r}/tags"), &[]).await
}
async fn list_commits(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let v = list(
        &ctx,
        &a,
        format!("/repos/{o}/{r}/commits"),
        &[
            ("sha", "sha"),
            ("author", "author"),
            ("path", "path"),
            ("since", "since"),
            ("until", "until"),
        ],
    )
    .await?;
    let min = v.as_array().map(|l| l.iter().map(|c| if c.get("test_mode").is_some() { c.clone() } else {
        json!({"sha": c["sha"], "html_url": c["html_url"], "commit": {"message": c["commit"]["message"], "author": c["commit"]["author"]}, "author": c["author"]["login"]})
    }).collect::<Vec<_>>());
    Ok(project(Value::Array(min.unwrap_or_default()), &a))
}
async fn list_releases(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    Ok(project(
        list(&ctx, &a, format!("/repos/{o}/{r}/releases"), &[]).await?,
        &a,
    ))
}
async fn list_collaborators(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    list(
        &ctx,
        &a,
        format!("/repos/{o}/{r}/collaborators"),
        &[("affiliation", "affiliation")],
    )
    .await
}
async fn list_starred(ctx: Ctx, a: Value) -> Result<Value> {
    let path = match arg_str(&a, "username").filter(|s| !s.is_empty()) {
        Some(u) => format!("/users/{}/starred", enc(u)),
        None => "/user/starred".into(),
    };
    list(
        &ctx,
        &a,
        path,
        &[("sort", "sort"), ("direction", "direction")],
    )
    .await
}

async fn push_files(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let (branch, message) = (req_str(&a, "branch")?, req_str(&a, "message")?);
    let files = a
        .get("files")
        .and_then(Value::as_array)
        .filter(|f| !f.is_empty())
        .ok_or_else(|| GhError::InvalidArgs("files requis (tableau non vide)".into()))?;
    let mut tree = vec![];
    for f in files {
        let (p, c) = (
            arg_str(f, "path")
                .ok_or_else(|| GhError::InvalidArgs("chaque fichier exige path".into()))?,
            arg_str(f, "content")
                .ok_or_else(|| GhError::InvalidArgs("chaque fichier exige content".into()))?,
        );
        rel_path(p)?;
        tree.push(json!({"path": p.trim_start_matches('/'), "mode": "100644", "type": "blob", "content": c}));
    }
    let gh = ctx.gh(&a)?;
    let base = format!("/repos/{o}/{r}/git");
    let rf = branch_sha(&gh, o, r, branch).await?;
    let head = sel!(rf, "/object/sha");
    let commit = gh
        .rest("GET", &format!("{base}/commits/{head}"), &[], None)
        .await?;
    let base_tree = sel!(commit, "/tree/sha");
    let t = gh
        .rest(
            "POST",
            &format!("{base}/trees"),
            &[],
            Some(json!({"base_tree": base_tree, "tree": tree})),
        )
        .await?;
    let tsha = sel!(t, "/sha");
    let c = gh
        .rest(
            "POST",
            &format!("{base}/commits"),
            &[],
            Some(json!({"message": message, "tree": tsha, "parents": [head]})),
        )
        .await?;
    let csha = sel!(c, "/sha");
    gh.rest(
        "PATCH",
        &format!("{base}/refs/heads/{branch}"),
        &[],
        Some(json!({"sha": csha})),
    )
    .await
}

async fn star(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let res = ctx
        .gh(&a)?
        .rest("PUT", &format!("/user/starred/{o}/{r}"), &[], None)
        .await?;
    Ok(msg_of(
        res,
        json!({"message": "Repository starred successfully"}),
    ))
}
async fn unstar(ctx: Ctx, a: Value) -> Result<Value> {
    let (o, r) = or(&a)?;
    let res = ctx
        .gh(&a)?
        .rest("DELETE", &format!("/user/starred/{o}/{r}"), &[], None)
        .await?;
    Ok(msg_of(
        res,
        json!({"message": "Repository unstarred successfully"}),
    ))
}

/// Outils de cette crate, concaténés par le serveur dans `registry()`.
pub fn tools() -> Vec<Tool> {
    use Access::{Read, Write};
    let rd = |t: Tool| {
        t.scopes(&["repo"])
            .perm("contents", Read)
            .level(Level::Lecture)
    };
    let wr = |t: Tool| {
        t.scopes(&["repo"])
            .perm("contents", Write)
            .level(Level::Dev)
    };
    vec![
        wr(Tool::official("create_branch", create_branch)),
        wr(Tool::official(
            "create_or_update_file",
            create_or_update_file,
        )),
        Tool::official("create_repository", create_repository)
            .scopes(&["repo"])
            .perm("administration", Write)
            .level(Level::Dev),
        wr(Tool::official("delete_file", delete_file)).approval(),
        Tool::official("delete_repository", delete_repository)
            .scopes(&["delete_repo", "repo"])
            .perm("administration", Write)
            .level(Level::Admin)
            .approval(),
        Tool::official("fork_repository", fork_repository)
            .scopes(&["repo"])
            .perm("administration", Write)
            .level(Level::Dev),
        rd(Tool::official("get_commit", get_commit)),
        rd(Tool::official("get_file_blame", get_file_blame)),
        rd(Tool::official("get_file_contents", get_file_contents)),
        rd(Tool::official("get_latest_release", latest_release)),
        rd(Tool::official("get_release_by_tag", release_by_tag)),
        rd(Tool::official("get_tag", get_tag)),
        rd(Tool::official("list_branches", list_branches)),
        rd(Tool::official("list_commits", list_commits)),
        rd(Tool::official("list_releases", list_releases)),
        Tool::official("list_repository_collaborators", list_collaborators)
            .scopes(&["repo"])
            .perm("metadata", Read)
            .level(Level::Lecture),
        Tool::official("list_starred_repositories", list_starred)
            .scopes(&["repo"])
            .perm("starring", Read)
            .level(Level::Lecture),
        rd(Tool::official("list_tags", list_tags)),
        wr(Tool::official("push_files", push_files)),
        Tool::official("star_repository", star)
            .scopes(&["repo"])
            .perm("starring", Write)
            .level(Level::Dev),
        Tool::official("unstar_repository", unstar)
            .scopes(&["repo"])
            .perm("starring", Write)
            .level(Level::Dev),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghmcp_core::{bus, Config};
    use std::sync::Arc;

    const NAMES: [&str; 21] = [
        "create_branch",
        "create_or_update_file",
        "create_repository",
        "delete_file",
        "delete_repository",
        "fork_repository",
        "get_commit",
        "get_file_blame",
        "get_file_contents",
        "get_latest_release",
        "get_release_by_tag",
        "get_tag",
        "list_branches",
        "list_commits",
        "list_releases",
        "list_repository_collaborators",
        "list_starred_repositories",
        "list_tags",
        "push_files",
        "star_repository",
        "unstar_repository",
    ];

    #[tokio::test]
    async fn vingt_et_un_outils_repondent_en_mode_test() {
        let names: Vec<String> = tools().iter().map(|t| t.name.clone()).collect();
        assert_eq!(names.len(), 21);
        for n in NAMES {
            assert!(names.iter().any(|x| x == n), "manque {n}");
        }
        std::env::set_var("GHMCP_TEST_T", "ghp_FAKE");
        let cfg = "default_account=\"a\"\n[[accounts]]\nname=\"a\"\nkind=\"classic\"\ntoken_env=\"GHMCP_TEST_T\"\n";
        let ctx = Ctx::new(Arc::new(Config::from_toml(cfg).unwrap()), bus());
        let args = json!({"owner":"acme","repo":"demo","branch":"b","path":"a.txt","content":"x","message":"m","name":"n","sha":"abc","tag":"v1","files":[{"path":"a.txt","content":"x"}]});
        for t in tools() {
            let r = (t.handler)(ctx.clone(), args.clone()).await;
            if t.needs_approval {
                assert_eq!(
                    r.unwrap_err().code(),
                    "DENIED",
                    "{} doit être refusé sans approbation",
                    t.name
                );
            } else {
                let v = r.unwrap_or_else(|e| panic!("{}: {e}", t.name));
                assert!(
                    v.to_string().contains("test_mode"),
                    "{} sans marqueur test_mode: {v}",
                    t.name
                );
            }
        }
    }

    #[test]
    fn fichier_rendu_en_ressource_embarquee_comme_le_go() {
        let v = file_resource(
            "acme",
            "demo",
            Some("abc"),
            "README.md",
            "0635b4b",
            b"# Titre\n".to_vec(),
        );
        let c = v["_mcp_content"].as_array().unwrap();
        assert_eq!(
            c[0],
            json!({"type": "text", "text": "successfully downloaded text file (SHA: 0635b4b)"})
        );
        assert_eq!(c[1]["type"], "resource");
        assert_eq!(
            c[1]["resource"],
            json!({"uri": "repo://acme/demo/sha/abc/contents/README.md", "mimeType": "text/plain; charset=utf-8", "text": "# Titre\n"})
        );
        let b = file_resource("acme", "demo", None, "a.bin", "s", vec![0, 159, 1]);
        assert_eq!(
            b["_mcp_content"][0]["text"],
            "successfully downloaded binary file (SHA: s)"
        );
        assert_eq!(b["_mcp_content"][1]["resource"]["blob"], "AJ8B");
    }

    #[test]
    fn chemins_refuses() {
        assert!(rel_path("../etc").is_err());
        assert_eq!(rel_path("/a b/c.md").unwrap(), "a%20b/c.md");
    }
}
