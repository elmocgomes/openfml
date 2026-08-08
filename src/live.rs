//! Phase 2: the incremental recalculation session.
//!
//! The checker already builds the complete (measure × member × period)
//! dependency graph, so incrementality is dirty propagation over it in
//! execution-plan order, with **early cutoff**: a step only re-runs when
//! one of its inputs actually changed *value*, and its own nodes are only
//! marked dirty when their values changed. In the Build-Systems-à-la-Carte
//! taxonomy this is a topological scheduler with a dirty-bit rebuilder plus
//! early cutoff — the option Excel never took.

use crate::ast::{Body, Expr, FirstLast, LitKind};
use crate::check::{Checked, Dist, MUnit, Step, UNBOUND};
use crate::eval::{self, AssertResult, Values};
use std::collections::HashSet;

/// One direct dependency cell of an explained value.
#[derive(Clone, Debug)]
pub struct Dep {
    pub name: String,
    /// Member label ("" for undimensioned).
    pub member: String,
    /// Referenced period slot (None for scalars / init values).
    pub period: Option<usize>,
    /// Human label: a period, "init", …
    pub label: String,
    pub value: f64,
    pub is_input: bool,
    /// How the reference reaches the cell: "", "prev", "init", "sum",
    /// "window", "rollup", "npv", "rate", "irr", "at".
    pub via: String,
}

/// One exact additive contribution to an explained value. The terms of an
/// explanation always sum to the cell's value (signed) — quantified
/// provenance, not a sensitivity approximation.
#[derive(Clone, Debug)]
pub struct Term {
    /// Rendered term: a cell display or a compact expression rendering.
    pub label: String,
    pub value: f64,
    /// The cell behind the term when it is exactly one reference —
    /// (name, member label, period) — making the term drillable.
    pub cell: Option<(String, String, Option<usize>)>,
}

/// "Explain this number": where a cell is defined (routed to the owning
/// file), which match/actuals arm fired, and the direct dependency cells
/// with their values — the provenance layer, one drill-down step at a time.
#[derive(Clone, Debug)]
pub struct Explanation {
    pub name: String,
    pub member: String,
    pub period: Option<usize>,
    pub value: f64,
    pub unit: String,
    pub is_input: bool,
    /// Owning file and 1-based line of the definition (via the source map).
    pub file: String,
    pub line: usize,
    /// The match/actuals arm that fired for this period/member, if any.
    pub arm: String,
    /// Nature notes: distribution, solve membership, literal editability.
    pub note: String,
    pub deps: Vec<Dep>,
    /// Exact additive decomposition of the value (empty for inputs and
    /// for cells whose top level is not additive-decomposable).
    pub terms: Vec<Term>,
}

#[derive(Clone, Debug)]
pub struct SimResult {
    pub trials: usize,
    /// (display name, is_series, per-slot [p10, p50, p90]).
    pub cells: Vec<(String, bool, Vec<[f64; 3]>)>,
}

/// Solution of a goal-seek: the input value found, the output actually
/// achieved at that value, and how many model evaluations it took.
#[derive(Clone, Copy, Debug)]
pub struct GoalSeekResult {
    pub value: f64,
    pub achieved: f64,
    pub iterations: usize,
}

/// Reconstruct the achieved output from a residual (f = output - target).
fn a_target(f: f64, target: f64) -> f64 {
    f + target
}

/// Plain decimal rendering for source literals (no exponent, tidy tail).
fn fmt_plain(v: f64) -> String {
    let r = if v.abs() < 1e12 { (v * 1e10).round() / 1e10 } else { v };
    format!("{r}")
}

/// Compact one-line rendering of an expression for term labels.
fn render_expr(e: &Expr) -> String {
    use crate::ast::BinOp;
    fn atom(e: &Expr) -> String {
        match e {
            Expr::Bin(..) => format!("({})", render_expr(e)),
            _ => render_expr(e),
        }
    }
    match e {
        Expr::Num(v) => format!("{v}"),
        Expr::Pct(v) => format!("{}%", v * 100.0),
        Expr::Qty(v, u) => format!("{v} {u}"),
        Expr::Ref(n) => n.clone(),
        Expr::Prev(n, _) => format!("prev({n})"),
        Expr::Neg(x) => format!("-{}", atom(x)),
        Expr::Bin(op, a, b) => {
            let o = match op {
                BinOp::Add => "+",
                BinOp::Sub => "−",
                BinOp::Mul => "×",
                BinOp::Div => "/",
                BinOp::Pow => "^",
            };
            format!("{} {o} {}", atom(a), atom(b))
        }
        Expr::Call(f, args) => {
            let a: Vec<String> = args.iter().map(render_expr).collect();
            format!("{f}({})", a.join(", "))
        }
        Expr::YearT => "year(t)".into(),
        Expr::MemberIx { name, members } => format!("{name}[{}]", members.join("][")),
        Expr::Conv { body, target, .. } => format!("{} in {target}", atom(body)),
        Expr::At { name, .. } => format!("{name}[t±]"),
        Expr::WindowSum { name, .. } => format!("sum({name}[…])"),
        Expr::RangeSum { range, body } => format!("sum[{range}]({})", render_expr(body)),
        Expr::AllocShare { total, driver, dp, .. } => {
            format!("{} by {} (round {dp})", atom(total), atom(driver))
        }
        Expr::Npv { range, .. } => format!("npv(… over {range})"),
        Expr::Irr { name, .. } => format!("irr({name})"),
        Expr::Annualize(x) => format!("annualize({})", render_expr(x)),
        Expr::When { value, .. } => format!("{} when …", atom(value)),
        Expr::MatchT(_) => "match t { … }".into(),
        Expr::MatchDim { dim, .. } => format!("match {dim} {{ … }}"),
    }
}

#[derive(Clone, Debug, Default)]
pub struct RecalcStats {
    pub steps_total: usize,
    pub steps_run: usize,
    pub nodes_changed: usize,
}

pub struct Session {
    pub checked: Checked,
    pub values: Values,
    /// The model source — kept current by `patch_input` (grid → text
    /// write-back via byte-exact span patching).
    src: String,
    /// The underlying files (files[0] = main) and the source map from flat
    /// spans back to them — patches are routed to the owning file too.
    files: Vec<crate::SourceFile>,
    segments: Vec<crate::Segment>,
    /// The lossless CST of the flat source. Edit-site spans are DERIVED
    /// from it via `site_paths` — token paths never shift, so the old
    /// span-shifting arithmetic is gone.
    cst: std::rc::Rc<crate::cst::GreenNode>,
    /// Per edit site: (root child index of the owning declaration, first
    /// and last token-child indices of the literal). Replacements re-lex
    /// to the same token count, so paths stay valid across edits.
    site_paths: Vec<(usize, usize, usize)>,
    tols: Vec<f64>,
    iterations: Vec<(String, Vec<u32>)>,
    /// Predecessors of each micro-node (inverted edges).
    preds: Vec<Vec<usize>>,
    dirty: HashSet<usize>,
}

impl Session {
    pub fn new(src: &str) -> Result<Session, String> {
        let files = vec![crate::SourceFile { name: "model".into(), text: src.to_string() }];
        let segments = vec![crate::Segment { flat_start: 0, flat_end: src.len(), file: 0, local_start: 0 }];
        Session::from_parts(src.to_string(), files, segments)
    }

    /// Build a session from an include-expanded multi-file model, keeping
    /// the source map so grid edits write back into the owning file.
    pub fn new_expanded(exp: crate::Expanded) -> Result<Session, String> {
        Session::from_parts(exp.flat, exp.files, exp.segments)
    }

    /// Build a session from an already-parsed model (e.g. a SALVAGED one:
    /// broken declarations dropped) over the original source text.
    pub fn from_model_parts(
        model: &crate::ast::Model,
        src: String,
        files: Vec<crate::SourceFile>,
        segments: Vec<crate::Segment>,
    ) -> Result<Session, String> {
        let checked = crate::check(model)?;
        Session::from_checked(checked, src, files, segments)
    }

    fn from_parts(
        src: String,
        files: Vec<crate::SourceFile>,
        segments: Vec<crate::Segment>,
    ) -> Result<Session, String> {
        let checked = crate::compile(&src)?;
        Session::from_checked(checked, src, files, segments)
    }

    /// Locate every edit site's literal in the CST as a (decl, first
    /// token, last token) path — the position-independent form of a span.
    fn build_site_paths(
        cst: &crate::cst::GreenNode,
        sites: &[(usize, usize, Option<usize>, (usize, usize), LitKind)],
    ) -> Result<Vec<(usize, usize, usize)>, String> {
        let mut out = Vec::with_capacity(sites.len());
        for (_, _, _, (s, e), _) in sites {
            let mut off = 0usize;
            let mut found = None;
            for (ci, child) in cst.children.iter().enumerate() {
                let w = child.width();
                if let crate::cst::GreenChild::Node(n) = child {
                    if *s >= off && *e <= off + w {
                        // Token-child indices covering [s, e).
                        let (mut toff, mut first, mut last) = (off, None, None);
                        for (ti, tc) in n.children.iter().enumerate() {
                            let tw = tc.width();
                            if toff < *e && *s < toff + tw {
                                if first.is_none() {
                                    first = Some(ti);
                                }
                                last = Some(ti);
                            }
                            toff += tw;
                        }
                        found = Some((ci, first, last));
                        break;
                    }
                }
                off += w;
            }
            match found {
                Some((ci, Some(f), Some(l))) => out.push((ci, f, l)),
                _ => return Err(format!("edit site at bytes {s}..{e} not locatable in the CST")),
            }
        }
        Ok(out)
    }

    fn from_checked(
        checked: Checked,
        src: String,
        files: Vec<crate::SourceFile>,
        segments: Vec<crate::Segment>,
    ) -> Result<Session, String> {
        let cst = crate::cst::parse_cst(&src)?;
        let site_paths = Session::build_site_paths(&cst, &checked.edit_sites)?;
        let mut values = eval::new_values(&checked);
        eval::init_inputs(&checked, &mut values)?;
        let tols = eval::compute_tols(&checked, &mut values)?;
        let iterations = checked
            .solves
            .iter()
            .map(|s| (s.name.clone(), Vec::new()))
            .collect();
        let mut preds = vec![Vec::new(); checked.nodes.len()];
        for (dep, outs) in checked.edges.iter().enumerate() {
            for &to in outs {
                preds[to].push(dep);
            }
        }
        Ok(Session {
            checked,
            values,
            src,
            files,
            segments,
            cst,
            site_paths,
            tols,
            iterations,
            preds,
            dirty: HashSet::new(),
        })
    }

    /// The CURRENT byte span of an edit site, derived from the CST — never
    /// stored, never shifted.
    fn site_span(&self, k: usize) -> (usize, usize) {
        let (decl, first, last) = self.site_paths[k];
        let mut off = 0usize;
        for child in self.cst.children.iter().take(decl) {
            off += child.width();
        }
        let node = match &self.cst.children[decl] {
            crate::cst::GreenChild::Node(n) => n,
            crate::cst::GreenChild::Token(_) => unreachable!("site paths point at decl nodes"),
        };
        let mut s = off;
        for tc in node.children.iter().take(first) {
            s += tc.width();
        }
        let mut e = s;
        for tc in node.children.iter().take(last + 1).skip(first) {
            e += tc.width();
        }
        (s, e)
    }

    /// Full evaluation of every step (also resets incremental state).
    pub fn run_full(&mut self) -> Result<RecalcStats, String> {
        for step in &self.checked.steps {
            eval::exec_step(
                &self.checked,
                &mut self.values,
                &self.tols,
                &mut self.iterations,
                step,
            )?;
        }
        self.dirty.clear();
        Ok(RecalcStats {
            steps_total: self.checked.steps.len(),
            steps_run: self.checked.steps.len(),
            nodes_changed: 0,
        })
    }

    /// Override an input value. `period` picks one period for series inputs
    /// (None = every period in the input's range); scalars ignore it.
    pub fn set_input(
        &mut self,
        name: &str,
        member: Option<&str>,
        period: Option<usize>,
        value: f64,
    ) -> Result<(), String> {
        let m = *self
            .checked
            .index
            .get(name)
            .ok_or_else(|| format!("unknown measure '{name}'"))?;
        let mi = &self.checked.measures[m];
        if !mi.is_input {
            return Err(format!("'{name}' is not an input"));
        }
        let mb = match (mi.dims.len(), member) {
            (0, _) => 0,
            (1, Some(mm)) => {
                let (dim, idx) = *self
                    .checked
                    .member_lookup
                    .get(mm)
                    .ok_or_else(|| format!("unknown member '{mm}'"))?;
                if dim != mi.dims[0] {
                    return Err(format!("member '{mm}' is not in '{name}''s dimension"));
                }
                idx
            }
            (1, None) => return Err(format!("input '{name}' is dimensioned — give a member")),
            _ => return Err(format!("multi-dimension input '{name}' cannot be set via this API yet")),
        };
        let slots: Vec<usize> = if mi.is_series {
            match period {
                Some(t) => {
                    if t < mi.range.0 || t > mi.range.1 {
                        return Err(format!("period index {t} outside the range of '{name}'"));
                    }
                    vec![t]
                }
                None => (mi.range.0..=mi.range.1).collect(),
            }
        } else {
            vec![0]
        };
        // A rounded input snaps at store time, like any posted amount.
        let value = eval::snap(mi, value);
        for t in slots {
            if self.values[m][mb][t] != value {
                self.values[m][mb][t] = value;
                let key = (m, mb, if mi.is_series { t } else { 0 });
                if let Some(&nid) = self.checked.node_id.get(&key) {
                    self.dirty.insert(nid);
                }
            }
        }
        Ok(())
    }

    /// The micro-nodes a step computes.
    fn step_nodes(&self, step: &Step) -> Vec<usize> {
        let ni = &self.checked.node_id;
        match step {
            Step::Eval { m, mb, t } => {
                let key = (*m, *mb, if self.checked.measures[*m].is_series { *t } else { 0 });
                ni.get(&key).copied().into_iter().collect()
            }
            Step::Gs { t, members, .. } => members
                .iter()
                .filter_map(|m| ni.get(&(*m, 0, *t)).copied())
                .collect(),
            Step::Tear { relaxes, inner, .. } => {
                let mut out: Vec<usize> = inner
                    .iter()
                    .filter_map(|(m, t)| {
                        let key = (*m, 0, if self.checked.measures[*m].is_series { *t } else { 0 });
                        ni.get(&key).copied()
                    })
                    .collect();
                for m in relaxes {
                    if let Some(&nid) = ni.get(&(*m, 0, 0)) {
                        out.push(nid);
                    }
                }
                out.sort_unstable();
                out.dedup();
                out
            }
        }
    }

    /// Incremental recalculation: re-run only the steps whose inputs
    /// changed, in plan order, with early cutoff on unchanged values.
    pub fn recalc(&mut self) -> Result<RecalcStats, String> {
        let mut changed: HashSet<usize> = std::mem::take(&mut self.dirty);
        let mut stats = RecalcStats {
            steps_total: self.checked.steps.len(),
            ..Default::default()
        };
        if changed.is_empty() {
            return Ok(stats);
        }
        stats.nodes_changed = changed.len();
        let steps = self.checked.steps.clone();
        for step in &steps {
            let nodes = self.step_nodes(step);
            let needs = nodes
                .iter()
                .any(|&nid| self.preds[nid].iter().any(|p| changed.contains(p)));
            if !needs {
                continue;
            }
            // Snapshot, re-run, early-cutoff compare.
            let old: Vec<f64> = nodes
                .iter()
                .map(|&nid| {
                    let (m, mb, t) = self.checked.nodes[nid];
                    let slot = if self.checked.measures[m].is_series { t } else { 0 };
                    self.values[m][mb][slot]
                })
                .collect();
            eval::exec_step(
                &self.checked,
                &mut self.values,
                &self.tols,
                &mut self.iterations,
                step,
            )?;
            stats.steps_run += 1;
            for (&nid, &before) in nodes.iter().zip(old.iter()) {
                let (m, mb, t) = self.checked.nodes[nid];
                let slot = if self.checked.measures[m].is_series { t } else { 0 };
                let after = self.values[m][mb][slot];
                if after != before {
                    changed.insert(nid);
                    stats.nodes_changed += 1;
                }
            }
        }
        Ok(stats)
    }

    pub fn run_asserts(&mut self) -> Result<Vec<AssertResult>, String> {
        eval::run_asserts(&self.checked, &mut self.values)
    }

    pub fn source(&self) -> &str {
        &self.src
    }

    /// The underlying source files (files[0] = main), kept current by
    /// `patch_input` — grid edits land in the file that owns the span.
    pub fn files(&self) -> &[crate::SourceFile] {
        &self.files
    }

    /// Monte Carlo over the model's distribution inputs (SIPmath posture:
    /// deterministic seeds → reproducible everywhere; trial-aligned draws).
    /// Returns per-cell [p10, p50, p90] for every computed series/scalar.
    /// Base (median) values are restored afterwards.
    pub fn simulate(&mut self, trials: usize) -> Result<SimResult, String> {
        use crate::check::{cholesky, corr_groups, corr_matrix, inv_norm, norm_cdf};
        let dist_inputs: Vec<usize> = self
            .checked
            .measures
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.dist.as_ref().map(|_| i))
            .collect();
        if dist_inputs.is_empty() {
            return Err("no distribution inputs — declare one with '~ metalog {…}' / '~ uniform(a,b)'".into());
        }
        // Correlation groups (Gaussian copula): the Cholesky factor turns
        // independent draws into coherent trial vectors while each input's
        // marginal stays exactly as assessed. Singletons keep the direct
        // uniform path (bit-identical to the uncorrelated engine).
        struct Group {
            ms: Vec<usize>,
            ks: Vec<usize>,
            l: Option<Vec<f64>>,
            per_period: bool,
            range: (usize, usize),
        }
        let mut groups: Vec<Group> = Vec::new();
        for g in corr_groups(&dist_inputs, &self.checked.correlations) {
            let l = if g.len() > 1 {
                Some(
                    cholesky(&corr_matrix(&g, &self.checked.correlations), g.len())
                        .ok_or("correlation matrix is not positive definite")?,
                )
            } else {
                None
            };
            let ks = g
                .iter()
                .map(|m| dist_inputs.iter().position(|x| x == m).unwrap())
                .collect();
            groups.push(Group {
                per_period: self.checked.measures[g[0]].dist_per_period,
                range: self.checked.measures[g[0]].range,
                ms: g,
                ks,
                l,
            });
        }
        let saved_values = self.values.clone();
        let saved_dirty = self.dirty.clone();
        // Collect trial outcomes for every computed measure cell.
        let n_measures = self.checked.measures.len();
        let mut acc: Vec<Vec<Vec<Vec<f64>>>> = (0..n_measures)
            .map(|m| {
                let tuples = self.checked.tuple_count(m);
                let slots = self.values[m][0].len();
                vec![vec![Vec::with_capacity(trials); slots]; tuples]
            })
            .collect();
        fn splitmix(mut x: u64) -> u64 {
            x = x.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }
        // Raw uniform for (input k, trial, optional period). With t = None
        // this is the original per-trial seed path, unchanged.
        let u_raw = |k: usize, trial: usize, t: Option<usize>| -> f64 {
            let mut s = splitmix(((k as u64 + 1) << 32) ^ (trial as u64 + 1));
            if let Some(t) = t {
                s = splitmix(s ^ (t as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15));
            }
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let result = (|| -> Result<(), String> {
            for trial in 0..trials {
                for g in &groups {
                    let times: Vec<Option<usize>> = if g.per_period {
                        (g.range.0..=g.range.1).map(Some).collect()
                    } else {
                        vec![None]
                    };
                    for &t in &times {
                        match &g.l {
                            None => {
                                let (m, k) = (g.ms[0], g.ks[0]);
                                let name = self.checked.measures[m].name.clone();
                                let dist = self.checked.measures[m].dist.clone().unwrap();
                                self.set_input(&name, None, t, dist.quantile(u_raw(k, trial, t)))?;
                            }
                            Some(l) => {
                                let n = g.ms.len();
                                let z: Vec<f64> = g
                                    .ks
                                    .iter()
                                    .map(|&k| inv_norm(u_raw(k, trial, t).clamp(1e-12, 1.0 - 1e-12)))
                                    .collect();
                                for i in 0..n {
                                    let mut zi = 0.0;
                                    for j in 0..=i {
                                        zi += l[i * n + j] * z[j];
                                    }
                                    let m = g.ms[i];
                                    let name = self.checked.measures[m].name.clone();
                                    let dist = self.checked.measures[m].dist.clone().unwrap();
                                    self.set_input(&name, None, t, dist.quantile(norm_cdf(zi)))?;
                                }
                            }
                        }
                    }
                }
                self.recalc()?;
                for m in 0..n_measures {
                    if self.checked.measures[m].is_input {
                        continue;
                    }
                    for mb in 0..self.checked.tuple_count(m) {
                        for (slot, v) in self.values[m][mb].iter().enumerate() {
                            acc[m][mb][slot].push(*v);
                        }
                    }
                }
            }
            Ok(())
        })();
        self.values = saved_values;
        self.dirty = saved_dirty;
        result?;
        let pct = |v: &mut Vec<f64>, p: f64| -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[((v.len() - 1) as f64 * p).round() as usize]
        };
        let mut cells = Vec::new();
        for m in 0..n_measures {
            if self.checked.measures[m].is_input {
                continue;
            }
            for mb in 0..self.checked.tuple_count(m) {
                let label = self.checked.tuple_label(m, mb);
                let display = if label.is_empty() {
                    self.checked.measures[m].name.clone()
                } else {
                    format!("{}[{}]", self.checked.measures[m].name, label)
                };
                let bands: Vec<[f64; 3]> = acc[m][mb]
                    .iter_mut()
                    .map(|v| {
                        if v.is_empty() || v.iter().any(|x| x.is_nan()) {
                            [f64::NAN; 3]
                        } else {
                            [pct(v, 0.10), pct(v, 0.50), pct(v, 0.90)]
                        }
                    })
                    .collect();
                cells.push((display, self.checked.measures[m].is_series, bands));
            }
        }
        Ok(SimResult { trials, cells })
    }

    /// Tornado sensitivity: perturb every literal-editable input site by
    /// ±rel (relative), recalc incrementally, and rank by impact on one
    /// output cell. Perturbations are runtime-only (no source patching)
    /// and fully restored afterwards.
    pub fn tornado(
        &mut self,
        output: &str,
        out_member: Option<&str>,
        out_period: Option<usize>,
        rel: f64,
    ) -> Result<Vec<(String, f64, f64)>, String> {
        let base_out = self.get(output, out_member, out_period)?;
        let saved_values = self.values.clone();
        let saved_dirty = self.dirty.clone();
        // Distinct perturbation targets from the edit sites.
        let mut targets: Vec<(usize, usize, Option<usize>)> = Vec::new();
        for (m, mb, t, _, _) in &self.checked.edit_sites {
            let key = (*m, *mb, *t);
            if !targets.contains(&key) {
                targets.push(key);
            }
        }
        let mut bars: Vec<(String, f64, f64)> = Vec::new();
        for (m, mb, t) in targets {
            let mi = &self.checked.measures[m];
            let name = mi.name.clone();
            let member: Option<String> = if mi.dims.len() == 1 {
                Some(self.checked.tuple_label(m, mb))
            } else {
                None
            };
            let slot = t.unwrap_or(mi.range.0);
            let cur = self.values[m][mb][if mi.is_series { slot } else { 0 }];
            if !cur.is_finite() || cur == 0.0 {
                continue; // relative perturbation of zero is meaningless
            }
            let mut outs = [0.0f64; 2];
            let mut failed = false;
            for (k, dir) in [-rel, rel].iter().enumerate() {
                let r = (|| -> Result<f64, String> {
                    self.set_input(&name, member.as_deref(), t, cur * (1.0 + dir))?;
                    self.recalc()?;
                    self.get(output, out_member, out_period)
                })();
                match r {
                    Ok(v) => outs[k] = v - base_out,
                    Err(_) => failed = true, // e.g. a solve stops converging
                }
                self.values = saved_values.clone();
                self.dirty = saved_dirty.clone();
                if failed {
                    break;
                }
            }
            if !failed {
                let label = match (&member, t) {
                    (Some(mm), Some(tt)) => {
                        format!("{name}[{mm}]@{}", self.checked.calendar.label(tt))
                    }
                    (Some(mm), None) => format!("{name}[{mm}]"),
                    (None, Some(tt)) => format!("{name}@{}", self.checked.calendar.label(tt)),
                    (None, None) => name.clone(),
                };
                bars.push((label, outs[0], outs[1]));
            }
        }
        bars.sort_by(|a, b| {
            let ia = a.1.abs().max(a.2.abs());
            let ib = b.1.abs().max(b.2.abs());
            ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(bars)
    }

    pub fn scenario_names(&self) -> Vec<String> {
        self.checked.scenarios.iter().map(|s| s.name.clone()).collect()
    }

    /// Evaluate a scenario as an incremental delta from the CURRENT (Base)
    /// values: clone, apply the override chain, recalc only what changed,
    /// return the scenario's values and stats. Base values are untouched.
    pub fn eval_scenario(&mut self, name: &str) -> Result<(Values, RecalcStats), String> {
        let idx = self
            .checked
            .scenarios
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| format!("unknown scenario '{name}'"))?;
        // Root-first override chain.
        let mut chain = vec![idx];
        let mut cur = idx;
        while let Some(p) = self.checked.scenarios[cur].parent {
            chain.push(p);
            cur = p;
        }
        chain.reverse();

        let saved_values = self.values.clone();
        let saved_dirty = std::mem::take(&mut self.dirty);
        let result = (|| -> Result<(Values, RecalcStats), String> {
            for &sc in &chain {
                let overrides = self.checked.scenarios[sc].overrides.clone();
                for (m, body) in &overrides {
                    self.apply_override(*m, body)?;
                }
            }
            let stats = self.recalc()?;
            Ok((self.values.clone(), stats))
        })();
        self.values = saved_values;
        self.dirty = saved_dirty;
        result
    }

    fn apply_override(&mut self, m: usize, body: &Body) -> Result<(), String> {
        let mi = &self.checked.measures[m];
        let is_series = mi.is_series;
        let (r0, r1) = mi.range;
        for mb in 0..self.checked.tuple_count(m) {
            let asg = self.checked.asg_of_tuple(m, mb);
            // Compute the new values first (immutable borrow), then write.
            let mut writes: Vec<(usize, f64)> = Vec::new();
            {
                let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
                match body {
                    Body::DimMatch { .. } => {
                        return Err("match bodies in scenario overrides are not supported yet".into())
                    }
                    Body::Map(entries) => {
                        for (lit, e) in entries {
                            let t = self.checked.calendar.index(lit)?;
                            writes.push((t, ctx.eval(e, &asg, t)?));
                        }
                    }
                    Body::Expr(e) => {
                        if is_series {
                            for t in r0..=r1 {
                                writes.push((t, ctx.eval(e, &asg, t)?));
                            }
                        } else {
                            writes.push((0, ctx.eval(e, &asg, 0)?));
                        }
                    }
                }
            }
            for (t, v) in writes {
                let v = eval::snap(&self.checked.measures[m], v);
                if self.values[m][mb][t] != v {
                    self.values[m][mb][t] = v;
                    let key = (m, mb, if is_series { t } else { 0 });
                    if let Some(&nid) = self.checked.node_id.get(&key) {
                        self.dirty.insert(nid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Grid → text write-back: rewrite the literal that defines an input in
    /// the SOURCE, byte-exactly (everything else preserved), then apply the
    /// same change to the runtime incrementally. A per-period map entry
    /// patches one period; a broadcast/scalar literal patches the single
    /// literal — and therefore every period it defines, exactly as the text
    /// says. Returns the periods the runtime change applied to.
    pub fn patch_input(
        &mut self,
        name: &str,
        member: Option<&str>,
        period: Option<usize>,
        value: f64,
    ) -> Result<(), String> {
        let m = *self
            .checked
            .index
            .get(name)
            .ok_or_else(|| format!("unknown measure '{name}'"))?;
        let want_mb = match (member, self.checked.measures[m].dims.len()) {
            (None, 0) => 0,
            (Some(mm), 1) => {
                let (dim, idx) = *self
                    .checked
                    .member_lookup
                    .get(mm)
                    .ok_or_else(|| format!("unknown member '{mm}'"))?;
                if dim != self.checked.measures[m].dims[0] {
                    return Err(format!("member '{mm}' is not in '{name}''s dimension"));
                }
                idx
            }
            (None, _) => return Err(format!("input '{name}' is dimensioned — give a member")),
            (Some(_), 0) => 0,
            _ => return Err(format!("multi-dimension input '{name}' is not patchable yet")),
        };
        // Prefer an exact per-period site; fall back to a broadcast site.
        let mut chosen: Option<usize> = None;
        for (k, (sm, smb, st, _, _)) in self.checked.edit_sites.iter().enumerate() {
            if *sm != m || *smb != want_mb {
                continue;
            }
            if *st == period {
                chosen = Some(k);
                break;
            }
            if st.is_none() && chosen.is_none() {
                chosen = Some(k);
            }
        }
        let k = chosen.ok_or_else(|| {
            format!("'{name}' is not literal-editable at this position (formula-defined input)")
        })?;
        let (_, _, site_t, _, kind) = self.checked.edit_sites[k].clone();
        // The site's CURRENT span, derived from the CST via its token path.
        let span = self.site_span(k);

        let rep = match &kind {
            LitKind::Num => fmt_plain(value),
            LitKind::Pct => format!("{}%", fmt_plain(value * 100.0)),
            LitKind::Qty(u) => format!("{} {u}", fmt_plain(value)),
        };
        let old_len = span.1 - span.0;
        // Route the patch to the owning source file (span provenance): find
        // the segment containing the span and splice the same bytes there.
        let seg = self
            .segments
            .iter()
            .position(|s| span.0 >= s.flat_start && span.1 <= s.flat_end)
            .ok_or_else(|| "edit span is not traceable to a source file".to_string())?;
        let (owner, local) = {
            let s = &self.segments[seg];
            (s.file, span.0 - s.flat_start + s.local_start)
        };
        // A file region expanded more than once (same file included twice)
        // would need multi-site flat patching — refuse rather than desync.
        for (si, s) in self.segments.iter().enumerate() {
            if si == seg || s.file != owner {
                continue;
            }
            let hi = s.local_start + (s.flat_end - s.flat_start);
            if s.local_start < local + old_len && local < hi {
                return Err(format!(
                    "\"{}\" is included more than once — not grid-editable",
                    self.files[owner].name
                ));
            }
        }
        self.files[owner].text.replace_range(local..local + old_len, &rep);
        // The flat source is the CST's reprint: replace the literal's
        // tokens 1:1 with the re-lexed replacement (same token count by
        // construction — Num→Num, Pct→Pct, Qty→Num·Ws·Ident), so every
        // site path everywhere stays valid. No span shifting exists.
        let reps = crate::cst::lex_green_tokens(&rep)?;
        let (decl, first, last) = self.site_paths[k];
        if reps.len() != last - first + 1 {
            return Err(format!(
                "internal: replacement '{rep}' lexes to {} tokens over a {}-token site",
                reps.len(),
                last - first + 1
            ));
        }
        self.cst = crate::cst::replace_tokens(&self.cst, decl, first, last, reps)?;
        self.src = self.cst.text();
        let delta = rep.len() as isize - old_len as isize;
        for (si, s) in self.segments.iter_mut().enumerate() {
            if si == seg {
                s.flat_end = (s.flat_end as isize + delta) as usize;
            } else if s.flat_start >= span.1 {
                s.flat_start = (s.flat_start as isize + delta) as usize;
                s.flat_end = (s.flat_end as isize + delta) as usize;
            }
            if s.file == owner && si != seg && s.local_start >= local + old_len {
                s.local_start = (s.local_start as isize + delta) as usize;
            }
        }
        // Runtime: broadcast sites change every period the literal defines.
        self.set_input(name, member, site_t, value)
    }

    /// Read a value: member for dimensioned measures, period for series.
    pub fn get(&self, name: &str, member: Option<&str>, period: Option<usize>) -> Result<f64, String> {
        let m = *self
            .checked
            .index
            .get(name)
            .ok_or_else(|| format!("unknown measure '{name}'"))?;
        let mi = &self.checked.measures[m];
        let mb = match (mi.dims.len(), member) {
            (0, _) => 0,
            (1, Some(mm)) => {
                let (dim, idx) = *self
                    .checked
                    .member_lookup
                    .get(mm)
                    .ok_or_else(|| format!("unknown member '{mm}'"))?;
                if dim != mi.dims[0] {
                    return Err(format!("member '{mm}' is not in '{name}''s dimension"));
                }
                idx
            }
            (1, None) => return Err(format!("'{name}' is dimensioned — give a member")),
            _ => return Err(format!("multi-dimension '{name}' cannot be read via this API yet")),
        };
        let slot = if mi.is_series {
            period.ok_or_else(|| format!("'{name}' is a series — give a period index"))?
        } else {
            0
        };
        Ok(self.values[m][mb][slot])
    }

    /// Goal-seek (the IFPS classic): find the value of `input` that makes
    /// `output` equal `target`. Safeguarded secant iteration over runtime
    /// values only — the model is fully restored afterwards; committing
    /// the solution is the caller's decision (e.g. via `patch_input`).
    pub fn goal_seek(
        &mut self,
        input: &str,
        in_member: Option<&str>,
        in_period: Option<usize>,
        output: &str,
        out_member: Option<&str>,
        out_period: Option<usize>,
        target: f64,
    ) -> Result<GoalSeekResult, String> {
        let m = *self
            .checked
            .index
            .get(input)
            .ok_or_else(|| format!("unknown measure '{input}'"))?;
        if !self.checked.measures[m].is_input {
            return Err(format!("'{input}' is not an input — goal-seek adjusts inputs"));
        }
        let read_p = if self.checked.measures[m].is_series {
            Some(in_period.unwrap_or(self.checked.measures[m].range.0))
        } else {
            None
        };
        let x0 = self.get(input, in_member, read_p)?;
        let saved_values = self.values.clone();
        let saved_dirty = self.dirty.clone();
        let mut evals = 0usize;
        let result = (|| -> Result<GoalSeekResult, String> {
            let mut eval_at = |s: &mut Self, x: f64| -> Result<f64, String> {
                evals += 1;
                s.set_input(input, in_member, in_period, x)?;
                s.recalc()?;
                s.get(output, out_member, out_period)
            };
            let scale = x0.abs().max(1e-6);
            let tol = target.abs().max(1.0) * 1e-9;
            let mut a = x0;
            let mut fa = eval_at(self, a)? - target;
            if fa.abs() <= tol {
                return Ok(GoalSeekResult { value: a, achieved: a_target(fa, target), iterations: evals });
            }
            let mut b = if x0 != 0.0 { x0 * 1.05 } else { 0.1 * scale };
            let mut fb = eval_at(self, b)? - target;
            if fa == fb {
                return Err(format!("'{output}' does not respond to '{input}' — pick a lever it depends on"));
            }
            for _ in 0..64 {
                if fb.abs() <= tol || (b - a).abs() <= scale * 1e-12 {
                    return Ok(GoalSeekResult { value: b, achieved: a_target(fb, target), iterations: evals });
                }
                let denom = fb - fa;
                if denom.abs() < 1e-300 {
                    return Err(format!("goal-seek stalled: '{output}' stopped responding near {b}"));
                }
                // Secant step, clamped to avoid wild leaps through solves.
                let mut c = b - fb * (b - a) / denom;
                if !c.is_finite() {
                    return Err("goal-seek diverged (non-finite step)".into());
                }
                let max_step = 100.0 * scale.max(b.abs());
                if (c - b).abs() > max_step {
                    c = b + max_step * (c - b).signum();
                }
                a = b;
                fa = fb;
                b = c;
                fb = eval_at(self, b)? - target;
            }
            Err(format!(
                "goal-seek did not converge in 64 iterations (last: {input} = {b}, {output} = {})",
                fb + target
            ))
        })();
        self.values = saved_values;
        self.dirty = saved_dirty;
        result
    }

    // ---- structural edits: declaration-level CST operations ------------
    // These are SOURCE transformations: they return new file texts and the
    // caller recompiles — structural edits are rare, recompiles are ~ms.

    /// Route a set of non-overlapping flat-source edits into the owning
    /// files (descending order, so positions stay valid per file).
    fn apply_flat_edits(&self, mut edits: Vec<(usize, usize, String)>) -> Result<Vec<(String, String)>, String> {
        edits.sort_by(|a, b| b.0.cmp(&a.0));
        let mut texts: Vec<(String, String)> =
            self.files.iter().map(|f| (f.name.clone(), f.text.clone())).collect();
        for (s, e, rep) in edits {
            let seg = self
                .segments
                .iter()
                .find(|g| s >= g.flat_start && e <= g.flat_end)
                .ok_or_else(|| format!("edit at {s}..{e} is not inside a single source file"))?;
            let local = s - seg.flat_start + seg.local_start;
            texts[seg.file].1.replace_range(local..local + (e - s), &rep);
        }
        Ok(texts)
    }

    /// Add one period at the end of the calendar: bump the calendar
    /// declaration's end literal and extend every FULL-RANGE map input
    /// with a copy of its last entry (sub-range maps like closed actuals
    /// keep their range). Returns (new file texts, the new period label).
    pub fn add_period(&self) -> Result<(Vec<(String, String)>, String), String> {
        use crate::cst::{Red, RedChild, SyntaxKind};
        let cal = &self.checked.calendar;
        let new_label = cal.label(cal.len);
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        // The calendar declaration's end literal: non-trivia tokens after `..`.
        let cal_decl = Red::root(&self.cst)
            .decls()
            .into_iter()
            .find(|d| d.green.kind == SyntaxKind::CalendarDecl)
            .ok_or("no calendar declaration found")?;
        let (mut after_dots, mut s0, mut e0) = (false, None, 0usize);
        for c in cal_decl.children() {
            if let RedChild::Token { kind, text, offset } = c {
                let trivia = matches!(kind, SyntaxKind::Whitespace | SyntaxKind::Comment);
                if after_dots && !trivia {
                    if s0.is_none() {
                        s0 = Some(offset);
                    }
                    e0 = offset + text.len();
                }
                if kind == SyntaxKind::Sym && text == ".." {
                    after_dots = true;
                }
            }
        }
        let s0 = s0.ok_or("calendar end literal not found")?;
        edits.push((s0, e0, new_label.clone()));
        // Extend each full-range map: insert after the last period's entry.
        for (k, (m, mb, t, _, kind)) in self.checked.edit_sites.iter().enumerate() {
            if *t != Some(cal.len - 1) || self.checked.measures[*m].range != (0, cal.len - 1) {
                continue;
            }
            let v = self.values[*m][*mb][cal.len - 1];
            let lit = match kind {
                LitKind::Num => fmt_plain(v),
                LitKind::Pct => format!("{}%", fmt_plain(v * 100.0)),
                LitKind::Qty(u) => format!("{} {u}", fmt_plain(v)),
            };
            let (_, e) = self.site_span(k);
            edits.push((e, e, format!(", {new_label}: {lit}")));
        }
        Ok((self.apply_flat_edits(edits)?, new_label))
    }

    /// Add a member to a dimension: extend the member list and insert a
    /// `member -> default` arm into every `match Dim { … }` block that
    /// lacks an `else` (which would already cover the newcomer). Tree
    /// rollups include the member automatically. Refused for the
    /// functional dimension (a new entity needs a currency mapping).
    pub fn add_member(&self, dim: &str, member: &str, default: &str) -> Result<Vec<(String, String)>, String> {
        use crate::cst::{Red, RedChild, SyntaxKind};
        let did = self
            .checked
            .dims
            .iter()
            .position(|d| d.name == dim)
            .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
        if self.checked.functional_dim == Some(did) {
            return Err(format!(
                "'{dim}' carries functional currencies — add the member and its currency mapping in the source"
            ));
        }
        let ok_ident = crate::lexer::lex(member)
            .ok()
            .map(|t| t.len() == 1 && matches!(t[0].tok, crate::lexer::Tok::Ident(_)))
            .unwrap_or(false);
        if !ok_ident || crate::parser::is_keyword(member) {
            return Err(format!("'{member}' is not a valid member name"));
        }
        let c = &self.checked;
        if c.member_lookup.contains_key(member)
            || c.group_lookup.contains_key(member)
            || c.index.contains_key(member)
            || c.dims.iter().any(|d| d.name == member)
            || c.unit_reg.contains_key(member)
            || c.range_index.contains_key(member)
        {
            return Err(format!("'{member}' already names something else in this model"));
        }
        if default.trim().is_empty() || crate::lexer::lex(default).is_err() {
            return Err("the default value must be a valid expression".into());
        }

        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for decl in Red::root(&self.cst).decls() {
            let toks: Vec<(SyntaxKind, &str, usize)> = decl
                .children()
                .into_iter()
                .filter_map(|ch| match ch {
                    RedChild::Token { kind, text, offset } => Some((kind, text, offset)),
                    RedChild::Node(_) => None,
                })
                .collect();
            // 1) The dimension declaration: append after the last member.
            if decl.green.kind == SyntaxKind::DimensionDecl
                && crate::cst::decl_name(decl.green).as_deref() == Some(dim)
            {
                let last_ident = toks
                    .iter()
                    .rev()
                    .find(|(k, _, _)| *k == SyntaxKind::Ident)
                    .ok_or("malformed dimension declaration")?;
                let end = last_ident.2 + last_ident.1.len();
                edits.push((end, end, format!(", {member}")));
            }
            // 2) Every `match <dim> {` block without an `else` arm.
            let real: Vec<usize> = toks
                .iter()
                .enumerate()
                .filter(|(_, (k, _, _))| {
                    !matches!(k, SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::Directive)
                })
                .map(|(i, _)| i)
                .collect();
            for w in 0..real.len() {
                let i = real[w];
                if toks[i].1 != "match" || toks[i].0 != SyntaxKind::Ident {
                    continue;
                }
                let (Some(&i1), Some(&i2)) = (real.get(w + 1), real.get(w + 2)) else { continue };
                if !(toks[i1].0 == SyntaxKind::Ident && toks[i1].1 == dim && toks[i2].1 == "{") {
                    continue;
                }
                // Find the matching close brace; note any depth-1 `else`.
                let (mut depth, mut has_else, mut close) = (1i32, false, None);
                for &j in real.iter().skip(w + 3) {
                    match toks[j].1 {
                        "{" => depth += 1,
                        "}" => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        "else" if depth == 1 => has_else = true,
                        _ => {}
                    }
                }
                let Some(cj) = close else { continue };
                if has_else {
                    continue; // the else arm already covers the new member
                }
                // Multi-line blocks get the arm on its own line; inline
                // blocks stay inline.
                let multiline = toks[cj - 1].0 == SyntaxKind::Whitespace && toks[cj - 1].1.contains('\n');
                let ins = if multiline {
                    format!("  {member} -> {default}\n")
                } else {
                    format!(" {member} -> {default} ")
                };
                let pos = toks[cj].2;
                edits.push((pos, pos, ins));
            }
        }
        self.apply_flat_edits(edits)
    }

    /// Rename a measure everywhere — token-exact across every file, with
    /// namespace-collision guards. Comments are deliberately untouched.
    pub fn rename_measure(&self, old: &str, new: &str) -> Result<Vec<(String, String)>, String> {
        use crate::cst::{GreenChild, GreenNode, SyntaxKind};
        if !self.checked.index.contains_key(old) {
            return Err(format!("unknown measure '{old}'"));
        }
        let ok_ident = crate::lexer::lex(new)
            .ok()
            .map(|t| t.len() == 1 && matches!(t[0].tok, crate::lexer::Tok::Ident(_)))
            .unwrap_or(false);
        if !ok_ident || crate::parser::is_keyword(new) {
            return Err(format!("'{new}' is not a valid measure name"));
        }
        let c = &self.checked;
        if c.index.contains_key(new)
            || c.member_lookup.contains_key(new)
            || c.group_lookup.contains_key(new)
            || c.dims.iter().any(|d| d.name == new)
            || c.unit_reg.contains_key(new)
            || c.range_index.contains_key(new)
            || c.scenarios.iter().any(|s| s.name == new)
        {
            return Err(format!("'{new}' already names something else in this model"));
        }
        fn collect(n: &GreenNode, off: usize, old: &str, new: &str, out: &mut Vec<(usize, usize, String)>) {
            let mut o = off;
            for ch in &n.children {
                match ch {
                    GreenChild::Node(inner) => collect(inner, o, old, new, out),
                    GreenChild::Token(t) => {
                        if t.kind == SyntaxKind::Ident && t.text == old {
                            out.push((o, o + t.text.len(), new.to_string()));
                        }
                    }
                }
                o += ch.width();
            }
        }
        let mut edits = Vec::new();
        collect(&self.cst, 0, old, new, &mut edits);
        self.apply_flat_edits(edits)
    }

    /// Map a 1-based line of the flat source to (owning file, local line)
    /// through the include source map.
    pub fn locate_line(&self, line: usize) -> (String, usize) {
        let mut off = 0usize;
        for (i, l) in self.src.split_inclusive('\n').enumerate() {
            if i + 1 == line {
                break;
            }
            off += l.len();
        }
        for s in &self.segments {
            if off >= s.flat_start && off < s.flat_end {
                let local = s.local_start + (off - s.flat_start);
                let lline = self.files[s.file].text[..local].matches('\n').count() + 1;
                return (self.files[s.file].name.clone(), lline);
            }
        }
        (self.files[0].name.clone(), line)
    }

    /// "Explain this number": the provenance of one cell — where it is
    /// defined (routed to the owning file), which match/actuals arm fired
    /// for this period, and every direct dependency cell with its value.
    /// Drill-down = calling explain again on a dependency.
    pub fn explain(
        &mut self,
        name: &str,
        member: Option<&str>,
        period: Option<usize>,
    ) -> Result<Explanation, String> {
        let m = *self
            .checked
            .index
            .get(name)
            .ok_or_else(|| format!("unknown measure '{name}'"))?;
        let mi = &self.checked.measures[m];
        let mb = match (mi.dims.len(), member) {
            (0, _) => 0,
            (1, Some(mm)) => {
                let (dim, idx) = *self
                    .checked
                    .member_lookup
                    .get(mm)
                    .ok_or_else(|| format!("unknown member '{mm}'"))?;
                if dim != mi.dims[0] {
                    return Err(format!("member '{mm}' is not in '{name}''s dimension"));
                }
                idx
            }
            (1, None) => return Err(format!("'{name}' is dimensioned — give a member")),
            _ => return Err(format!("multi-dimension '{name}' cannot be explained via this API yet")),
        };
        let is_series = mi.is_series;
        let slot = if is_series {
            period.ok_or_else(|| format!("'{name}' is a series — give a period index"))?
        } else {
            0
        };
        let t = if is_series { slot } else { mi.range.0 };
        let value = self.values[m][mb][slot];
        let unit = match &mi.munit {
            MUnit::Uniform(u) => format!("{u}"),
            MUnit::Local => "local".to_string(),
        };
        let member_label = if mi.dims.len() == 1 { self.checked.tuple_label(m, mb) } else { String::new() };
        let is_input = mi.is_input;
        let line = mi.line;
        let (file, local_line) = self.locate_line(line);

        let mut notes: Vec<String> = Vec::new();
        if let Some(d) = &mi.dist {
            let base = match d {
                Dist::Metalog { .. } => format!("~ metalog · median {:.4}", d.median()),
                Dist::Uniform { a, b } => format!("~ uniform({a}, {b})"),
                Dist::Normal { mu, sd } => format!("~ normal({mu}, {sd})"),
            };
            let freq = if mi.dist_per_period { "fresh draw each period" } else { "one draw per trial" };
            notes.push(format!("{base} · {freq} · deterministic base uses the median"));
            for &(a, b, rho) in &self.checked.correlations {
                if a == m || b == m {
                    let other = if a == m { b } else { a };
                    notes.push(format!("correlated {rho} with '{}'", self.checked.measures[other].name));
                }
            }
        }
        if let Some(s) = mi.solve {
            notes.push(format!("computed inside solve '{}'", self.checked.solves[s].name));
        }
        if is_input
            && self
                .checked
                .edit_sites
                .iter()
                .any(|(sm, smb, ..)| *sm == m && *smb == mb)
        {
            notes.push("literal input — editable in the grid".into());
        }

        let mut deps = Vec::new();
        let mut arm = String::new();
        let mut terms = Vec::new();
        if !is_input {
            let asg = self.checked.asg_of_tuple(m, mb);
            let body = mi.body.clone();
            if let Body::Expr(e) = &body {
                self.collect(e, &asg, t, "", &mut deps, &mut arm)?;
                self.collect_terms(e, &asg, t, 1.0, &mut terms)?;
            }
        }
        Ok(Explanation {
            name: name.to_string(),
            member: member_label,
            period: if is_series { Some(slot) } else { None },
            value,
            unit,
            is_input,
            file,
            line: local_line,
            arm,
            note: notes.join(" · "),
            deps,
            terms,
        })
    }

    fn push_dep(&self, deps: &mut Vec<Dep>, m: usize, mb: usize, period: Option<isize>, via: &str) {
        let mi = &self.checked.measures[m];
        let (p, label, value) = if !mi.is_series {
            (None, String::new(), self.values[m][mb][0])
        } else {
            match period {
                Some(p) if p >= mi.range.0 as isize && p <= mi.range.1 as isize => (
                    Some(p as usize),
                    self.checked.calendar.label(p as usize),
                    self.values[m][mb][p as usize],
                ),
                _ => (None, "out of range".to_string(), 0.0),
            }
        };
        let member = if mi.dims.len() == 1 { self.checked.tuple_label(m, mb) } else { String::new() };
        if deps
            .iter()
            .any(|d| d.name == mi.name && d.member == member && d.period == p && d.via == via && d.label == label)
        {
            return;
        }
        deps.push(Dep {
            name: mi.name.clone(),
            member,
            period: p,
            label,
            value,
            is_input: mi.is_input,
            via: via.to_string(),
        });
    }

    /// A cell-reference term (value read straight from the store, signed).
    fn push_cell_term(&self, out: &mut Vec<Term>, m: usize, mb: usize, period: Option<isize>, sign: f64) {
        let mi = &self.checked.measures[m];
        let (p, value) = if !mi.is_series {
            (None, self.values[m][mb][0])
        } else {
            match period {
                Some(p) if p >= mi.range.0 as isize && p <= mi.range.1 as isize => {
                    (Some(p as usize), self.values[m][mb][p as usize])
                }
                _ => (None, 0.0),
            }
        };
        let member = if mi.dims.len() == 1 { self.checked.tuple_label(m, mb) } else { String::new() };
        let disp = if member.is_empty() { mi.name.clone() } else { format!("{}[{member}]", mi.name) };
        let label = match p {
            Some(pp) => format!("{disp} @ {}", self.checked.calendar.label(pp)),
            None => disp,
        };
        out.push(Term {
            label: if sign < 0.0 { format!("− {label}") } else { label },
            value: sign * value,
            cell: Some((mi.name.clone(), member, p)),
        });
    }

    /// An opaque term: anything non-additive, evaluated as one piece.
    fn push_expr_term(&mut self, out: &mut Vec<Term>, e: &Expr, asg: &[usize], t: usize, sign: f64) -> Result<(), String> {
        let v = {
            let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
            ctx.eval(e, asg, t)?
        };
        let label = render_expr(e);
        out.push(Term {
            label: if sign < 0.0 { format!("− {label}") } else { label },
            value: sign * v,
            cell: None,
        });
        Ok(())
    }

    /// Exact additive decomposition of the TAKEN branch: walk +/−, expand
    /// aggregates, rollups and npv into constituents; everything else is
    /// one opaque term. The terms always sum to the cell's value.
    fn collect_terms(&mut self, e: &Expr, asg: &[usize], t: usize, sign: f64, out: &mut Vec<Term>) -> Result<(), String> {
        use crate::ast::BinOp;
        match e {
            Expr::Bin(BinOp::Add, a, b) => {
                self.collect_terms(a, asg, t, sign, out)?;
                self.collect_terms(b, asg, t, sign, out)?;
            }
            Expr::Bin(BinOp::Sub, a, b) => {
                self.collect_terms(a, asg, t, sign, out)?;
                self.collect_terms(b, asg, t, -sign, out)?;
            }
            Expr::Neg(x) => self.collect_terms(x, asg, t, -sign, out)?,
            Expr::Ref(name) => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                self.push_cell_term(out, m, mb, Some(t as isize), sign);
            }
            Expr::Prev(name, inline_init) => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let ts = t as isize - 1;
                if ts >= self.checked.measures[m].range.0 as isize {
                    self.push_cell_term(out, m, mb, Some(ts), sign);
                } else {
                    let init = inline_init
                        .as_deref()
                        .cloned()
                        .or_else(|| self.checked.measures[m].init.clone());
                    let v = match &init {
                        Some(ie) => {
                            let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
                            ctx.eval(ie, asg, t)?
                        }
                        None => f64::NAN,
                    };
                    let label = format!("{name} init");
                    out.push(Term {
                        label: if sign < 0.0 { format!("− {label}") } else { label },
                        value: sign * v,
                        cell: None,
                    });
                }
            }
            Expr::At { name, bound } => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let at = self.checked.resolve_bound(bound, t)?;
                self.push_cell_term(out, m, mb, Some(at), sign);
            }
            Expr::MemberIx { name, members } => {
                let m = self.checked.index[name];
                let mut asgs = vec![asg.to_vec()];
                for mname in members {
                    if let Some(&(dim, idx)) = self.checked.member_lookup.get(mname) {
                        for a in asgs.iter_mut() {
                            a[dim] = idx;
                        }
                    } else if let Some(&dim) = self.checked.group_lookup.get(mname) {
                        let mut next = Vec::new();
                        for a in &asgs {
                            for idx in 0..self.checked.dims[dim].members.len() {
                                let mut a2 = a.clone();
                                a2[dim] = idx;
                                next.push(a2);
                            }
                        }
                        asgs = next;
                    } else {
                        return Err(format!("unknown member '{mname}'"));
                    }
                }
                for a in &asgs {
                    let mb = self.checked.tuple_of(m, a)?;
                    self.push_cell_term(out, m, mb, Some(t as isize), sign);
                }
            }
            Expr::WindowSum { name, from, to } => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let a = self.checked.resolve_bound(from, t)?;
                let b = self.checked.resolve_bound(to, t)?;
                for at in a..=b {
                    self.push_cell_term(out, m, mb, Some(at), sign);
                }
            }
            Expr::RangeSum { range, body } => {
                if let Some(did) = self.checked.dim_by_name(range) {
                    for c in 0..self.checked.dims[did].members.len() {
                        let mut a = asg.to_vec();
                        a[did] = c;
                        self.collect_terms(body, &a, t, sign, out)?;
                    }
                } else {
                    let r = self
                        .checked
                        .range_of(range)
                        .ok_or_else(|| format!("unknown period range '{range}'"))?
                        .clone();
                    for p in r.start..=r.end {
                        self.collect_terms(body, asg, p, sign, out)?;
                    }
                }
            }
            Expr::Npv { rate, body, range } => {
                // The PV bridge: one discounted term per period.
                let r = self
                    .checked
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?
                    .clone();
                let rt = {
                    let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
                    ctx.eval(rate, asg, t)?
                };
                for (i, p) in (r.start..=r.end).enumerate() {
                    let v = {
                        let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
                        ctx.eval(body, asg, p)?
                    } / (1.0 + rt).powi(i as i32 + 1);
                    let label = format!("PV @ {}", self.checked.calendar.label(p));
                    out.push(Term {
                        label: if sign < 0.0 { format!("− {label}") } else { label },
                        value: sign * v,
                        cell: None,
                    });
                }
            }
            Expr::When { value, pos, range } => {
                let r = self
                    .checked
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?;
                let boundary = match pos {
                    FirstLast::First => r.start,
                    FirstLast::Last => r.end,
                };
                if t == boundary {
                    self.collect_terms(value, asg, t, sign, out)?;
                }
            }
            Expr::MatchT(arms) => {
                for (set, e2) in arms {
                    let base = self
                        .checked
                        .range_of(&set.base)
                        .ok_or_else(|| format!("unknown period range '{}'", set.base))?;
                    let excluded = match &set.minus {
                        Some(x) => self
                            .checked
                            .range_of(x)
                            .ok_or_else(|| format!("unknown period range '{x}'"))?
                            .contains(t),
                        None => false,
                    };
                    if base.contains(t) && !excluded {
                        return self.collect_terms(e2, asg, t, sign, out);
                    }
                }
            }
            Expr::MatchDim { dim, arms, default } => {
                let did = self
                    .checked
                    .dim_by_name(dim)
                    .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                let c = asg[did];
                if c == UNBOUND {
                    return Err(format!("match on {dim} outside a {dim}-bound context"));
                }
                let mname = self.checked.dims[did].members[c].clone();
                for (arm_member, e2) in arms {
                    if *arm_member == mname {
                        return self.collect_terms(e2, asg, t, sign, out);
                    }
                }
                if let Some(def) = default {
                    return self.collect_terms(def, asg, t, sign, out);
                }
            }
            // Non-additive top level: one opaque term (Mul/Div/Pow, calls,
            // conversions, literals, irr, annualize, year(t)).
            other => self.push_expr_term(out, other, asg, t, sign)?,
        }
        Ok(())
    }

    /// Collect the direct dependency cells of the TAKEN branch of `e`.
    fn collect(
        &mut self,
        e: &Expr,
        asg: &[usize],
        t: usize,
        via: &str,
        deps: &mut Vec<Dep>,
        arm: &mut String,
    ) -> Result<(), String> {
        match e {
            Expr::Num(_) | Expr::Qty(_, _) | Expr::Pct(_) | Expr::YearT => {}
            Expr::Ref(name) => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                self.push_dep(deps, m, mb, Some(t as isize), via);
            }
            Expr::Prev(name, inline_init) => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let ts = t as isize - 1;
                if ts >= self.checked.measures[m].range.0 as isize {
                    self.push_dep(deps, m, mb, Some(ts), "prev");
                } else {
                    // Boundary: the init expression supplies the value.
                    let init = inline_init
                        .as_deref()
                        .cloned()
                        .or_else(|| self.checked.measures[m].init.clone());
                    let v = match &init {
                        Some(ie) => {
                            let ctx = eval::Ctx { c: &self.checked, values: &mut self.values };
                            ctx.eval(ie, asg, t)?
                        }
                        None => f64::NAN,
                    };
                    let mi = &self.checked.measures[m];
                    let member =
                        if mi.dims.len() == 1 { self.checked.tuple_label(m, mb) } else { String::new() };
                    deps.push(Dep {
                        name: mi.name.clone(),
                        member,
                        period: None,
                        label: "init".to_string(),
                        value: v,
                        is_input: mi.is_input,
                        via: "prev".to_string(),
                    });
                }
            }
            Expr::MemberIx { name, members } => {
                let m = self.checked.index[name];
                let mut asgs = vec![asg.to_vec()];
                let mut rolled = false;
                for mname in members {
                    if let Some(&(dim, idx)) = self.checked.member_lookup.get(mname) {
                        for a in asgs.iter_mut() {
                            a[dim] = idx;
                        }
                    } else if let Some(&dim) = self.checked.group_lookup.get(mname) {
                        rolled = true;
                        let mut next = Vec::new();
                        for a in &asgs {
                            for idx in 0..self.checked.dims[dim].members.len() {
                                let mut a2 = a.clone();
                                a2[dim] = idx;
                                next.push(a2);
                            }
                        }
                        asgs = next;
                    } else {
                        return Err(format!("unknown member '{mname}'"));
                    }
                }
                let v = if rolled { "rollup" } else { via };
                for a in &asgs {
                    let mb = self.checked.tuple_of(m, a)?;
                    self.push_dep(deps, m, mb, Some(t as isize), v);
                }
            }
            Expr::At { name, bound } => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let at = self.checked.resolve_bound(bound, t)?;
                self.push_dep(deps, m, mb, Some(at), "at");
            }
            Expr::WindowSum { name, from, to } => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let a = self.checked.resolve_bound(from, t)?;
                let b = self.checked.resolve_bound(to, t)?;
                for at in a..=b {
                    self.push_dep(deps, m, mb, Some(at), "window");
                }
            }
            Expr::RangeSum { range, body } => {
                if let Some(did) = self.checked.dim_by_name(range) {
                    for c in 0..self.checked.dims[did].members.len() {
                        let mut a = asg.to_vec();
                        a[did] = c;
                        self.collect(body, &a, t, "sum", deps, arm)?;
                    }
                } else {
                    let r = self
                        .checked
                        .range_of(range)
                        .ok_or_else(|| format!("unknown period range '{range}'"))?
                        .clone();
                    for p in r.start..=r.end {
                        self.collect(body, asg, p, "sum", deps, arm)?;
                    }
                }
            }
            Expr::Npv { rate, body, range } => {
                self.collect(rate, asg, t, "rate", deps, arm)?;
                let r = self
                    .checked
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?
                    .clone();
                for p in r.start..=r.end {
                    self.collect(body, asg, p, "npv", deps, arm)?;
                }
            }
            Expr::Irr { name, .. } => {
                let m = self.checked.index[name];
                let mb = self.checked.tuple_of(m, asg)?;
                let (r0, r1) = self.checked.measures[m].range;
                for p in r0..=r1 {
                    self.push_dep(deps, m, mb, Some(p as isize), "irr");
                }
            }
            Expr::Annualize(x) => self.collect(x, asg, t, via, deps, arm)?,
            Expr::When { value, pos, range } => {
                let r = self
                    .checked
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?;
                let boundary = match pos {
                    FirstLast::First => r.start,
                    FirstLast::Last => r.end,
                };
                if t == boundary {
                    self.collect(value, asg, t, via, deps, arm)?;
                } else if arm.is_empty() {
                    *arm = format!("when {}({range}): not this period → 0",
                        if matches!(pos, FirstLast::First) { "first" } else { "last" });
                }
            }
            Expr::MatchT(arms) => {
                for (set, e2) in arms {
                    let base = self
                        .checked
                        .range_of(&set.base)
                        .ok_or_else(|| format!("unknown period range '{}'", set.base))?;
                    let excluded = match &set.minus {
                        Some(x) => self
                            .checked
                            .range_of(x)
                            .ok_or_else(|| format!("unknown period range '{x}'"))?
                            .contains(t),
                        None => false,
                    };
                    if base.contains(t) && !excluded {
                        if arm.is_empty() {
                            *arm = match &set.minus {
                                Some(x) => format!("match t → in {} \\ {x}", set.base),
                                None => format!("match t → in {}", set.base),
                            };
                        }
                        return self.collect(e2, asg, t, via, deps, arm);
                    }
                }
            }
            Expr::MatchDim { dim, arms, default } => {
                let did = self
                    .checked
                    .dim_by_name(dim)
                    .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                let c = asg[did];
                if c == UNBOUND {
                    return Err(format!("match on {dim} outside a {dim}-bound context"));
                }
                let mname = self.checked.dims[did].members[c].clone();
                for (arm_member, e2) in arms {
                    if *arm_member == mname {
                        if arm.is_empty() {
                            *arm = format!("match {dim} → {mname}");
                        }
                        return self.collect(e2, asg, t, via, deps, arm);
                    }
                }
                if let Some(def) = default {
                    if arm.is_empty() {
                        *arm = format!("match {dim} → else ({mname})");
                    }
                    return self.collect(def, asg, t, via, deps, arm);
                }
            }
            Expr::AllocShare { total, driver, dim, .. } => {
                self.collect(total, asg, t, via, deps, arm)?;
                // The member's own driver, then the full allocation basis.
                self.collect(driver, asg, t, via, deps, arm)?;
                let did = self
                    .checked
                    .dim_by_name(dim)
                    .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                for c in 0..self.checked.dims[did].members.len() {
                    let mut a = asg.to_vec();
                    a[did] = c;
                    self.collect(driver, &a, t, "sum", deps, arm)?;
                }
            }
            Expr::Conv { body, rate, .. } => {
                self.collect(body, asg, t, via, deps, arm)?;
                if let Some(r) = rate {
                    self.collect(r, asg, t, "rate", deps, arm)?;
                }
            }
            Expr::Neg(x) => self.collect(x, asg, t, via, deps, arm)?,
            Expr::Bin(_, a, b) => {
                self.collect(a, asg, t, via, deps, arm)?;
                self.collect(b, asg, t, via, deps, arm)?;
            }
            Expr::Call(_, args) => {
                for a in args {
                    self.collect(a, asg, t, via, deps, arm)?;
                }
            }
        }
        Ok(())
    }
}
