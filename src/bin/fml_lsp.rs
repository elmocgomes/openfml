//! fml-lsp — a zero-dependency Language Server Protocol server (stdio).
//!
//! Capabilities: full-document sync, diagnostics (resilient parse +
//! salvage: broken declarations and their dropped dependents), hover
//! (unit, kind, range, formula, live values), go-to-definition (include-
//! aware: lands in the owning file) and document symbols. Reloads run
//! through the salsa path, so trivia keystrokes reuse the whole analysis.
//!
//! Wire it to any editor as a generic LSP for `.fml` files:
//!   command: fml-lsp     (no arguments, stdio transport)

use fml::json::{parse, J};
use fml::{Expanded, Session};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

struct Doc {
    text: String,
    session: Option<Session>,
}

fn main() {
    let mut docs: HashMap<String, Doc> = HashMap::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let Some(msg) = read_message(&mut input) else { break };
        let Ok(msg) = parse(&msg) else { continue };
        let method = msg.get("method").and_then(J::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        match method {
            "initialize" => {
                let caps = J::obj(vec![
                    ("textDocumentSync", J::n(1.0)), // full
                    ("hoverProvider", J::B(true)),
                    ("definitionProvider", J::B(true)),
                    ("documentSymbolProvider", J::B(true)),
                    ("completionProvider", J::obj(vec![("triggerCharacters", J::A(vec![]))])),
                ]);
                respond(id, J::obj(vec![
                    ("capabilities", caps),
                    ("serverInfo", J::obj(vec![("name", J::s("fml-lsp"))])),
                ]));
            }
            "initialized" => {}
            "shutdown" => respond(id, J::Null),
            "exit" => break,
            "textDocument/didOpen" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("").to_string();
                let text = msg.path("params.textDocument.text").and_then(J::as_str).unwrap_or("").to_string();
                open_or_change(&mut docs, &uri, text);
            }
            "textDocument/didChange" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("").to_string();
                let text = msg
                    .path("params.contentChanges")
                    .and_then(J::as_arr)
                    .and_then(|a| a.last())
                    .and_then(|c| c.get("text"))
                    .and_then(J::as_str)
                    .unwrap_or("")
                    .to_string();
                open_or_change(&mut docs, &uri, text);
            }
            "textDocument/didClose" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("");
                docs.remove(uri);
            }
            "textDocument/hover" => {
                let r = with_doc_pos(&docs, &msg, |doc, s, off| {
                    let name = s.measure_at(0, off)?;
                    let md = s.hover_info(&name)?;
                    let _ = doc;
                    Some(J::obj(vec![(
                        "contents",
                        J::obj(vec![("kind", J::s("markdown")), ("value", J::S(md))]),
                    )]))
                });
                respond(id, r.unwrap_or(J::Null));
            }
            "textDocument/definition" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("").to_string();
                let r = with_doc_pos(&docs, &msg, |doc, s, off| {
                    let name = s.measure_at(0, off)?;
                    let (file, line) = s.definition_of(&name)?;
                    // files[0] is the document itself; others are includes
                    // resolved next to it on disk.
                    let target = if file == s.files()[0].name || file == "model" || file == "main" {
                        uri.clone()
                    } else {
                        sibling_uri(&uri, &file)
                    };
                    let _ = doc;
                    let pos = J::obj(vec![("line", J::n((line - 1) as f64)), ("character", J::n(0.0))]);
                    Some(J::obj(vec![
                        ("uri", J::S(target)),
                        ("range", J::obj(vec![("start", pos.clone()), ("end", pos)])),
                    ]))
                });
                respond(id, r.unwrap_or(J::Null));
            }
            "textDocument/completion" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("");
                let mut items = Vec::new();
                if let Some(doc) = docs.get(uri) {
                    if let Some(s) = &doc.session {
                        for (name, kind, detail) in s.completions() {
                            let k = match kind {
                                "keyword" => 14.0,
                                "member" | "group" => 20.0,
                                "unit" => 11.0,
                                "dimension" => 7.0,
                                "range" => 21.0,
                                _ => 6.0, // measures & inputs: Variable
                            };
                            let detail_s = if detail.is_empty() {
                                kind.to_string()
                            } else {
                                format!("{kind} · {detail}")
                            };
                            items.push(J::obj(vec![
                                ("label", J::S(name)),
                                ("kind", J::n(k)),
                                ("detail", J::S(detail_s)),
                            ]));
                        }
                    }
                }
                respond(id, J::A(items));
            }
            "textDocument/documentSymbol" => {
                let uri = msg.path("params.textDocument.uri").and_then(J::as_str).unwrap_or("");
                let mut out = Vec::new();
                if let Some(doc) = docs.get(uri) {
                    if let Some(s) = &doc.session {
                        let main = s.files()[0].name.clone();
                        for (name, kind, file, line) in s.symbols() {
                            if file != main {
                                continue; // symbols of THIS document only
                            }
                            let pos = J::obj(vec![("line", J::n((line - 1) as f64)), ("character", J::n(0.0))]);
                            out.push(J::obj(vec![
                                ("name", J::S(name)),
                                ("detail", J::s(kind)),
                                ("kind", J::n(13.0)), // Variable
                                ("range", J::obj(vec![("start", pos.clone()), ("end", pos.clone())])),
                                ("selectionRange", J::obj(vec![("start", pos.clone()), ("end", pos)])),
                            ]));
                        }
                    }
                }
                respond(id, J::A(out));
            }
            _ => {
                if id.is_some() {
                    respond(id, J::Null);
                }
            }
        }
    }
}

/// (Re)analyze a document: expand includes from disk, reload the session
/// through the salsa path, publish diagnostics.
fn open_or_change(docs: &mut HashMap<String, Doc>, uri: &str, text: String) {
    let dir = uri_to_path(uri).and_then(|p| p.parent().map(Path::to_path_buf));
    let name = uri_to_path(uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "model.fml".into());
    let exp = fml::expand_includes_with_map(&name, &text, &mut |rel| {
        let base = dir.clone().unwrap_or_default();
        std::fs::read_to_string(base.join(rel)).map_err(|e| format!("include {rel}: {e}"))
    });
    let prev = docs.remove(uri).and_then(|d| d.session);
    let (session, diags) = match exp {
        Err(e) => (None, vec![diag(&text, &e, 1)]),
        Ok(exp) => match try_load(prev, exp.clone()) {
            Ok(s) => (Some(s), Vec::new()),
            Err(_) => {
                // Salvage view: per-declaration errors + dropped dependents.
                let mut ds = Vec::new();
                match fml::parse_salvage(&exp.flat) {
                    Ok(sal) => {
                        for e in &sal.errors {
                            ds.push(diag(&text, &e.msg, e.line));
                        }
                        if sal.errors.is_empty() {
                            // Parse fine → check-level error; extract line.
                            if let Err(ce) = fml::compile(&exp.flat) {
                                let line = ce
                                    .strip_prefix("line ")
                                    .and_then(|r| r.split(':').next())
                                    .and_then(|n| n.parse().ok())
                                    .unwrap_or(1);
                                ds.push(diag(&text, &ce, line));
                            }
                        }
                        for (who, why) in sal.dropped.iter().take(6) {
                            ds.push(warn(&text, &format!("{who} omitted: {why}"), 1));
                        }
                    }
                    Err(le) => ds.push(diag(&text, &le, 1)),
                }
                (None, ds)
            }
        },
    };
    publish(uri, diags);
    docs.insert(uri.to_string(), Doc { text, session });
}

fn try_load(prev: Option<Session>, exp: Expanded) -> Result<Session, String> {
    if let Some(mut s) = prev {
        if s.reload(exp.clone()).is_ok() {
            return Ok(s);
        }
    }
    let mut s = Session::new_expanded(exp)?;
    s.run_full()?;
    Ok(s)
}

fn with_doc_pos<F>(docs: &HashMap<String, Doc>, msg: &J, f: F) -> Option<J>
where
    F: FnOnce(&Doc, &Session, usize) -> Option<J>,
{
    let uri = msg.path("params.textDocument.uri").and_then(J::as_str)?;
    let doc = docs.get(uri)?;
    let s = doc.session.as_ref()?;
    let line = msg.path("params.position.line").and_then(J::as_f64)? as u32;
    let ch = msg.path("params.position.character").and_then(J::as_f64)? as u32;
    let off = pos_to_offset(&doc.text, line, ch);
    f(doc, s, off)
}

// ---- positions (LSP lines/UTF-16 characters ↔ byte offsets) ------------

fn pos_to_offset(text: &str, line: u32, ch: u32) -> usize {
    let mut cur = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i as u32 == line {
            let mut u16c = 0u32;
            for (bi, c) in l.char_indices() {
                if u16c >= ch {
                    return cur + bi;
                }
                u16c += c.len_utf16() as u32;
            }
            return cur + l.len();
        }
        cur += l.len();
    }
    text.len()
}

fn line_range(text: &str, line1: usize) -> J {
    let l = line1.saturating_sub(1);
    let len = text.lines().nth(l).map(|s| s.chars().map(char::len_utf16).sum::<usize>()).unwrap_or(0);
    J::obj(vec![
        ("start", J::obj(vec![("line", J::n(l as f64)), ("character", J::n(0.0))])),
        ("end", J::obj(vec![("line", J::n(l as f64)), ("character", J::n(len as f64))])),
    ])
}

fn diag(text: &str, msg: &str, line1: usize) -> J {
    J::obj(vec![
        ("range", line_range(text, line1)),
        ("severity", J::n(1.0)),
        ("source", J::s("fml")),
        ("message", J::s(msg)),
    ])
}

fn warn(text: &str, msg: &str, line1: usize) -> J {
    J::obj(vec![
        ("range", line_range(text, line1)),
        ("severity", J::n(2.0)),
        ("source", J::s("fml")),
        ("message", J::s(msg)),
    ])
}

// ---- transport -----------------------------------------------------------

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let p = uri.strip_prefix("file://")?;
    // Percent-decode the essentials (spaces).
    Some(PathBuf::from(p.replace("%20", " ")))
}

fn sibling_uri(doc_uri: &str, file: &str) -> String {
    match uri_to_path(doc_uri).and_then(|p| p.parent().map(|d| d.join(file))) {
        Some(p) => format!("file://{}", p.display()),
        None => doc_uri.to_string(),
    }
}

fn publish(uri: &str, diags: Vec<J>) {
    send(J::obj(vec![
        ("jsonrpc", J::s("2.0")),
        ("method", J::s("textDocument/publishDiagnostics")),
        ("params", J::obj(vec![("uri", J::s(uri)), ("diagnostics", J::A(diags))])),
    ]));
}

fn respond(id: Option<J>, result: J) {
    send(J::obj(vec![
        ("jsonrpc", J::s("2.0")),
        ("id", id.unwrap_or(J::Null)),
        ("result", result),
    ]));
}

fn send(v: J) {
    let body = v.dump();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = out.flush();
}

fn read_message(input: &mut impl Read) -> Option<String> {
    // Headers: read byte-by-byte until \r\n\r\n.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if input.read_exact(&mut b).is_err() {
            return None;
        }
        head.push(b[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 65536 {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let len: usize = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))?
        .split(':')
        .nth(1)?
        .trim()
        .parse()
        .ok()?;
    let mut body = vec![0u8; len];
    input.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}
