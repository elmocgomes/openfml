//! fml-server — the budget-process server (zero-dependency HTTP/1.1).
//!
//!   openfml-server <config-dir> <port>
//!   openfml-server token <user> [secret-file]
//!
//! The config directory holds the whole deployment, declaratively:
//!   users.cfg      user: department role     (admin | editor | viewer)
//!   access.cfg     per model: readable departments + write grants
//!   models/*.fml   the models (includes resolve inside models/)
//!   logs/<model>.log   signed event logs (created on demand)
//!   server.secret  HMAC secret (created on first run)
//!
//! Endpoints (all take token; GETs via ?token=…&model=…):
//!   GET  /models     → models the caller may read, + their role/dept
//!   GET  /state      /model   /grants   /process   /seq   (per model)
//!   GET  /log        → the signed event log (admins only)
//!   POST /patch      {token, model, name, member?, period?, value}
//!   POST /formula    {token, model, name, body}      (admins only)
//!   POST /submit     {token, model}                  (department submits)
//!   POST /reopen     {token, model, dept}            (admins)
//!   POST /lock       {token, model}                  (admins)
//!
//! Every write passes ONE gate: verify token → read access → process
//! state → role → grants → apply → sign → append. Process transitions
//! live in the same hash-chained log as the numbers — who submitted when
//! is as tamper-evident as the values themselves.

use openfml::server::{
    apply_event, gate, make_token, replay_signed, sign_line, verify_token, Access, Action,
    Event, Process, Role, User, UserStore, GENESIS,
};
use openfml::Session;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// Minimal flat-JSON object parser: string, number and null values only.
fn parse_flat_json(body: &str) -> Result<Vec<(String, String)>, String> {
    let b = body.trim();
    let inner = b
        .strip_prefix('{')
        .and_then(|x| x.strip_suffix('}'))
        .ok_or("body must be a JSON object")?;
    let mut out = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        let (key, r) = read_string(rest)?;
        rest = r.trim_start();
        rest = rest.strip_prefix(':').ok_or("expected ':'")?.trim_start();
        if rest.starts_with('"') {
            let (val, r) = read_string(rest)?;
            out.push((key, val));
            rest = r;
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            out.push((key, rest[..end].trim().to_string()));
            rest = &rest[end..];
        }
        rest = rest.trim_start();
        rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
    }
    Ok(out)
}

fn read_string(s: &str) -> Result<(String, &str), String> {
    let s = s.trim_start();
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, '"')) => {}
        _ => return Err(format!("expected string in: {s}")),
    }
    let mut out = String::new();
    let mut escape = false;
    for (i, c) in chars {
        if escape {
            out.push(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else if c == '"' {
            return Ok((out, &s[i + 1..]));
        } else {
            out.push(c);
        }
    }
    Err("unterminated string".into())
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_params(path: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some((_, q)) = path.split_once('?') {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                out.insert(k.to_string(), url_decode(v));
            }
        }
    }
    out
}

/// One evaluated line of history: a session plus its signed event chain.
/// The PRIMARY is the budget of record; VERSIONS ("3+9", "Budget V2") are
/// editable forecast copies forked from the approved baseline files, each
/// with its own signed log — reproducible as baseline + version log.
struct VState {
    session: Session,
    next_seq: u64,
    chain_tip: String,
    log_path: PathBuf,
}

struct ModelState {
    primary: VState,
    process: Process,
    versions: HashMap<String, VState>,
}

fn respond(stream: &mut std::net::TcpStream, code: u16, body: &str) {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn err_json(msg: &str) -> String {
    format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(msg))
}

fn process_json(p: &Process) -> String {
    let mut subs: Vec<&String> = p.submitted.iter().collect();
    subs.sort();
    format!(
        "{{\"locked\":{},\"submitted\":[{}]}}",
        p.locked,
        subs.iter().map(|d| format!("\"{}\"", json_escape(d))).collect::<Vec<_>>().join(",")
    )
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn respond_bytes(stream: &mut std::net::TcpStream, ctype: &str, body: &[u8]) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
}

fn content_type(name: &str) -> &'static str {
    if name.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if name.ends_with(".wasm") {
        "application/wasm"
    } else if name.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    }
}

/// Static assets: the portal (/), the studio (/studio) and their files —
/// served from <config-dir>/www if present, else ./www. Bare names only
/// (no traversal); the config dir's models/, logs/ and secret are NEVER
/// served. This makes one binary the whole deployment: UI + API, one port.
fn try_static(dir: &Path, path: &str) -> Option<(String, Vec<u8>)> {
    let name = match path {
        "/" | "/portal" => "app.html",
        "/studio" => "index.html",
        p => p.trim_start_matches('/'),
    };
    if name.is_empty() || name.contains("..") || name.contains('/') || name.starts_with('.') {
        return None;
    }
    for base in [dir.join("www"), PathBuf::from("www")] {
        let p = base.join(name);
        if p.is_file() {
            if let Ok(b) = std::fs::read(&p) {
                return Some((name.to_string(), b));
            }
        }
    }
    None
}

/// Compile + evaluate a model from the config dir's baseline files.
fn build_session(dir: &Path, file: &str) -> Session {
    let mpath = dir.join("models").join(file);
    let raw = std::fs::read_to_string(&mpath).unwrap_or_else(|e| panic!("read {}: {e}", mpath.display()));
    let base = dir.join("models");
    let exp = openfml::expand_includes_with_map(file, &raw, &mut |rel| {
        std::fs::read_to_string(base.join(rel)).map_err(|e| format!("include {rel}: {e}"))
    })
    .expect("expand includes");
    let mut session = Session::new_expanded_resolve(exp, &mut |f| {
        if f.contains('/') || f.contains("..") {
            return Err(format!("data file \"{f}\": path must be a bare file name"));
        }
        std::fs::read_to_string(base.join(f)).map_err(|e| format!("data file \"{f}\": {e}"))
    })
    .expect("compile model");
    session.run_full().expect("evaluate model");
    session
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::Admin => "admin",
        Role::Editor => "editor",
        Role::Viewer => "viewer",
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && (args[1] == "--version" || args[1] == "-V") {
        println!("openfml-server {VERSION}");
        return;
    }
    if args.len() >= 3 && args[1] == "token" {
        let path = args.get(3).map(String::as_str).unwrap_or("server.secret");
        let secret = openfml::crypto::load_or_create_secret(Path::new(path)).expect("secret");
        println!("{}", make_token(&secret, &args[2]));
        return;
    }
    if args.len() != 3 {
        eprintln!("usage: openopenfml-server <config-dir> <port>");
        eprintln!("       openfml-server token <user> [secret-file]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let port = &args[2];
    let secret = openfml::crypto::load_or_create_secret(&dir.join("server.secret")).expect("server secret");
    // Users & roles live in the mutable store (users.json); a legacy
    // users.cfg is migrated on first boot, and a Super Admin is seeded
    // if none exists — its initial password is printed once and written
    // (0600) next to the config so operators can complete first login.
    let (mut store, seeded) = UserStore::open(&dir).expect("open user store");
    if let Some((name, pw)) = &seeded {
        let pw_path = dir.join("admin-initial-password.txt");
        let _ = std::fs::write(&pw_path, format!("user: {name}\npassword: {pw}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&pw_path, std::fs::Permissions::from_mode(0o600));
        }
        eprintln!("seeded Super Admin '{name}' with initial password: {pw}");
        eprintln!("  (also written to {} — delete it after first login)", pw_path.display());
    }
    let access = Access::parse(&std::fs::read_to_string(dir.join("access.cfg")).expect("read access.cfg"))
        .expect("parse access.cfg");
    std::fs::create_dir_all(dir.join("logs")).expect("logs dir");

    // Boot every model: expand includes inside models/, replay its signed
    // log — values AND process state both come from the chain.
    let mut states: HashMap<String, ModelState> = HashMap::new();
    for ma in &access.models {
        let mut session = build_session(&dir, &ma.file);
        let mut process = Process::default();
        let log_path = dir.join("logs").join(format!("{}.log", ma.file));
        let (mut next_seq, mut chain_tip) = (1u64, GENESIS.to_string());
        if let Ok(log) = std::fs::read_to_string(&log_path) {
            let (last, tip) =
                replay_signed(&mut session, &mut process, &log, &secret).expect("replay signed event log");
            next_seq = last + 1;
            chain_tip = tip;
            eprintln!("{}: replayed {} events (chain verified)", ma.file, last);
        }
        // Versions float on the CURRENT baseline: <model>@<name>.log
        // replays over freshly-built baseline files.
        let mut versions = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(dir.join("logs")) {
            for e in entries.flatten() {
                let fname = e.file_name().to_string_lossy().into_owned();
                let prefix = format!("{}@", ma.file);
                if let Some(rest) = fname.strip_prefix(&prefix) {
                    if let Some(vname) = rest.strip_suffix(".log") {
                        let mut vsession = build_session(&dir, &ma.file);
                        let mut scratch = Process::default();
                        let (mut vseq, mut vtip) = (1u64, GENESIS.to_string());
                        if let Ok(log) = std::fs::read_to_string(e.path()) {
                            if !log.trim().is_empty() {
                                let (last, tip) = replay_signed(&mut vsession, &mut scratch, &log, &secret)
                                    .expect("replay version log");
                                vseq = last + 1;
                                vtip = tip;
                            }
                        }
                        eprintln!("{}@{vname}: version replayed ({} events)", ma.file, vseq - 1);
                        versions.insert(vname.to_string(), VState {
                            session: vsession,
                            next_seq: vseq,
                            chain_tip: vtip,
                            log_path: e.path(),
                        });
                    }
                }
            }
        }
        states.insert(ma.file.clone(), ModelState {
            primary: VState { session, next_seq, chain_tip, log_path },
            process,
            versions,
        });
    }

    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    eprintln!(
        "fml-server on 127.0.0.1:{port} — {} models, {} users, tokens via 'openfml-server token <user>'",
        states.len(),
        store.accounts.len()
    );

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let mut header_end = 0;
        loop {
            let Ok(n) = stream.read(&mut tmp) else { break };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = pos + 4;
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf[..header_end.min(buf.len())]).to_string();
        let clen: usize = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        while buf.len() < header_end + clen {
            let Ok(n) = stream.read(&mut tmp) else { break };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&buf[header_end..(header_end + clen).min(buf.len())]).to_string();
        let request_line = head.lines().next().unwrap_or_default().to_string();
        let mut parts = request_line.split_whitespace();
        let (method, full_path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let path = full_path.split('?').next().unwrap_or("");
        let q = query_params(full_path);
        let fields = if method == "POST" { parse_flat_json(&body).unwrap_or_default() } else { Vec::new() };
        let get = |k: &str| -> Option<String> {
            fields
                .iter()
                .find(|(f, _)| f == k)
                .map(|(_, v)| v.clone())
                .or_else(|| q.get(k).cloned())
        };

        // Static assets first — the UI itself needs no token.
        if method == "GET" {
            if let Some((name, bytes)) = try_static(&dir, path) {
                respond_bytes(&mut stream, content_type(&name), &bytes);
                continue;
            }
        }

        // Login exchanges credentials for the same HMAC bearer token the
        // API has always used — the only endpoint besides static assets
        // that works without one.
        if method == "POST" && path == "/login" {
            let u = get("user").unwrap_or_default();
            let p = get("password").unwrap_or_default();
            if !store.login(&u, &p) {
                respond(&mut stream, 401, &err_json("wrong user or password"));
                continue;
            }
            let a = store.find(&u).expect("login checked existence").clone();
            let base = store.as_gate_user(&a).role;
            respond(
                &mut stream,
                200,
                &format!(
                    "{{\"ok\":true,\"token\":\"{}\",\"user\":\"{}\",\"dept\":\"{}\",\"role\":\"{}\",\"roleName\":\"{}\",\"canManageUsers\":{},\"mustChange\":{}}}",
                    json_escape(&make_token(&secret, &a.name)),
                    json_escape(&a.name),
                    json_escape(&a.dept),
                    role_str(base),
                    json_escape(&a.role),
                    store.can_manage(&a),
                    a.must_change
                ),
            );
            continue;
        }

        // Identity first: every other endpoint requires a verified token.
        let Some(account) = get("token").and_then(|t| verify_token(&secret, &t)).and_then(|n| store.find(&n).cloned())
        else {
            respond(&mut stream, 401, &err_json("missing or invalid token (or unknown user)"));
            continue;
        };
        let user = store.as_gate_user(&account);

        // Own-password change (any authenticated user).
        if method == "POST" && path == "/password" {
            let old = get("old").unwrap_or_default();
            let new = get("new").unwrap_or_default();
            match store.set_own_password(&account.name, &old, &new).and_then(|_| store.save()) {
                Ok(()) => respond(&mut stream, 200, "{\"ok\":true}"),
                Err(e) => respond(&mut stream, 400, &err_json(&e)),
            }
            continue;
        }

        // ---- user & role management (Super Admin capability) ----------
        let manage_paths = ["/user_create", "/user_update", "/user_delete", "/role_create", "/role_update", "/role_delete"];
        if method == "POST" && manage_paths.contains(&path) {
            if !store.can_manage(&account) {
                respond(&mut stream, 403, &err_json("user management requires the Super Admin capability"));
                continue;
            }
            let r = match path {
                "/user_create" => {
                    let name = get("user").unwrap_or_default();
                    store.create_user(
                        &name,
                        &get("dept").unwrap_or_default(),
                        &get("role").unwrap_or_else(|| "viewer".into()),
                        get("password").as_deref(),
                    )
                }
                "/user_update" => {
                    let name = get("user").unwrap_or_default();
                    store.update_user(&name, get("dept").as_deref(), get("role").as_deref(), get("password").as_deref())
                }
                "/user_delete" => store.delete_user(&get("user").unwrap_or_default(), &account.name),
                "/role_create" => store.create_role(
                    &get("name").unwrap_or_default(),
                    &get("base").unwrap_or_default(),
                    get("manage_users").map(|v| v == "true").unwrap_or(false),
                ),
                "/role_update" => store.update_role(
                    &get("name").unwrap_or_default(),
                    get("base").as_deref(),
                    get("manage_users").map(|v| v == "true"),
                ),
                _ => store.delete_role(&get("name").unwrap_or_default()),
            };
            match r.and_then(|_| store.save()) {
                Ok(()) => respond(&mut stream, 200, "{\"ok\":true}"),
                Err(e) => respond(&mut stream, 400, &err_json(&e)),
            }
            continue;
        }
        if method == "GET" && path == "/roles" {
            if user.role != Role::Admin {
                respond(&mut stream, 403, &err_json("the role table is admin-only"));
                continue;
            }
            let items: Vec<String> = store.roles.iter().map(|r| format!(
                "{{\"name\":\"{}\",\"base\":\"{}\",\"manageUsers\":{},\"builtin\":{},\"assigned\":{}}}",
                json_escape(&r.name),
                role_str(r.base),
                r.manage_users,
                r.builtin,
                store.accounts.iter().filter(|a| a.role == r.name).count()
            )).collect();
            respond(&mut stream, 200, &format!("{{\"ok\":true,\"roles\":[{}]}}", items.join(",")));
            continue;
        }

        if method == "GET" && path == "/users" {
            if user.role != Role::Admin {
                respond(&mut stream, 403, &err_json("the user directory is admin-only"));
                continue;
            }
            let mut out = String::from("{\"ok\":true,\"users\":[");
            for (k, a) in store.accounts.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"user\":\"{}\",\"dept\":\"{}\",\"role\":\"{}\",\"roleName\":\"{}\",\"hasPassword\":{},\"mustChange\":{},\"canManageUsers\":{}}}",
                    json_escape(&a.name),
                    json_escape(&a.dept),
                    role_str(store.as_gate_user(a).role),
                    json_escape(&a.role),
                    a.pass.is_some(),
                    a.must_change,
                    store.can_manage(a)
                ));
            }
            out.push_str("]}");
            respond(&mut stream, 200, &out);
            continue;
        }
        if method == "POST" && path == "/mint" {
            if user.role != Role::Admin {
                respond(&mut stream, 403, &err_json("token minting is admin-only"));
                continue;
            }
            let Some(for_user) = get("user") else {
                respond(&mut stream, 400, &err_json("need user"));
                continue;
            };
            if store.find(&for_user).is_none() {
                respond(&mut stream, 404, &err_json("unknown user — create the account first"));
                continue;
            }
            respond(
                &mut stream,
                200,
                &format!("{{\"ok\":true,\"token\":\"{}\"}}", json_escape(&make_token(&secret, &for_user))),
            );
            continue;
        }
        if method == "GET" && path == "/models" {
            let mut out = format!(
                "{{\"ok\":true,\"version\":\"{VERSION}\",\"me\":{{\"user\":\"{}\",\"dept\":\"{}\",\"role\":\"{}\",\"roleName\":\"{}\",\"canManageUsers\":{},\"mustChange\":{}}},\"models\":[",
                json_escape(&user.name),
                json_escape(&user.dept),
                role_str(user.role),
                json_escape(&account.role),
                store.can_manage(&account),
                account.must_change
            );
            let mut first = true;
            for ma in &access.models {
                if !ma.can_read(&user) {
                    continue;
                }
                let st = &states[&ma.file];
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!(
                    "{{\"model\":\"{}\",\"process\":{}}}",
                    json_escape(&ma.file),
                    process_json(&st.process)
                ));
            }
            out.push_str("]}");
            respond(&mut stream, 200, &out);
            continue;
        }

        // Everything else is per-model: resolve + read-access check.
        let Some(model) = get("model") else {
            respond(&mut stream, 400, &err_json("missing model"));
            continue;
        };
        let Some(ma) = access.model(&model) else {
            respond(&mut stream, 404, &err_json("unknown model"));
            continue;
        };
        if !ma.can_read(&user) {
            respond(
                &mut stream,
                403,
                &err_json(&format!("{} ({}) may not access {}", user.name, user.dept, model)),
            );
            continue;
        }
        let st = states.get_mut(&model).expect("state exists for access-listed model");
        let ModelState { primary, process, versions } = st;
        // ?version=… targets a forecast copy; absent = the primary.
        let version = get("version").filter(|v| !v.is_empty());
        let in_version = version.is_some();
        // ---- version management (needs primary + versions together) ----
        if method == "GET" && path == "/versions" {
            let mut names: Vec<&String> = versions.keys().collect();
            names.sort();
            let items: Vec<String> = names
                .iter()
                .map(|n| format!(
                    "{{\"name\":\"{}\",\"seq\":{}}}",
                    json_escape(n),
                    versions[*n].next_seq - 1
                ))
                .collect();
            respond(&mut stream, 200, &format!("{{\"ok\":true,\"versions\":[{}]}}", items.join(",")));
            continue;
        }
        if method == "POST" && path == "/version" {
            if user.role == Role::Viewer {
                respond(&mut stream, 403, &err_json("viewers cannot create versions"));
                continue;
            }
            let Some(name) = get("name") else {
                respond(&mut stream, 400, &err_json("need name"));
                continue;
            };
            let ok_name = !name.is_empty()
                && name.len() <= 40
                && name.chars().all(|c| c.is_ascii_alphanumeric() || "+-_. ".contains(c))
                && !name.contains("..");
            if !ok_name {
                respond(&mut stream, 400, &err_json("version names: letters, digits, + - _ . space (max 40)"));
                continue;
            }
            if versions.contains_key(&name) {
                respond(&mut stream, 400, &err_json(&format!("version '{name}' already exists")));
                continue;
            }
            // Fork from the APPROVED BASELINE files (reproducible forever
            // as baseline + this version's own signed log). If you want
            // uncommitted primary work included, checkpoint first.
            let session = build_session(&dir, &model);
            let log_path = dir.join("logs").join(format!("{}@{}.log", model, name));
            let _ = std::fs::write(&log_path, "");
            versions.insert(name.clone(), VState {
                session,
                next_seq: 1,
                chain_tip: GENESIS.to_string(),
                log_path,
            });
            respond(&mut stream, 200, &format!("{{\"ok\":true,\"version\":\"{}\"}}", json_escape(&name)));
            continue;
        }
        if method == "POST" && path == "/version_drop" {
            if user.role != Role::Admin {
                respond(&mut stream, 403, &err_json("dropping versions is admin-only"));
                continue;
            }
            let Some(name) = get("name") else {
                respond(&mut stream, 400, &err_json("need name"));
                continue;
            };
            match versions.remove(&name) {
                Some(v) => {
                    let _ = std::fs::rename(&v.log_path, format!("{}.dropped", v.log_path.display()));
                    respond(&mut stream, 200, "{\"ok\":true}");
                }
                None => respond(&mut stream, 404, &err_json(&format!("unknown version '{name}'"))),
            }
            continue;
        }
        if method == "POST" && path == "/promote" {
            // Promotion is a management decision: the version's input cells
            // become the primary's, as ORDINARY signed patch events
            // attributed to the promoting admin — fully audited, replayable.
            if user.role != Role::Admin {
                respond(&mut stream, 403, &err_json("promoting a version is admin-only"));
                continue;
            }
            let Some(name) = get("name") else {
                respond(&mut stream, 400, &err_json("need name"));
                continue;
            };
            let Some(v) = versions.get(&name) else {
                respond(&mut stream, 404, &err_json(&format!("unknown version '{name}'")));
                continue;
            };
            let ts_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Diff every literal-editable cell; apply differences as patches.
            let mut planned: Vec<(String, Option<String>, Option<usize>, f64)> = Vec::new();
            {
                let ck = &v.session.checked;
                let mut seen = std::collections::HashSet::new();
                for (m, mb, t_opt, _, _) in &ck.edit_sites {
                    let mname = ck.measures[*m].name.clone();
                    let label = ck.tuple_label(*m, *mb);
                    if !seen.insert((mname.clone(), label.clone(), *t_opt)) {
                        continue;
                    }
                    let member = if label.is_empty() { None } else { Some(label.clone()) };
                    let probe = t_opt.or(Some(ck.measures[*m].range.0));
                    let vv = v.session.get(&mname, member.as_deref(), probe);
                    let pv = primary.session.get(&mname, member.as_deref(), probe);
                    if let (Ok(vv), Ok(pv)) = (vv, pv) {
                        if (vv - pv).abs() > 1e-9 {
                            planned.push((mname, member, *t_opt, vv));
                        }
                    }
                }
            }
            let mut applied = 0usize;
            let mut failed: Option<String> = None;
            for (mname, member, t_opt, vv) in planned {
                match primary.session.patch_input(&mname, member.as_deref(), t_opt, vv) {
                    Ok(_) => {
                        let ev = Event {
                            seq: primary.next_seq,
                            user: user.name.clone(),
                            kind: "patch".into(),
                            name: mname,
                            member,
                            period: t_opt,
                            value: vv,
                            text: None,
                            ts: ts_now,
                        };
                        append_event(primary, &secret, &ev);
                        applied += 1;
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            match failed {
                Some(e) => respond(&mut stream, 500, &err_json(&format!("promote stopped after {applied} cells: {e}"))),
                None => respond(
                    &mut stream,
                    200,
                    &format!("{{\"ok\":true,\"promoted\":\"{}\",\"cells\":{applied},\"seq\":{}}}", json_escape(&name), primary.next_seq - 1),
                ),
            }
            continue;
        }
        let vs: &mut VState = match &version {
            Some(v) => match versions.get_mut(v) {
                Some(x) => x,
                None => {
                    respond(&mut stream, 404, &err_json(&format!("unknown version '{v}' — create it with POST /version")));
                    continue;
                }
            },
            None => primary,
        };

        match (method, path) {
            ("GET", "/state") => {
                let stats = format!(
                    "\"seq\":{},\"process\":{},\"steps_run\":0,\"steps_total\":0,\"nodes_changed\":0,",
                    vs.next_seq - 1,
                    process_json(process)
                );
                let json = openfml::wasm::dump_state(&mut vs.session, &stats, false);
                respond(&mut stream, 200, &json);
            }
            ("GET", "/info") => {
                // The model's structural self-description (files/includes,
                // measure graph, dims, asserts) — any reader may see it;
                // reading the source is already granted via /model.
                respond(&mut stream, 200, &vs.session.model_info_json());
            }
            ("GET", "/explain") => {
                // The analysis drawer, server-evaluated (version-aware).
                let Some(name) = get("name") else {
                    respond(&mut stream, 400, &err_json("need name"));
                    continue;
                };
                let member = get("member").filter(|m| !m.is_empty());
                let p = get("period").and_then(|x| x.parse::<i64>().ok()).filter(|&x| x >= 0).map(|x| x as usize);
                match openfml::wasm::explain_json(&mut vs.session, &name, member.as_deref(), p) {
                    Ok(j) => respond(&mut stream, 200, &j),
                    Err(e) => respond(&mut stream, 400, &err_json(&e)),
                }
            }
            ("GET", "/tornado") => {
                let Some(name) = get("name") else {
                    respond(&mut stream, 400, &err_json("need name"));
                    continue;
                };
                let member = get("member").filter(|m| !m.is_empty());
                let p = get("period").and_then(|x| x.parse::<i64>().ok()).filter(|&x| x >= 0).map(|x| x as usize);
                let rel = get("rel").and_then(|x| x.parse().ok()).unwrap_or(0.10);
                match openfml::wasm::tornado_json(&mut vs.session, &name, member.as_deref(), p, rel) {
                    Ok(j) => respond(&mut stream, 200, &j),
                    Err(e) => respond(&mut stream, 400, &err_json(&e)),
                }
            }
            ("POST", "/goalseek") => {
                // Runtime-only solve (the session is restored); committing
                // the answer is an ordinary gated /patch from the client.
                let need = |k: &str| get(k).ok_or(format!("need {k}"));
                let r = (|| -> Result<String, String> {
                    let input = need("input")?;
                    let output = need("output")?;
                    let target: f64 = need("target")?.parse().map_err(|_| "target must be a number".to_string())?;
                    let in_member = get("in_member").filter(|m| !m.is_empty());
                    let out_member = get("out_member").filter(|m| !m.is_empty());
                    let pin = get("in_period").and_then(|x| x.parse::<i64>().ok()).filter(|&x| x >= 0).map(|x| x as usize);
                    let pout = get("out_period").and_then(|x| x.parse::<i64>().ok()).filter(|&x| x >= 0).map(|x| x as usize);
                    let gs = vs.session.goal_seek(&input, in_member.as_deref(), pin,
                        &output, out_member.as_deref(), pout, target)?;
                    Ok(format!(
                        "{{\"ok\":true,\"value\":{},\"achieved\":{},\"iterations\":{}}}",
                        gs.value, gs.achieved, gs.iterations
                    ))
                })();
                match r {
                    Ok(j) => respond(&mut stream, 200, &j),
                    Err(e) => respond(&mut stream, 400, &err_json(&e)),
                }
            }
            ("GET", "/seq") => {
                respond(
                    &mut stream,
                    200,
                    &format!("{{\"ok\":true,\"seq\":{},\"process\":{}}}", vs.next_seq - 1, process_json(process)),
                );
            }
            ("GET", "/process") => {
                respond(&mut stream, 200, &format!("{{\"ok\":true,\"process\":{}}}", process_json(process)));
            }
            ("GET", "/grants") => {
                // Effective per-user grants (directory × access × role).
                let mut out = String::from("{\"ok\":true,\"users\":[");
                for (k, a) in store.accounts.iter().enumerate() {
                    let u = store.as_gate_user(a);
                    if k > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!(
                        "{{\"user\":\"{}\",\"dept\":\"{}\",\"role\":\"{}\",\"roleName\":\"{}\",\"grants\":[",
                        json_escape(&u.name),
                        json_escape(&u.dept),
                        role_str(u.role),
                        json_escape(&a.role)
                    ));
                    for (j, g) in ma.effective_grants(&u).iter().enumerate() {
                        if j > 0 {
                            out.push(',');
                        }
                        out.push_str(&format!(
                            "{{\"measure\":\"{}\",\"member\":{}}}",
                            json_escape(&g.measure),
                            g.member
                                .as_ref()
                                .map(|m| format!("\"{}\"", json_escape(m)))
                                .unwrap_or_else(|| "null".into())
                        ));
                    }
                    out.push_str("]}");
                }
                out.push_str("]}");
                respond(&mut stream, 200, &out);
            }
            ("GET", "/model") => {
                respond(
                    &mut stream,
                    200,
                    &format!("{{\"ok\":true,\"src\":\"{}\"}}", json_escape(vs.session.source())),
                );
            }
            ("POST", "/checkpoint") => {
                // The budget-cycle persistence act (admin): write the
                // current numbers back to the model files, archive the
                // signed log, and start the next round on that baseline.
                if user.role != Role::Admin {
                    respond(&mut stream, 403, &err_json("checkpoint is admin-only"));
                    continue;
                }
                if in_version {
                    respond(&mut stream, 400, &err_json("checkpoint applies to the primary — promote the version first"));
                    continue;
                }
                let mut written = Vec::new();
                for f in primary.session.files() {
                    let path = dir.join("models").join(&f.name);
                    if let Err(e) = std::fs::write(&path, &f.text) {
                        respond(&mut stream, 500, &err_json(&format!("write {}: {e}", f.name)));
                        continue;
                    }
                    written.push(f.name.clone());
                }
                let archived = format!("{}.{}.archived", primary.log_path.display(), primary.next_seq - 1);
                if primary.log_path.exists() {
                    let _ = std::fs::rename(&primary.log_path, &archived);
                }
                primary.next_seq = 1;
                primary.chain_tip = GENESIS.to_string();
                *process = Process::default();
                respond(
                    &mut stream,
                    200,
                    &format!(
                        "{{\"ok\":true,\"files\":[{}],\"archived\":\"{}\"}}",
                        written.iter().map(|w| format!("\"{}\"", json_escape(w))).collect::<Vec<_>>().join(","),
                        json_escape(&archived)
                    ),
                );
            }
            ("GET", "/log") => {
                if user.role != Role::Admin {
                    respond(&mut stream, 403, &err_json("the audit log is admin-only"));
                    continue;
                }
                let log = std::fs::read_to_string(&vs.log_path).unwrap_or_default();
                respond(&mut stream, 200, &format!("{{\"ok\":true,\"log\":\"{}\"}}", json_escape(&log)));
            }
            ("POST", "/patch") | ("POST", "/formula") | ("POST", "/submit") | ("POST", "/reopen")
            | ("POST", "/lock") => {
                if in_version && matches!(path, "/submit" | "/reopen" | "/lock") {
                    respond(&mut stream, 400, &err_json("round actions apply to the primary, not a version"));
                    continue;
                }
                let ev = match build_event(path, &user, vs.next_seq, &get) {
                    Ok(ev) => ev,
                    Err(e) => {
                        respond(&mut stream, 400, &err_json(&e));
                        continue;
                    }
                };
                let action = match action_of(&ev) {
                    Ok(a) => a,
                    Err(e) => {
                        respond(&mut stream, 400, &err_json(&e));
                        continue;
                    }
                };
                // THE gate — versions keep role + grants but ignore round
                // state (that is their point: draft the next forecast while
                // the budget of record is submitted or locked).
                let open = Process::default();
                let gate_process: &Process = if in_version { &open } else { process };
                if let Err(e) = gate(&user, ma, gate_process, &action) {
                    respond(&mut stream, 403, &err_json(&e));
                    continue;
                }
                let mut scratch = Process::default();
                let apply_process: &mut Process = if in_version { &mut scratch } else { process };
                match apply_event(&mut vs.session, apply_process, &ev) {
                    Ok(()) => {
                        append_event(vs, &secret, &ev);
                        respond(
                            &mut stream,
                            200,
                            &format!("{{\"ok\":true,\"seq\":{},\"process\":{}}}", ev.seq, process_json(process)),
                        );
                    }
                    Err(e) => respond(&mut stream, 400, &err_json(&e)),
                }
            }
            _ => respond(&mut stream, 404, &err_json("not found")),
        }
    }
}

/// Sign an event over the state's chain tip and append it to its log.
fn append_event(vs: &mut VState, secret: &[u8], ev: &Event) {
    let line = ev.to_line();
    let sig = sign_line(secret, &vs.chain_tip, &line);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&vs.log_path)
        .expect("open log");
    writeln!(f, "{line}\t{sig}").expect("append log");
    vs.chain_tip = sig;
    vs.next_seq += 1;
}

fn build_event(
    path: &str,
    user: &User,
    next_seq: u64,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<Event, String> {
    let seq = next_seq;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match path {
        "/patch" => {
            let name = get("name").ok_or("need name")?;
            let value: f64 = get("value").ok_or("need value")?.parse().map_err(|_| "value must be a number")?;
            let member = get("member").filter(|m| m != "null" && !m.is_empty());
            let period: Option<usize> = get("period").and_then(|p| p.parse().ok());
            Ok(Event { seq, user: user.name.clone(), kind: "patch".into(), name, member, period, value, text: None, ts })
        }
        "/formula" => {
            let name = get("name").ok_or("need name")?;
            let body = get("body").ok_or("need body")?;
            Ok(Event {
                seq,
                user: user.name.clone(),
                kind: "formula".into(),
                name,
                member: None,
                period: None,
                value: 0.0,
                text: Some(body),
                ts,
            })
        }
        "/submit" => Ok(Event {
            seq,
            user: user.name.clone(),
            kind: "submit".into(),
            name: user.dept.clone(),
            member: None,
            period: None,
            value: 0.0,
            text: None,
            ts,
        }),
        "/reopen" => {
            let dept = get("dept").ok_or("need dept")?;
            Ok(Event { seq, user: user.name.clone(), kind: "reopen".into(), name: dept, member: None, period: None, value: 0.0, text: None, ts })
        }
        "/lock" => Ok(Event {
            seq,
            user: user.name.clone(),
            kind: "lock".into(),
            name: "-".into(),
            member: None,
            period: None,
            value: 0.0,
            text: None,
            ts,
        }),
        _ => Err("unknown action".into()),
    }
}

fn action_of(ev: &Event) -> Result<Action<'_>, String> {
    Ok(match ev.kind.as_str() {
        "patch" => Action::Patch { measure: &ev.name, member: ev.member.as_deref() },
        "formula" => Action::Formula,
        "submit" => Action::Submit,
        "reopen" => Action::Reopen { dept: &ev.name },
        "lock" => Action::Lock,
        other => return Err(format!("unknown kind '{other}'")),
    })
}
