//! fml — a corporate finance modelling language.
//!
//! Phase 1 (this crate): lexer → parser → unit/type checker → reference
//! evaluator, exercised by the golden-model test suite. See
//! `finmodel-lang-research/` in the workspace for the design documents.

pub mod ast;
pub mod calendar;
pub mod check;
pub mod crypto;
pub mod cst;
pub mod eval;
pub mod json;
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

/// Result of a salvage parse: every intact declaration, with broken ones
/// and their transitive dependents removed — the model that CAN still run
/// while the file is mid-edit.
pub struct Salvaged {
    pub model: ast::Model,
    pub errors: Vec<parser::ParseError>,
    /// (declaration, reason) for everything omitted beyond the errors.
    pub dropped: Vec<(String, String)>,
}

/// Resilient parse + dependency cascade: broken declarations are recorded,
/// and any measure/assert/solve/scenario that (transitively) references a
/// missing name is dropped with a reason. The caller decides whether the
/// salvaged model checks and runs.
pub fn parse_salvage(src: &str) -> Result<Salvaged, String> {
    use std::collections::HashSet;
    let (mut model, _spans, errors) = Parser::parse_resilient(src)?;
    let mut dropped: Vec<(String, String)> = Vec::new();

    fn measure_refs(m: &ast::MeasureDecl, out: &mut Vec<String>) {
        out.extend(ast::measure_references(m));
    }

    loop {
        let mut defined: HashSet<String> = HashSet::new();
        for it in &model.items {
            match it {
                ast::Item::Measure(m) => {
                    defined.insert(m.name.clone());
                }
                ast::Item::Solve(s) => {
                    if let ast::SolveForm::Block(ms) = &s.form {
                        for m in ms {
                            defined.insert(m.name.clone());
                        }
                    }
                }
                ast::Item::Assert(_) => {}
            }
        }
        let mut removed_any = false;
        let mut keep: Vec<ast::Item> = Vec::new();
        for it in model.items.drain(..) {
            let mut refs = Vec::new();
            let who = match &it {
                ast::Item::Measure(m) => {
                    measure_refs(m, &mut refs);
                    m.name.clone()
                }
                ast::Item::Assert(a) => {
                    ast::all_names(&a.lhs, &mut refs);
                    ast::all_names(&a.rhs, &mut refs);
                    if let Some(t) = &a.tol {
                        ast::all_names(t, &mut refs);
                    }
                    format!("assert {}", a.name)
                }
                ast::Item::Solve(s) => {
                    match &s.form {
                        ast::SolveForm::Block(ms) => {
                            for m in ms {
                                measure_refs(m, &mut refs);
                            }
                        }
                        ast::SolveForm::Tearing(rs) => {
                            for r in rs {
                                refs.push(r.name.clone());
                                ast::all_names(&r.init, &mut refs);
                            }
                        }
                    }
                    format!("solve {}", s.name)
                }
            };
            match refs.iter().find(|n| !defined.contains(*n)) {
                Some(n) => {
                    dropped.push((who, format!("references missing '{n}'")));
                    removed_any = true;
                }
                None => keep.push(it),
            }
        }
        model.items = keep;
        if !removed_any {
            break;
        }
    }
    // Prune scenarios (including chains off dropped parents) and
    // correlations/edit-sites that target removed declarations.
    let defined: std::collections::HashSet<String> = model
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Measure(m) => Some(m.name.clone()),
            _ => None,
        })
        .collect();
    loop {
        let names: Vec<String> = model.scenarios.iter().map(|s| s.name.clone()).collect();
        let before = model.scenarios.len();
        model.scenarios.retain(|s| {
            let ok = s.overrides.iter().all(|(t, _, _)| defined.contains(t))
                && s.from.as_ref().map(|f| f == "Base" || names.contains(f)).unwrap_or(true);
            if !ok {
                dropped.push((format!("scenario {}", s.name), "targets a missing declaration".into()));
            }
            ok
        });
        if model.scenarios.len() == before {
            break;
        }
    }
    model
        .correlations
        .retain(|c| defined.contains(&c.a) && defined.contains(&c.b));
    model.edit_sites.retain(|e| defined.contains(&e.measure));
    Ok(Salvaged { model, errors, dropped })
}

/// Bind the fact plane: replace every `= data "file.csv" [sha256 "…"]`
/// body with the values from its external table, BEFORE checking. The
/// desugared bodies are ordinary maps/match arms — but built post-parse,
/// so they carry NO edit sites: facts are structurally not
/// literal-editable; they change by re-import.
///
/// CSV shape is header-driven:
///   value                  → scalar (one data row)
///   period,value           → period map
///   <Dim>,period,value     → `match <Dim>` with one map arm per member
/// Period labels use calendar spelling: `2026` | `2026-Q3` | `2026-07`.
/// A `sha256` pin, when present, must match the content hash exactly.
pub fn bind_data(
    model: &mut ast::Model,
    resolve: &mut dyn FnMut(&str) -> Result<String, String>,
    store: &mut Vec<(String, String)>,
) -> Result<(), String> {
    fn period_label(s: &str, file: &str, ln: usize) -> Result<calendar::PeriodLit, String> {
        let bad = || format!("data file \"{file}\" line {ln}: bad period label '{s}'");
        let (y, rest) = match s.split_once('-') {
            Some((y, r)) => (y, Some(r)),
            None => (s, None),
        };
        let year: i64 = y.trim().parse().map_err(|_| bad())?;
        match rest.map(str::trim) {
            None => Ok(calendar::PeriodLit { year, sub: None, q_form: false }),
            Some(r) => {
                if let Some(q) = r.strip_prefix('Q') {
                    let qn: u8 = q.parse().map_err(|_| bad())?;
                    Ok(calendar::PeriodLit { year, sub: Some(qn), q_form: true })
                } else {
                    let m: u8 = r.parse().map_err(|_| bad())?;
                    Ok(calendar::PeriodLit { year, sub: Some(m), q_form: false })
                }
            }
        }
    }
    fn num(s: &str, file: &str, ln: usize) -> Result<f64, String> {
        s.trim()
            .replace('_', "")
            .parse()
            .map_err(|_| format!("data file \"{file}\" line {ln}: bad number '{}'", s.trim()))
    }
    for it in &mut model.items {
        let ast::Item::Measure(m) = it else { continue };
        let ast::Body::Data { file, sha256 } = &m.body else { continue };
        let (file, pin) = (file.clone(), sha256.clone());
        let text = match store.iter().find(|(n, _)| *n == file) {
            Some((_, t)) => t.clone(),
            None => {
                let t = resolve(&file)?;
                store.push((file.clone(), t.clone()));
                t
            }
        };
        if let Some(pin) = &pin {
            let h = crypto::hex(&crypto::sha256(text.as_bytes()));
            if h != *pin {
                return Err(format!(
                    "data file \"{file}\": content sha256 {h} does not match the pin {pin} \
                     declared on '{}' — re-import deliberately (update the pin) or restore the file",
                    m.name
                ));
            }
        }
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty());
        let Some((_, header)) = lines.next() else {
            return Err(format!("data file \"{file}\" is empty"));
        };
        let cols: Vec<&str> = header.split(',').map(str::trim).collect();
        let split = |ln: usize, l: &str, want: usize| -> Result<Vec<String>, String> {
            let f: Vec<String> = l.split(',').map(|c| c.trim().to_string()).collect();
            if f.len() != want {
                return Err(format!(
                    "data file \"{file}\" line {ln}: expected {want} columns ({}), found {}",
                    cols.join(","),
                    f.len()
                ));
            }
            Ok(f)
        };
        let body = match cols.as_slice() {
            ["value"] => {
                let Some((ln, l)) = lines.next() else {
                    return Err(format!("data file \"{file}\": no value row"));
                };
                let f = split(ln, l, 1)?;
                if lines.next().is_some() {
                    return Err(format!(
                        "data file \"{file}\": a scalar table has exactly one value row"
                    ));
                }
                ast::Body::Expr(ast::Expr::Num(num(&f[0], &file, ln)?))
            }
            ["period", "value"] => {
                let mut entries = Vec::new();
                for (ln, l) in lines {
                    let f = split(ln, l, 2)?;
                    entries.push((period_label(&f[0], &file, ln)?, ast::Expr::Num(num(&f[1], &file, ln)?)));
                }
                ast::Body::Map(entries)
            }
            [dim, "period", "value"] => {
                let mut arms: Vec<(String, Vec<(calendar::PeriodLit, ast::Expr)>)> = Vec::new();
                for (ln, l) in lines {
                    let f = split(ln, l, 3)?;
                    let entry = (period_label(&f[1], &file, ln)?, ast::Expr::Num(num(&f[2], &file, ln)?));
                    match arms.iter_mut().find(|(mb, _)| *mb == f[0]) {
                        Some((_, es)) => es.push(entry),
                        None => arms.push((f[0].clone(), vec![entry])),
                    }
                }
                ast::Body::DimMatch {
                    dim: dim.to_string(),
                    arms: arms.into_iter().map(|(mb, es)| (mb, ast::Body::Map(es))).collect(),
                    default: None,
                }
            }
            _ => {
                return Err(format!(
                    "data file \"{file}\": header must be 'value', 'period,value' or \
                     '<Dimension>,period,value' — found '{}'",
                    cols.join(",")
                ))
            }
        };
        m.body = body;
        m.data_src = Some(file);
    }
    Ok(())
}

/// Parse + check a source file.
pub fn compile(src: &str) -> Result<Checked, String> {
    let mut model = Parser::parse(src)?;
    bind_data(
        &mut model,
        &mut |f| Err(format!("data file \"{f}\" is not loaded")),
        &mut Vec::new(),
    )?;
    check(&model)
}

/// Parse + check + evaluate.
pub fn run(src: &str) -> Result<EvalResult, String> {
    let checked = compile(src)?;
    evaluate(&checked)
}
