//! End-to-end LSP test: spawn the real fml-lsp binary and speak JSON-RPC
//! over stdio — initialize, open a multi-file model, hover, definition
//! into the include, symbols, break-and-fix diagnostics.

use fml::json::{parse, J};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

struct Lsp {
    child: Child,
    next_id: f64,
}

impl Lsp {
    fn start() -> Lsp {
        let child = Command::new(env!("CARGO_BIN_EXE_fml-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fml-lsp");
        Lsp { child, next_id: 1.0 }
    }

    fn send(&mut self, v: J) {
        let body = v.dump();
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: J) -> f64 {
        let id = self.next_id;
        self.next_id += 1.0;
        self.send(J::obj(vec![
            ("jsonrpc", J::s("2.0")),
            ("id", J::n(id)),
            ("method", J::s(method)),
            ("params", params),
        ]));
        id
    }

    fn notify(&mut self, method: &str, params: J) {
        self.send(J::obj(vec![
            ("jsonrpc", J::s("2.0")),
            ("method", J::s(method)),
            ("params", params),
        ]));
    }

    fn read_msg(&mut self) -> J {
        let out = self.child.stdout.as_mut().unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        loop {
            out.read_exact(&mut b).expect("read header");
            head.push(b[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head);
        let len: usize = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .unwrap()
            .split(':')
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut body = vec![0u8; len];
        out.read_exact(&mut body).expect("read body");
        parse(&String::from_utf8_lossy(&body)).expect("json")
    }

    /// Read messages until the response with `id` arrives; collect any
    /// publishDiagnostics seen along the way.
    fn response(&mut self, id: f64, diags: &mut Vec<J>) -> J {
        loop {
            let m = self.read_msg();
            if m.get("id").and_then(J::as_f64) == Some(id) {
                return m.get("result").cloned().unwrap_or(J::Null);
            }
            if m.get("method").and_then(J::as_str) == Some("textDocument/publishDiagnostics") {
                diags.push(m.get("params").cloned().unwrap());
            }
        }
    }

    /// Read messages until a publishDiagnostics notification arrives.
    fn next_diagnostics(&mut self) -> J {
        loop {
            let m = self.read_msg();
            if m.get("method").and_then(J::as_str) == Some("textDocument/publishDiagnostics") {
                return m.get("params").cloned().unwrap();
            }
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn td(uri: &str) -> J {
    J::obj(vec![("textDocument", J::obj(vec![("uri", J::s(uri))]))])
}

fn pos_params(uri: &str, line: f64, ch: f64) -> J {
    J::obj(vec![
        ("textDocument", J::obj(vec![("uri", J::s(uri))])),
        ("position", J::obj(vec![("line", J::n(line)), ("character", J::n(ch))])),
    ])
}

#[test]
fn full_protocol_session() {
    // A multi-file model on disk, so includes and definitions resolve.
    let dir = std::env::temp_dir().join(format!("fml_lsp_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_path = dir.join("plan.fml");
    let inc_path = dir.join("team.fml");
    std::fs::write(&inc_path, "input spend : EUR flow over plan = { 2026: 10, 2027: 12 }\n").unwrap();
    let main_text = "model demo.lsp\ncalendar plan = yearly 2026 .. 2027\ncurrency EUR\n\
include \"team.fml\"\ntotal : EUR flow over plan = spend * 2\n";
    std::fs::write(&main_path, main_text).unwrap();
    let uri = format!("file://{}", main_path.display());

    let mut lsp = Lsp::start();
    let mut diags = Vec::new();

    // initialize → capabilities
    let id = lsp.request("initialize", J::obj(vec![]));
    let init = lsp.response(id, &mut diags);
    assert_eq!(init.path("capabilities.hoverProvider"), Some(&J::B(true)));
    lsp.notify("initialized", J::obj(vec![]));

    // didOpen → clean diagnostics
    lsp.notify(
        "textDocument/didOpen",
        J::obj(vec![(
            "textDocument",
            J::obj(vec![("uri", J::s(&uri)), ("text", J::s(main_text))]),
        )]),
    );
    let d = lsp.next_diagnostics();
    assert_eq!(d.get("diagnostics").and_then(J::as_arr).unwrap().len(), 0, "{d:?}");

    // hover on `total` (line 4, col 0..5) → markdown with unit + value
    let id = lsp.request("textDocument/hover", pos_params(&uri, 4.0, 1.0));
    let hover = lsp.response(id, &mut diags);
    let md = hover.path("contents.value").and_then(J::as_str).unwrap();
    assert!(md.contains("**total**"), "{md}");
    assert!(md.contains("`EUR`"), "{md}");
    assert!(md.contains("2026: 20"), "live values in hover: {md}");

    // hover on `spend` inside total's formula → the INPUT from the include
    let col = main_text.lines().nth(4).unwrap().find("spend").unwrap() as f64;
    let id = lsp.request("textDocument/hover", pos_params(&uri, 4.0, col + 1.0));
    let hover2 = lsp.response(id, &mut diags);
    assert!(hover2.path("contents.value").and_then(J::as_str).unwrap().contains("input spend"));

    // definition of `spend` → the INCLUDED file, line 1
    let id = lsp.request("textDocument/definition", pos_params(&uri, 4.0, col + 1.0));
    let def = lsp.response(id, &mut diags);
    assert!(def.get("uri").and_then(J::as_str).unwrap().ends_with("team.fml"), "{def:?}");
    assert_eq!(def.path("range.start.line"), Some(&J::N(0.0)));

    // documentSymbol → this document's declarations only
    let id = lsp.request("textDocument/documentSymbol", td(&uri));
    let syms = lsp.response(id, &mut diags);
    let names: Vec<&str> = syms
        .as_arr()
        .unwrap()
        .iter()
        .map(|s| s.get("name").and_then(J::as_str).unwrap())
        .collect();
    assert!(names.contains(&"total") && names.contains(&"plan"), "{names:?}");
    assert!(!names.contains(&"spend"), "include-file symbols excluded: {names:?}");

    // completion → measures with units, keywords
    let id = lsp.request("textDocument/completion", pos_params(&uri, 4.0, 5.0));
    let comp = lsp.response(id, &mut diags);
    let items = comp.as_arr().unwrap();
    let spend = items
        .iter()
        .find(|i| i.get("label").and_then(J::as_str) == Some("spend"))
        .expect("spend completes");
    assert!(spend.get("detail").and_then(J::as_str).unwrap().contains("EUR"));
    assert!(items.iter().any(|i| i.get("label").and_then(J::as_str) == Some("allocate")), "keywords complete");

    // break the model → error diagnostic + dropped-dependent warning
    let broken = main_text.replace("= spend * 2", "= spend * * 2");
    lsp.notify(
        "textDocument/didChange",
        J::obj(vec![
            ("textDocument", J::obj(vec![("uri", J::s(&uri))])),
            ("contentChanges", J::A(vec![J::obj(vec![("text", J::s(&broken))])])),
        ]),
    );
    let d2 = lsp.next_diagnostics();
    let list = d2.get("diagnostics").and_then(J::as_arr).unwrap();
    assert!(!list.is_empty());
    assert!(list[0].get("message").and_then(J::as_str).unwrap().contains("expected an expression"));

    // fix it → diagnostics clear
    lsp.notify(
        "textDocument/didChange",
        J::obj(vec![
            ("textDocument", J::obj(vec![("uri", J::s(&uri))])),
            ("contentChanges", J::A(vec![J::obj(vec![("text", J::s(main_text))])])),
        ]),
    );
    let d3 = lsp.next_diagnostics();
    assert_eq!(d3.get("diagnostics").and_then(J::as_arr).unwrap().len(), 0);

    // shutdown/exit
    let id = lsp.request("shutdown", J::Null);
    lsp.response(id, &mut diags);
    lsp.notify("exit", J::Null);
    let _ = std::fs::remove_dir_all(&dir);
}
