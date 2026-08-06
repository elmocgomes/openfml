//! fml — a corporate finance modelling language.
//!
//! Phase 1 (this crate): lexer → parser → unit/type checker → reference
//! evaluator, exercised by the golden-model test suite. See
//! `finmodel-lang-research/` in the workspace for the design documents.

pub mod ast;
pub mod calendar;
pub mod check;
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

/// Expand `include "path"` lines (whole-line directives) using the given
/// resolver, recursively, with cycle/depth protection. Multi-file models:
/// each cost-center/team owns a file; git merges become structurally
/// conflict-free. The expansion is textual — spans and line numbers refer
/// to the expanded document (per-file provenance is a follow-up).
pub fn expand_includes(
    src: &str,
    resolver: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    fn go(
        src: &str,
        resolver: &mut dyn FnMut(&str) -> Result<String, String>,
        depth: usize,
        stack: &mut Vec<String>,
    ) -> Result<String, String> {
        if depth > 16 {
            return Err("include depth exceeds 16 — circular includes?".into());
        }
        let mut out = String::with_capacity(src.len());
        for line in src.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("include ") {
                let path = rest.trim().trim_matches('"');
                if stack.iter().any(|p| p == path) {
                    return Err(format!("circular include of \"{path}\""));
                }
                let inner = resolver(path)?;
                stack.push(path.to_string());
                let expanded = go(&inner, resolver, depth + 1, stack)?;
                stack.pop();
                out.push_str(&format!("// >>> include \"{path}\"\n"));
                out.push_str(&expanded);
                if !expanded.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!("// <<< include \"{path}\"\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(out)
    }
    go(src, resolver, 0, &mut Vec::new())
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
