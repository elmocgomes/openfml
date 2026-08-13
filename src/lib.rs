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

/// Expand user defs: every `Call` to a def inlines its body (hygienic
/// parameter substitution) until only core expressions remain — the same
/// growth-by-desugaring the built-ins use, promoted to users. Runs BEFORE
/// checking, so units, provenance, incremental evaluation, simulate and
/// goal-seek see the expanded graph and need no new machinery. The def
/// call graph must be a DAG (no recursion — time recursion is `prev`,
/// circularity is `solve`). Fully-annotated defs are additionally checked
/// at DEFINITION site with skolem units: the body is compiled once
/// against fresh opaque units standing in for each `$X`, so a def that is
/// unit-sound there is sound for every instantiation.
pub fn expand_defs(model: &mut ast::Model) -> Result<(), String> {
    use ast::{DefAnn, DefDecl, Expr};
    use std::collections::HashMap;
    let defs: HashMap<String, DefDecl> =
        model.defs.iter().map(|d| (d.name.clone(), d.clone())).collect();

    // ---- the def call graph must be a DAG --------------------------------
    fn calls_in(e: &Expr, out: &mut Vec<String>) {
        if let Expr::Call(n, _) = e {
            if n != "min" && n != "max" {
                out.push(n.clone());
            }
        }
        match e {
            Expr::Neg(a) | Expr::Annualize(a) => calls_in(a, out),
            Expr::Bin(_, a, b) => {
                calls_in(a, out);
                calls_in(b, out);
            }
            Expr::Call(_, args) => {
                for a in args {
                    calls_in(a, out);
                }
            }
            Expr::Prev(_, Some(i)) => calls_in(i, out),
            Expr::When { value, .. } => calls_in(value, out),
            Expr::MatchT(arms) => arms.iter().for_each(|(_, a)| calls_in(a, out)),
            Expr::MatchDim { arms, default, .. } => {
                arms.iter().for_each(|(_, a)| calls_in(a, out));
                if let Some(d) = default {
                    calls_in(d, out);
                }
            }
            Expr::Conv { body, rate, .. } => {
                calls_in(body, out);
                if let Some(r) = rate {
                    calls_in(r, out);
                }
            }
            Expr::RangeSum { body, .. } => calls_in(body, out),
            Expr::Npv { rate, body, .. } => {
                calls_in(rate, out);
                calls_in(body, out);
            }
            Expr::AllocShare { total, driver, .. } => {
                calls_in(total, out);
                calls_in(driver, out);
            }
            _ => {}
        }
    }
    // Depth-first cycle check over def→def edges.
    fn dag(
        name: &str,
        defs: &HashMap<String, DefDecl>,
        state: &mut HashMap<String, u8>, // 1 = on stack, 2 = done
    ) -> Result<(), String> {
        match state.get(name) {
            Some(1) => return Err(format!("def '{name}' is recursive — the def call graph must be a DAG (use prev/solve for recursion in time or fixpoints)")),
            Some(2) => return Ok(()),
            _ => {}
        }
        state.insert(name.to_string(), 1);
        let mut callees = Vec::new();
        if let Some(d) = defs.get(name) {
            calls_in(&d.body, &mut callees);
        }
        for c in callees {
            if defs.contains_key(&c) {
                dag(&c, defs, state)?;
            }
        }
        state.insert(name.to_string(), 2);
        Ok(())
    }
    let mut state = HashMap::new();
    for name in defs.keys() {
        dag(name, &defs, &mut state)?;
    }

    // ---- substitution ----------------------------------------------------
    // Value positions take the argument expression; NAME positions (prev,
    // range names, series indexing) require the argument to be a bare name.
    fn name_of(env: &HashMap<String, Expr>, n: &str, ctx: &str) -> Result<String, String> {
        match env.get(n) {
            None => Ok(n.to_string()),
            Some(Expr::Ref(m)) => Ok(m.clone()),
            Some(_) => Err(format!(
                "def parameter '{n}' is used as a {ctx} name — pass a bare name for it"
            )),
        }
    }
    fn subst(e: &Expr, env: &HashMap<String, Expr>) -> Result<Expr, String> {
        Ok(match e {
            Expr::Ref(n) => env.get(n).cloned().unwrap_or_else(|| e.clone()),
            Expr::Prev(n, init) => Expr::Prev(
                name_of(env, n, "prev() measure")?,
                init.as_ref().map(|i| subst(i, env).map(Box::new)).transpose()?,
            ),
            Expr::Neg(a) => Expr::Neg(Box::new(subst(a, env)?)),
            Expr::Annualize(a) => Expr::Annualize(Box::new(subst(a, env)?)),
            Expr::Bin(op, a, b) => {
                Expr::Bin(*op, Box::new(subst(a, env)?), Box::new(subst(b, env)?))
            }
            Expr::Call(n, args) => Expr::Call(
                n.clone(),
                args.iter().map(|a| subst(a, env)).collect::<Result<_, _>>()?,
            ),
            Expr::When { value, pos, range } => Expr::When {
                value: Box::new(subst(value, env)?),
                pos: *pos,
                range: name_of(env, range, "range")?,
            },
            Expr::MatchT(arms) => Expr::MatchT(
                arms.iter()
                    .map(|(r, a)| {
                        Ok((
                            ast::RangeSetRef {
                                base: name_of(env, &r.base, "range")?,
                                minus: r
                                    .minus
                                    .as_ref()
                                    .map(|m| name_of(env, m, "range"))
                                    .transpose()?,
                            },
                            subst(a, env)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            ),
            Expr::MatchDim { dim, arms, default } => Expr::MatchDim {
                dim: dim.clone(),
                arms: arms
                    .iter()
                    .map(|(m, a)| Ok((m.clone(), subst(a, env)?)))
                    .collect::<Result<Vec<_>, String>>()?,
                default: default
                    .as_ref()
                    .map(|d| subst(d, env).map(Box::new))
                    .transpose()?,
            },
            Expr::MemberIx { name, members } => Expr::MemberIx {
                name: name_of(env, name, "measure")?,
                members: members.clone(),
            },
            Expr::Conv { body, target, rate } => Expr::Conv {
                body: Box::new(subst(body, env)?),
                target: target.clone(),
                rate: rate.as_ref().map(|r| subst(r, env).map(Box::new)).transpose()?,
            },
            Expr::WindowSum { name, from, to } => Expr::WindowSum {
                name: name_of(env, name, "measure")?,
                from: from.clone(),
                to: to.clone(),
            },
            Expr::RangeSum { range, body } => Expr::RangeSum {
                range: name_of(env, range, "range")?,
                body: Box::new(subst(body, env)?),
            },
            Expr::Npv { rate, body, range } => Expr::Npv {
                rate: Box::new(subst(rate, env)?),
                body: Box::new(subst(body, env)?),
                range: name_of(env, range, "range")?,
            },
            Expr::At { name, bound } => Expr::At {
                name: name_of(env, name, "measure")?,
                bound: bound.clone(),
            },
            Expr::Irr { calendar, name } => Expr::Irr {
                calendar: name_of(env, calendar, "calendar")?,
                name: name_of(env, name, "measure")?,
            },
            Expr::AllocShare { total, driver, dim, dp, policy } => Expr::AllocShare {
                total: Box::new(subst(total, env)?),
                driver: Box::new(subst(driver, env)?),
                dim: dim.clone(),
                dp: *dp,
                policy: *policy,
            },
            Expr::Num(_) | Expr::Qty(_, _) | Expr::Pct(_) | Expr::YearT => e.clone(),
        })
    }

    // ---- expansion -------------------------------------------------------
    fn xexpr(e: &Expr, defs: &HashMap<String, DefDecl>) -> Result<Expr, String> {
        // Recurse into children first, then inline calls bottom-up.
        let e = subst(e, &HashMap::new())?; // cheap structural clone via subst
        let rec = |c: &Expr| xexpr(c, defs);
        Ok(match &e {
            Expr::Call(name, args) if name != "min" && name != "max" => {
                let d = defs.get(name).ok_or_else(|| {
                    format!(
                        "unknown function '{name}' — defs in scope: {}; builtins: prev, min, max, sum, npv, irr, annualize, year",
                        if defs.is_empty() { "(none)".to_string() } else { defs.keys().cloned().collect::<Vec<_>>().join(", ") }
                    )
                })?;
                if d.params.len() != args.len() {
                    return Err(format!(
                        "def '{name}' takes {} argument{}, got {}",
                        d.params.len(),
                        if d.params.len() == 1 { "" } else { "s" },
                        args.len()
                    ));
                }
                let xargs: Vec<Expr> = args.iter().map(rec).collect::<Result<_, _>>()?;
                let env: HashMap<String, Expr> = d
                    .params
                    .iter()
                    .zip(xargs)
                    .map(|((p, _), a)| (p.clone(), a))
                    .collect();
                let body = subst(&d.body, &env)
                    .map_err(|er| format!("in def '{name}' (line {}): {er}", d.line))?;
                xexpr(&body, defs)?
            }
            Expr::Call(name, args) => {
                Expr::Call(name.clone(), args.iter().map(rec).collect::<Result<_, _>>()?)
            }
            Expr::Neg(a) => Expr::Neg(Box::new(rec(a)?)),
            Expr::Annualize(a) => Expr::Annualize(Box::new(rec(a)?)),
            Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(rec(a)?), Box::new(rec(b)?)),
            Expr::Prev(n, init) => Expr::Prev(
                n.clone(),
                init.as_ref().map(|i| rec(i).map(Box::new)).transpose()?,
            ),
            Expr::When { value, pos, range } => Expr::When {
                value: Box::new(rec(value)?),
                pos: *pos,
                range: range.clone(),
            },
            Expr::MatchT(arms) => Expr::MatchT(
                arms.iter()
                    .map(|(r, a)| Ok((r.clone(), rec(a)?)))
                    .collect::<Result<Vec<_>, String>>()?,
            ),
            Expr::MatchDim { dim, arms, default } => Expr::MatchDim {
                dim: dim.clone(),
                arms: arms
                    .iter()
                    .map(|(m, a)| Ok((m.clone(), rec(a)?)))
                    .collect::<Result<Vec<_>, String>>()?,
                default: default.as_ref().map(|d| rec(d).map(Box::new)).transpose()?,
            },
            Expr::Conv { body, target, rate } => Expr::Conv {
                body: Box::new(rec(body)?),
                target: target.clone(),
                rate: rate.as_ref().map(|r| rec(r).map(Box::new)).transpose()?,
            },
            Expr::RangeSum { range, body } => Expr::RangeSum {
                range: range.clone(),
                body: Box::new(rec(body)?),
            },
            Expr::Npv { rate, body, range } => Expr::Npv {
                rate: Box::new(rec(rate)?),
                body: Box::new(rec(body)?),
                range: range.clone(),
            },
            Expr::AllocShare { total, driver, dim, dp, policy } => Expr::AllocShare {
                total: Box::new(rec(total)?),
                driver: Box::new(rec(driver)?),
                dim: dim.clone(),
                dp: *dp,
                policy: *policy,
            },
            _ => e.clone(),
        })
    }
    fn xbody(b: &mut ast::Body, defs: &HashMap<String, DefDecl>) -> Result<(), String> {
        match b {
            ast::Body::Expr(e) => *e = xexpr(e, defs)?,
            ast::Body::Map(entries) => {
                for (_, e) in entries {
                    *e = xexpr(e, defs)?;
                }
            }
            ast::Body::DimMatch { arms, default, .. } => {
                for (_, a) in arms {
                    xbody(a, defs)?;
                }
                if let Some(d) = default {
                    xbody(d, defs)?;
                }
            }
            ast::Body::Data { .. } => {}
        }
        Ok(())
    }

    // ---- definition-site unit soundness (skolem units) --------------------
    for d in defs.values() {
        let all_annotated =
            d.params.iter().all(|(_, a)| a.is_some()) && d.ret.is_some();
        if !all_annotated {
            continue;
        }
        let mut unit_vars: Vec<String> = Vec::new();
        let mut units = Vec::new();
        let skolem = |v: &str, unit_vars: &mut Vec<String>| -> String {
            if let Some(k) = unit_vars.iter().position(|x| x == v) {
                format!("__u{k}")
            } else {
                unit_vars.push(v.to_string());
                format!("__u{}", unit_vars.len() - 1)
            }
        };
        let mut items = Vec::new();
        let mut env: HashMap<String, Expr> = HashMap::new();
        for (p, ann) in &d.params {
            match ann.as_ref().unwrap() {
                DefAnn::Var(v, kind) => {
                    let u = skolem(v, &mut unit_vars);
                    items.push(ast::Item::Measure(ast::MeasureDecl {
                        name: p.clone(),
                        is_input: true,
                        ann: ast::TypeAnn {
                            unit: Some(ast::UnitAst { num: u, den: None }),
                            kind: *kind,
                        },
                        over: if kind.is_some() { vec!["__c".into()] } else { vec![] },
                        init: None,
                        round: None,
                        body: ast::Body::Expr(Expr::Num(1.0)),
                        dist: None,
                        data_src: None,
                        line: d.line,
                    }));
                }
                DefAnn::Dimensionless => {
                    items.push(ast::Item::Measure(ast::MeasureDecl {
                        name: p.clone(),
                        is_input: true,
                        ann: ast::TypeAnn {
                            unit: Some(ast::UnitAst { num: "rate".into(), den: None }),
                            kind: None,
                        },
                        over: vec![],
                        init: None,
                        round: None,
                        body: ast::Body::Expr(Expr::Num(0.1)),
                        dist: None,
                        data_src: None,
                        line: d.line,
                    }));
                }
                DefAnn::Range => {
                    env.insert(p.clone(), Expr::Ref("__c".into()));
                }
            }
        }
        for k in 0..unit_vars.len() {
            units.push(ast::UnitDecl { name: format!("__u{k}"), scaled: None });
        }
        let (ret_over, ret_kind) = match d.ret.as_ref().unwrap() {
            DefAnn::Var(_, k) => (k.is_some(), *k),
            _ => (false, None),
        };
        let body = subst(&d.body, &env)
            .map_err(|e| format!("def '{}' (line {}): {e}", d.name, d.line))?;
        items.push(ast::Item::Measure(ast::MeasureDecl {
            name: "__r".into(),
            is_input: false,
            ann: ast::TypeAnn { unit: None, kind: ret_kind },
            over: if ret_over { vec!["__c".into()] } else { vec![] },
            init: None,
            round: None,
            body: ast::Body::Expr(body),
            dist: None,
            data_src: None,
            line: d.line,
        }));
        let mut synth = ast::Model {
            name: "__defcheck".into(),
            calendar: Some(ast::CalendarDecl {
                name: "__c".into(),
                grain: "yearly".into(),
                start: calendar::PeriodLit { year: 2026, sub: None, q_form: false },
                end: calendar::PeriodLit { year: 2029, sub: None, q_form: false },
            }),
            period_ranges: Vec::new(),
            dimensions: Vec::new(),
            functional: None,
            currency: None,
            units,
            items,
            defs: model.defs.clone(),
            scenarios: Vec::new(),
            edit_sites: Vec::new(),
            correlations: Vec::new(),
        };
        // Expand nested def calls inside the synthetic body, then check.
        for it in &mut synth.items {
            if let ast::Item::Measure(m) = it {
                xbody(&mut m.body, &defs)
                    .map_err(|e| format!("def '{}' (line {}): {e}", d.name, d.line))?;
            }
        }
        check(&synth).map_err(|e| {
            format!(
                "def '{}' (line {}) is not unit-sound for all instantiations \
                 (defs are closed over their parameters; units are checked \
                 with skolem units): {e}",
                d.name, d.line
            )
        })?;
    }

    // ---- expand the model ------------------------------------------------
    for it in &mut model.items {
        match it {
            ast::Item::Measure(m) => {
                xbody(&mut m.body, &defs)?;
                if let Some((_, e)) = &mut m.init {
                    *e = xexpr(e, &defs)?;
                }
                if let Some(dist) = &mut m.dist {
                    for (_, e) in &mut dist.params {
                        *e = xexpr(e, &defs)?;
                    }
                }
            }
            ast::Item::Assert(a) => {
                a.lhs = xexpr(&a.lhs, &defs)?;
                a.rhs = xexpr(&a.rhs, &defs)?;
                if let Some(t) = &mut a.tol {
                    *t = xexpr(t, &defs)?;
                }
            }
            ast::Item::Solve(s) => match &mut s.form {
                ast::SolveForm::Block(ms) => {
                    for m in ms {
                        xbody(&mut m.body, &defs)?;
                        if let Some((_, e)) = &mut m.init {
                            *e = xexpr(e, &defs)?;
                        }
                    }
                }
                ast::SolveForm::Tearing(relaxes) => {
                    for r in relaxes.iter_mut() {
                        r.init = xexpr(&r.init, &defs)?;
                    }
                }
            },
        }
    }
    for sc in &mut model.scenarios {
        for (_, b, _) in &mut sc.overrides {
            xbody(b, &defs)?;
        }
    }
    Ok(())
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
    expand_defs(&mut model)?;
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
