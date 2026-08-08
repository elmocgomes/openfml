//! fml — a corporate finance modelling language.
//!
//! Phase 1 (this crate): lexer → parser → unit/type checker → reference
//! evaluator, exercised by the golden-model test suite. See
//! `finmodel-lang-research/` in the workspace for the design documents.

pub mod ast;
pub mod calendar;
pub mod check;
pub mod crypto;
pub mod eval;
pub mod lexer;
pub mod live;
pub mod parser;
pub mod server;
pub mod units;
pub mod wasm;

pub use check::{check, Checked};
pub use eval::{evaluate, EvalResult};
pub use live::Session;
pub use parser::Parser;

/// One file of a multi-file model.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

/// One contiguous run of the expanded document copied verbatim from a file:
/// `flat[flat_start..flat_end] == files[file].text[local_start..][..len]`.
/// The generated include markers lie between segments and belong to no file.
#[derive(Clone, Debug)]
pub struct Segment {
    pub flat_start: usize,
    pub flat_end: usize,
    pub file: usize,
    pub local_start: usize,
}

/// An include-expanded model with per-file span provenance: any byte span
/// of `flat` inside a segment maps back to the file that owns it — the
/// basis for routing grid → text write-back into the right file.
#[derive(Clone, Debug)]
pub struct Expanded {
    pub flat: String,
    pub files: Vec<SourceFile>,
    pub segments: Vec<Segment>,
}

/// Expand `include "path"` lines (whole-line directives) using the given
/// resolver, recursively, with cycle/depth protection, keeping a source
/// map. Multi-file models: each cost-center/team owns a file; git merges
/// become structurally conflict-free. `files[0]` is always the main file.
pub fn expand_includes_with_map(
    main_name: &str,
    src: &str,
    resolver: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<Expanded, String> {
    fn go(
        file: usize,
        files: &mut Vec<SourceFile>,
        flat: &mut String,
        segments: &mut Vec<Segment>,
        resolver: &mut dyn FnMut(&str) -> Result<String, String>,
        depth: usize,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if depth > 16 {
            return Err("include depth exceeds 16 — circular includes?".into());
        }
        let text = files[file].text.clone();
        let mut pos = 0usize;
        // (flat_start, local_start) of the verbatim run currently open.
        let mut run: Option<(usize, usize)> = None;
        let close_run = |run: &mut Option<(usize, usize)>, flat: &String, segments: &mut Vec<Segment>| {
            if let Some((fs, ls)) = run.take() {
                segments.push(Segment { flat_start: fs, flat_end: flat.len(), file, local_start: ls });
            }
        };
        while pos < text.len() {
            let end = text[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(text.len());
            let line = &text[pos..end]; // terminator included — byte-exact copy
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("include ") {
                close_run(&mut run, flat, segments);
                let path = rest.trim().trim_matches('"').to_string();
                if stack.iter().any(|p| p == &path) {
                    return Err(format!("circular include of \"{path}\""));
                }
                let idx = match files.iter().position(|f| f.name == path) {
                    Some(i) => i,
                    None => {
                        let inner = resolver(&path)?;
                        files.push(SourceFile { name: path.clone(), text: inner });
                        files.len() - 1
                    }
                };
                flat.push_str(&format!("// >>> include \"{path}\"\n"));
                stack.push(path.clone());
                go(idx, files, flat, segments, resolver, depth + 1, stack)?;
                stack.pop();
                flat.push_str(&format!("// <<< include \"{path}\"\n"));
            } else {
                if run.is_none() {
                    run = Some((flat.len(), pos));
                }
                flat.push_str(line);
            }
            pos = end;
        }
        close_run(&mut run, flat, segments);
        // Newline OUTSIDE any segment, so file-local offsets stay exact.
        if !flat.ends_with('\n') && !flat.is_empty() {
            flat.push('\n');
        }
        Ok(())
    }
    let mut files = vec![SourceFile { name: main_name.to_string(), text: src.to_string() }];
    let mut flat = String::with_capacity(src.len());
    let mut segments = Vec::new();
    let mut stack = vec![main_name.to_string()];
    go(0, &mut files, &mut flat, &mut segments, resolver, 0, &mut stack)?;
    Ok(Expanded { flat, files, segments })
}

/// Flat-text expansion (no source map) — see `expand_includes_with_map`.
pub fn expand_includes(
    src: &str,
    resolver: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    expand_includes_with_map("", src, resolver).map(|e| e.flat)
}

/// Parse + check a source file.
pub fn compile(src: &str) -> Result<Checked, String> {
    let model = Parser::parse(src)?;
    check(&model)
}

/// Parse + check + evaluate.
pub fn run(src: &str) -> Result<EvalResult, String> {
    let checked = compile(src)?;
    evaluate(&checked)
}
