//! fml-server — the collaboration gate (zero-dependency HTTP/1.1).
//!
//!   fml-server <model.fml> <owners.cfg> <events.log> <port>
//!
//! Endpoints (JSON):
//!   GET  /state                → full evaluated state + asserts
//!   GET  /model                → current source text (write-back applied)
//!   GET  /log                  → the event log
//!   POST /patch                → {"user","name","member"?,"period"?,"value"}
//!                                 ACL-checked, applied, logged. 403 on deny.
//!
//! State is event-sourced: on boot the model file is loaded and the log
//! replayed; the log line is written only after a successful apply.

use fml::server::{apply_event, make_token, replay_signed, sign_line, verify_token, Acl, Event, GENESIS};
use fml::Session;
use std::io::{Read, Write};
use std::net::TcpListener;

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

fn state_json(session: &mut Session) -> String {
    let c = &session.checked;
    let mut out = String::from("{\"ok\":true,\"series\":[");
    let mut first = true;
    for (i, mi) in c.measures.iter().enumerate() {
        for mb in 0..c.tuple_count(i) {
            if !mi.is_series {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let label = c.tuple_label(i, mb);
            let display = if label.is_empty() { mi.name.clone() } else { format!("{}[{}]", mi.name, label) };
            out.push_str(&format!("[\"{}\",[", json_escape(&display)));
            for (t, v) in session.values[i][mb].iter().enumerate() {
                if t > 0 {
                    out.push(',');
                }
                if v.is_finite() {
                    out.push_str(&format!("{v}"));
                } else {
                    out.push_str("null");
                }
            }
            out.push_str("]]");
        }
    }
    out.push_str("],\"asserts\":[");
    if let Ok(asserts) = session.run_asserts() {
        for (k, a) in asserts.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            out.push_str(&format!("[\"{}\",{}]", json_escape(&a.name), a.passed));
        }
    }
    out.push_str("]}");
    out
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `fml-server token <user> [secret-file]` — mint a bearer token.
    if args.len() >= 3 && args[1] == "token" {
        let path = args.get(3).map(String::as_str).unwrap_or("server.secret");
        let secret = fml::crypto::load_or_create_secret(std::path::Path::new(path)).expect("secret");
        println!("{}", make_token(&secret, &args[2]));
        return;
    }
    if args.len() != 5 && args.len() != 6 {
        eprintln!("usage: fml-server <model.fml> <owners.cfg> <events.log> <port> [secret-file]");
        eprintln!("       fml-server token <user> [secret-file]");
        std::process::exit(2);
    }
    let (model_path, acl_path, log_path, port) = (&args[1], &args[2], &args[3], &args[4]);
    let secret_path = args.get(5).map(String::as_str).unwrap_or("server.secret");
    let secret =
        fml::crypto::load_or_create_secret(std::path::Path::new(secret_path)).expect("server secret");

    let raw = std::fs::read_to_string(model_path).expect("read model");
    let base = std::path::Path::new(model_path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let src = fml::expand_includes(&raw, &mut |rel| {
        std::fs::read_to_string(base.join(rel)).map_err(|e| format!("include {rel}: {e}"))
    })
    .expect("expand includes");
    let acl = Acl::parse(&std::fs::read_to_string(acl_path).expect("read owners")).expect("parse owners");

    let mut session = Session::new(&src).expect("compile model");
    session.run_full().expect("evaluate model");
    let mut next_seq = 1u64;
    let mut chain_tip = GENESIS.to_string();
    if let Ok(log) = std::fs::read_to_string(log_path) {
        // Verify-then-apply: a log that fails its hash chain has been
        // modified after the fact — refuse to serve from it.
        let (last, tip) = replay_signed(&mut session, &log, &secret).expect("replay signed event log");
        next_seq = last + 1;
        chain_tip = tip;
        eprintln!("replayed {} events (chain verified)", last);
    }

    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    eprintln!(
        "fml-server on 127.0.0.1:{port} — model {model_path}, {} users, tokens via 'fml-server token <user>'",
        acl.users.len()
    );

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        // Read headers (+ body via Content-Length).
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
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

        match (method, path) {
            ("GET", "/state") => {
                let stats = format!(
                    "\"seq\":{},\"steps_run\":0,\"steps_total\":0,\"nodes_changed\":0,",
                    next_seq - 1
                );
                let json = fml::wasm::dump_state(&mut session, &stats, false);
                respond(&mut stream, 200, &json);
            }
            ("GET", "/seq") => {
                respond(&mut stream, 200, &format!("{{\"ok\":true,\"seq\":{}}}", next_seq - 1));
            }
            ("GET", "/grants") => {
                let mut out = String::from("{\"ok\":true,\"users\":[");
                for (k, (user, grants)) in acl.users.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("{{\"user\":\"{}\",\"grants\":[", json_escape(user)));
                    for (j, g) in grants.iter().enumerate() {
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
            ("GET", "/legacy_state") => {
                let json = state_json(&mut session);
                respond(&mut stream, 200, &json);
            }
            ("GET", "/model") => {
                respond(
                    &mut stream,
                    200,
                    &format!("{{\"ok\":true,\"src\":\"{}\"}}", json_escape(session.source())),
                );
            }
            ("GET", "/log") => {
                let log = std::fs::read_to_string(log_path).unwrap_or_default();
                respond(&mut stream, 200, &format!("{{\"ok\":true,\"log\":\"{}\"}}", json_escape(&log)));
            }
            ("POST", "/patch") => {
                let fields = match parse_flat_json(&body) {
                    Ok(f) => f,
                    Err(e) => {
                        respond(&mut stream, 400, &format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
                        continue;
                    }
                };
                let get = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
                // Identity comes ONLY from a verified token — a claimed
                // "user" field is ignored.
                let Some(token) = get("token") else {
                    respond(&mut stream, 401, "{\"ok\":false,\"error\":\"missing token — get one with 'fml-server token <user>'\"}");
                    continue;
                };
                let Some(user) = verify_token(&secret, &token) else {
                    respond(&mut stream, 401, "{\"ok\":false,\"error\":\"invalid token\"}");
                    continue;
                };
                let (Some(name), Some(value)) = (get("name"), get("value")) else {
                    respond(&mut stream, 400, "{\"ok\":false,\"error\":\"need name, value\"}");
                    continue;
                };
                let member = get("member").filter(|m| m != "null" && !m.is_empty());
                let period: Option<usize> = get("period").and_then(|p| p.parse().ok());
                let Ok(value) = value.parse::<f64>() else {
                    respond(&mut stream, 400, "{\"ok\":false,\"error\":\"value must be a number\"}");
                    continue;
                };
                // THE gate: authorize, apply, log — in that order.
                if !acl.authorize(&user, &name, member.as_deref()) {
                    respond(
                        &mut stream,
                        403,
                        &format!(
                            "{{\"ok\":false,\"error\":\"{} may not write {}{}\"}}",
                            json_escape(&user),
                            json_escape(&name),
                            member.as_deref().map(|m| format!("[{m}]")).unwrap_or_default()
                        ),
                    );
                    continue;
                }
                let ev = Event { seq: next_seq, user: user.clone(), name, member, period, value };
                match apply_event(&mut session, &ev) {
                    Ok(()) => {
                        use std::io::Write as _;
                        let line = ev.to_line();
                        let sig = sign_line(&secret, &chain_tip, &line);
                        let mut f = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(log_path)
                            .expect("open log");
                        writeln!(f, "{line}\t{sig}").expect("append log");
                        chain_tip = sig;
                        next_seq += 1;
                        respond(&mut stream, 200, &format!("{{\"ok\":true,\"seq\":{}}}", ev.seq));
                    }
                    Err(e) => {
                        respond(&mut stream, 400, &format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
                    }
                }
            }
            _ => respond(&mut stream, 404, "{\"ok\":false,\"error\":\"not found\"}"),
        }
    }
}
