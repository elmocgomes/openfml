//! Reference evaluator over the (measure × dimension-tuple × period) plan.
//! Contexts are member assignments over all dimensions; reads project the
//! assignment onto the target's dimension set (broadcasting).

use crate::ast::{Body, CmpOp, Expr, FirstLast, Kind};
use crate::check::{Checked, MeasureInfo, SolveFormInfo, Step, UNBOUND};
use crate::units::Unit;
use std::collections::HashMap;

/// [measure][tuple][slot] — slot is the period for series, 0 for scalars.
pub type Values = Vec<Vec<Vec<f64>>>;

#[derive(Clone, Debug)]
pub struct AssertResult {
    pub name: String,
    pub passed: bool,
    pub max_deviation: f64,
    pub first_failure: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EvalResult {
    pub period_labels: Vec<String>,
    pub series: Vec<(String, Vec<f64>)>,
    pub scalars: Vec<(String, f64)>,
    pub asserts: Vec<AssertResult>,
    pub solve_iterations: Vec<(String, Vec<u32>)>,
}

pub(crate) struct Ctx<'a> {
    pub c: &'a Checked,
    pub values: &'a mut Values,
}

impl<'a> Ctx<'a> {
    fn read(&self, m: usize, mb: usize, at: isize) -> Result<f64, String> {
        let mi = &self.c.measures[m];
        if !mi.is_series {
            return Ok(self.values[m][mb][0]);
        }
        if at < 0 || at as usize >= self.c.calendar.len {
            if mi.kind == Some(Kind::Stock) {
                return Err(format!("stock '{}' read outside the calendar", mi.name));
            }
            return Ok(0.0);
        }
        let at = at as usize;
        if at < mi.range.0 || at > mi.range.1 {
            if mi.kind == Some(Kind::Stock) {
                return Err(format!("stock '{}' read outside its range", mi.name));
            }
            return Ok(0.0);
        }
        Ok(self.values[m][mb][at])
    }

    pub fn eval(&self, e: &Expr, asg: &[usize], t: usize) -> Result<f64, String> {
        use crate::ast::BinOp::*;
        Ok(match e {
            Expr::Num(v) | Expr::Qty(v, _) => *v,
            Expr::Pct(v) => *v,
            Expr::YearT => self.c.calendar.year_of(t) as f64,
            Expr::Ref(name) => {
                let m = self.c.index[name];
                let mb = self.c.tuple_of(m, asg)?;
                self.read(m, mb, t as isize)?
            }
            Expr::MemberIx { name, members } => {
                let m = self.c.index[name];
                let mut asgs = vec![asg.to_vec()];
                for mname in members {
                    if let Some(&(dim, idx)) = self.c.member_lookup.get(mname) {
                        for a in asgs.iter_mut() {
                            a[dim] = idx;
                        }
                    } else if let Some(&dim) = self.c.group_lookup.get(mname) {
                        let mut next = Vec::new();
                        for a in &asgs {
                            for idx in 0..self.c.dims[dim].members.len() {
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
                let mut acc = 0.0;
                for a in &asgs {
                    let mb = self.c.tuple_of(m, a)?;
                    acc += self.read(m, mb, t as isize)?;
                }
                acc
            }
            Expr::Prev(name, inline_init) => {
                let m = self.c.index[name];
                let mi = &self.c.measures[m];
                let ts = t as isize - 1;
                if ts >= mi.range.0 as isize {
                    let mb = self.c.tuple_of(m, asg)?;
                    self.values[m][mb][ts as usize]
                } else if let Some(init) = inline_init {
                    self.eval(init, asg, t)?
                } else if let Some(init) = &mi.init {
                    self.eval(init, asg, t)?
                } else {
                    return Err(format!("prev({name}) at its range start without init"));
                }
            }
            Expr::Conv { body, target, rate } => {
                let b = self.eval(body, asg, t)?;
                let bu = self
                    .c
                    .expr_unit(body, asg)?
                    .ok_or("cannot convert a bare literal")?;
                let tu = self
                    .c
                    .unit_reg
                    .get(target)
                    .cloned()
                    .unwrap_or_else(|| Unit::base(target));
                match rate {
                    Some(rate) => {
                        let r = self.eval(rate, asg, t)?;
                        let ru = self
                            .c
                            .expr_unit(rate, asg)?
                            .ok_or("conversion rate needs a unit")?;
                        if bu == tu {
                            b
                        } else if bu.mul(&ru) == tu {
                            b * r
                        } else if bu.div(&ru) == tu {
                            b / r
                        } else {
                            return Err(format!("cannot convert {bu} to {tu} with a rate in {ru}"));
                        }
                    }
                    None => {
                        if bu.same_dimension(&tu) {
                            b * bu.scale / tu.scale
                        } else {
                            return Err(format!(
                                "cannot convert {bu} to {tu} without a rate — they differ in dimension"
                            ));
                        }
                    }
                }
            }
            Expr::At { name, bound } => {
                let m = self.c.index[name];
                let mb = self.c.tuple_of(m, asg)?;
                let at = self.c.resolve_bound(bound, t)?;
                self.read(m, mb, at)?
            }
            Expr::WindowSum { name, from, to } => {
                let m = self.c.index[name];
                let mb = self.c.tuple_of(m, asg)?;
                let a = self.c.resolve_bound(from, t)?;
                let b = self.c.resolve_bound(to, t)?;
                let mut acc = 0.0;
                for at in a..=b {
                    acc += self.read(m, mb, at)?;
                }
                acc
            }
            Expr::RangeSum { range, body } => {
                if let Some(did) = self.c.dim_by_name(range) {
                    let mut acc = 0.0;
                    for c in 0..self.c.dims[did].members.len() {
                        let mut a = asg.to_vec();
                        a[did] = c;
                        acc += self.eval(body, &a, t)?;
                    }
                    return Ok(acc);
                }
                let r = self
                    .c
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?
                    .clone();
                let mut acc = 0.0;
                for p in r.start..=r.end {
                    acc += self.eval(body, asg, p)?;
                }
                acc
            }
            Expr::Npv { rate, body, range } => {
                let r = self
                    .c
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?
                    .clone();
                let rt = self.eval(rate, asg, t)?;
                let mut acc = 0.0;
                for (i, p) in (r.start..=r.end).enumerate() {
                    acc += self.eval(body, asg, p)? / (1.0 + rt).powi(i as i32 + 1);
                }
                acc
            }
            Expr::Irr { name, .. } => {
                let m = self.c.index[name];
                let mb = self.c.tuple_of(m, asg)?;
                let mi = &self.c.measures[m];
                let cf: Vec<f64> = (mi.range.0..=mi.range.1).map(|p| self.values[m][mb][p]).collect();
                irr(&cf)?
            }
            Expr::Annualize(x) => {
                let v = self.eval(x, asg, t)?;
                (1.0 + v).powi(self.c.calendar.periods_per_year() as i32) - 1.0
            }
            Expr::When { value, pos, range } => {
                let r = self
                    .c
                    .range_of(range)
                    .ok_or_else(|| format!("unknown period range '{range}'"))?;
                let boundary = match pos {
                    FirstLast::First => r.start,
                    FirstLast::Last => r.end,
                };
                if t == boundary {
                    self.eval(value, asg, t)?
                } else {
                    0.0
                }
            }
            Expr::MatchT(arms) => {
                for (set, arm) in arms {
                    let base = self
                        .c
                        .range_of(&set.base)
                        .ok_or_else(|| format!("unknown period range '{}'", set.base))?;
                    let excluded = match &set.minus {
                        Some(x) => self
                            .c
                            .range_of(x)
                            .ok_or_else(|| format!("unknown period range '{x}'"))?
                            .contains(t),
                        None => false,
                    };
                    if base.contains(t) && !excluded {
                        return self.eval(arm, asg, t);
                    }
                }
                return Err(format!("match t: no arm covers period {}", self.c.calendar.label(t)));
            }
            Expr::MatchDim { dim, arms, default } => {
                let did = self
                    .c
                    .dim_by_name(dim)
                    .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                let c = asg[did];
                if c == UNBOUND {
                    return Err(format!("match on {dim} outside a {dim}-bound context"));
                }
                let mname = &self.c.dims[did].members[c];
                for (arm_member, arm) in arms {
                    if arm_member == mname {
                        return self.eval(arm, asg, t);
                    }
                }
                match default {
                    Some(def) => return self.eval(def, asg, t),
                    None => return Err(format!("no match arm for member '{mname}'")),
                }
            }
            Expr::Neg(x) => -self.eval(x, asg, t)?,
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.eval(a, asg, t)?, self.eval(b, asg, t)?);
                match op {
                    Add => x + y,
                    Sub => x - y,
                    Mul => x * y,
                    Div => x / y,
                    Pow => x.powf(y),
                }
            }
            Expr::Call(f, args) => {
                let vals: Result<Vec<f64>, String> =
                    args.iter().map(|a| self.eval(a, asg, t)).collect();
                let vals = vals?;
                match f.as_str() {
                    "min" => vals.iter().cloned().fold(f64::INFINITY, f64::min),
                    "max" => vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                    other => return Err(format!("unknown function '{other}'")),
                }
            }
        })
    }

    fn body_expr(&self, m: usize) -> &Expr {
        match &self.c.measures[m].body {
            Body::Expr(e) => e,
            Body::Map(_) | Body::DimMatch { .. } => {
                unreachable!("map/match bodies are inputs, never scheduled")
            }
        }
    }

    fn store(&mut self, m: usize, mb: usize, t: usize, v: f64) {
        let slot = if self.c.measures[m].is_series { t } else { 0 };
        self.values[m][mb][slot] = v;
    }
}

fn irr(cf: &[f64]) -> Result<f64, String> {
    let f = |r: f64| -> f64 { cf.iter().enumerate().map(|(t, c)| c / (1.0 + r).powi(t as i32)).sum() };
    let (mut lo, mut hi) = (-0.9f64, 10.0f64);
    let flo = f(lo);
    if flo.signum() == f(hi).signum() {
        return Err("irr: no sign change in (-90%, 1000%)".into());
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if f(mid).signum() == flo.signum() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok((lo + hi) / 2.0)
}

/// Dimensions an assert must iterate (dims of measures referenced bare).
fn assert_dims(e: &Expr, measures: &[MeasureInfo], index: &HashMap<String, usize>, out: &mut Vec<usize>) {
    match e {
        Expr::Ref(nm) | Expr::Prev(nm, _) => {
            for &d in &measures[index[nm]].dims {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
        Expr::MatchDim { arms, default, .. } => {
            for (_, a) in arms {
                assert_dims(a, measures, index, out);
            }
            if let Some(d) = default {
                assert_dims(d, measures, index, out);
            }
        }
        Expr::Neg(x) | Expr::Annualize(x) => assert_dims(x, measures, index, out),
        Expr::Conv { body, rate, .. } => {
            assert_dims(body, measures, index, out);
            if let Some(r) = rate {
                assert_dims(r, measures, index, out);
            }
        }
        Expr::Bin(_, a, b) => {
            assert_dims(a, measures, index, out);
            assert_dims(b, measures, index, out);
        }
        Expr::Call(_, args) => {
            for a in args {
                assert_dims(a, measures, index, out);
            }
        }
        Expr::When { value, .. } => assert_dims(value, measures, index, out),
        Expr::MatchT(arms) => {
            for (_, a) in arms {
                assert_dims(a, measures, index, out);
            }
        }
        Expr::RangeSum { body, .. } => assert_dims(body, measures, index, out),
        Expr::Npv { rate, body, .. } => {
            assert_dims(rate, measures, index, out);
            assert_dims(body, measures, index, out);
        }
        _ => {}
    }
}

// ---- reusable pieces (shared with live::Session) --------------------------

pub fn new_values(c: &Checked) -> Values {
    (0..c.measures.len())
        .map(|i| {
            let slots = if c.measures[i].is_series { c.calendar.len } else { 1 };
            vec![vec![f64::NAN; slots]; c.tuple_count(i)]
        })
        .collect()
}

pub fn init_inputs(c: &Checked, values: &mut Values) -> Result<(), String> {
    for i in 0..c.measures.len() {
        if !c.measures[i].is_input {
            continue;
        }
        for mb in 0..c.tuple_count(i) {
            let asg = c.asg_of_tuple(i, mb);
            let mi = &c.measures[i];
            // Resolve match-body arms down to a Map or Expr for this member.
            let mut body = mi.body.clone();
            loop {
                match body {
                    Body::DimMatch { ref dim, ref arms, ref default } => {
                        let did = c
                            .dim_by_name(dim)
                            .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                        let mname = c.dims[did].members[asg[did]].clone();
                        let next = arms
                            .iter()
                            .find(|(am, _)| *am == mname)
                            .map(|(_, b)| b.clone())
                            .or_else(|| default.as_deref().cloned());
                        match next {
                            Some(b) => body = b,
                            None => {
                                return Err(format!(
                                    "input '{}': no match arm for member '{mname}'",
                                    mi.name
                                ))
                            }
                        }
                    }
                    _ => break,
                }
            }
            match (body, mi.is_series) {
                (Body::Map(entries), true) => {
                    let mut by_idx: HashMap<usize, f64> = HashMap::new();
                    for (lit, e) in &entries {
                        let idx = c.calendar.index(lit)?;
                        let v = Ctx { c, values }.eval(e, &asg, idx)?;
                        by_idx.insert(idx, v);
                    }
                    for t in mi.range.0..=mi.range.1 {
                        let v = *by_idx.get(&t).ok_or_else(|| {
                            format!("input '{}' has no value for {}", mi.name, c.calendar.label(t))
                        })?;
                        values[i][mb][t] = v;
                    }
                }
                (Body::Expr(e), true) => {
                    for t in mi.range.0..=mi.range.1 {
                        let v = Ctx { c, values }.eval(&e, &asg, t)?;
                        values[i][mb][t] = v;
                    }
                }
                (Body::Expr(e), false) => {
                    let v = Ctx { c, values }.eval(&e, &asg, 0)?;
                    values[i][mb][0] = v;
                }
                (Body::Map(_), false) => unreachable!(),
                (Body::DimMatch { .. }, _) => unreachable!("resolved above"),
            }
        }
    }
    Ok(())
}

pub fn compute_tols(c: &Checked, values: &mut Values) -> Result<Vec<f64>, String> {
    let unbound = vec![UNBOUND; c.dims.len()];
    let mut tols = Vec::new();
    for s in &c.solves {
        let v = Ctx { c, values }.eval(&s.tolerance, &unbound, 0)?;
        tols.push(v);
    }
    Ok(tols)
}

pub fn exec_step(
    c: &Checked,
    values: &mut Values,
    tols: &[f64],
    iterations: &mut [(String, Vec<u32>)],
    step: &Step,
) -> Result<(), String> {
    let unbound = vec![UNBOUND; c.dims.len()];
    let mut ctx = Ctx { c, values };
    match step {
        Step::Eval { m, mb, t } => {
            let asg = c.asg_of_tuple(*m, *mb);
            let v = ctx.eval(ctx.body_expr(*m), &asg, *t)?;
            if !v.is_finite() {
                return Err(format!(
                    "'{}' is not finite at {} — check for division by zero",
                    c.measures[*m].name,
                    c.calendar.label(*t)
                ));
            }
            ctx.store(*m, *mb, *t, v);
        }
        Step::Gs { solve, t, members } => {
            let tol = tols[*solve];
            let max_iter = c.solves[*solve].max_iterations;
            for &m in members {
                let seed = if *t > c.measures[m].range.0 {
                    ctx.values[m][0][*t - 1]
                } else if let Some(init) = &c.measures[m].init {
                    ctx.eval(init, &unbound, *t)?
                } else {
                    0.0
                };
                ctx.store(m, 0, *t, seed);
            }
            let mut converged = false;
            let mut worst = (0usize, 0.0f64, 0.0f64); // measure, |Δ|, last value
            for iter in 1..=max_iter {
                let mut delta = 0.0f64;
                for &m in members {
                    let old = ctx.values[m][0][*t];
                    let new = ctx.eval(ctx.body_expr(m), &unbound, *t)?;
                    if !new.is_finite() {
                        return Err(format!(
                            "solve '{}': '{}' diverged at {} (iteration {})",
                            c.solves[*solve].name,
                            c.measures[m].name,
                            c.calendar.label(*t),
                            iter
                        ));
                    }
                    let d = (new - old).abs();
                    if d >= delta {
                        delta = d;
                        worst = (m, d, new);
                    }
                    ctx.store(m, 0, *t, new);
                }
                if delta < tol {
                    converged = true;
                    iterations[*solve].1.push(iter);
                    break;
                }
            }
            if !converged {
                return Err(format!(
                    "solve '{}' did not converge at {} within {} iterations — worst residual: '{}' still moving by {:.4} (last value {:.4}); the system is likely oscillating: check for negative or near-zero denominators (e.g. a negative share price from negative earnings) or a scale mismatch between new inputs and balance-sheet inits",
                    c.solves[*solve].name,
                    c.calendar.label(*t),
                    max_iter,
                    c.measures[worst.0].name,
                    worst.1,
                    worst.2
                ));
            }
        }
        Step::Tear { solve, relaxes, inner } => {
            let tol = tols[*solve];
            let max_iter = c.solves[*solve].max_iterations;
            let init_exprs: Vec<(usize, Expr)> = match &c.solves[*solve].form {
                SolveFormInfo::Tearing { relaxes } => relaxes.clone(),
                _ => unreachable!(),
            };
            for (m, init) in &init_exprs {
                let v = ctx.eval(init, &unbound, 0)?;
                ctx.store(*m, 0, 0, v);
            }
            let mut converged = false;
            for iter in 1..=max_iter {
                for (m, t) in inner {
                    let v = ctx.eval(ctx.body_expr(*m), &unbound, *t)?;
                    if !v.is_finite() {
                        return Err(format!(
                            "solve '{}': '{}' diverged at {} (iteration {})",
                            c.solves[*solve].name,
                            c.measures[*m].name,
                            c.calendar.label(*t),
                            iter
                        ));
                    }
                    ctx.store(*m, 0, *t, v);
                }
                let mut delta = 0.0f64;
                let mut worst = (0usize, 0.0f64);
                for &m in relaxes {
                    let old = ctx.values[m][0][0];
                    let new = ctx.eval(ctx.body_expr(m), &unbound, 0)?;
                    if !new.is_finite() {
                        return Err(format!(
                            "solve '{}': relaxed '{}' diverged (iteration {})",
                            c.solves[*solve].name, c.measures[m].name, iter
                        ));
                    }
                    let d = (new - old).abs();
                    if d >= delta {
                        delta = d;
                        worst = (m, d);
                    }
                    ctx.store(m, 0, 0, new);
                }
                if delta < tol {
                    converged = true;
                    iterations[*solve].1.push(iter);
                    break;
                }
                if iter == max_iter {
                    return Err(format!(
                        "solve '{}' did not converge within {} iterations — worst residual: '{}' still moving by {:.4}",
                        c.solves[*solve].name, max_iter, c.measures[worst.0].name, worst.1
                    ));
                }
            }
            if !converged {
                return Err(format!(
                    "solve '{}' did not converge within {} iterations",
                    c.solves[*solve].name, max_iter
                ));
            }
        }
    }
    Ok(())
}

pub fn run_asserts(c: &Checked, values: &mut Values) -> Result<Vec<AssertResult>, String> {
    let n = c.calendar.len;
    let mut out = Vec::new();
    for a in &c.asserts {
        let ctx = Ctx { c, values };
        let unbound = vec![UNBOUND; c.dims.len()];
        let tol = match &a.tol {
            Some(e) => ctx.eval(e, &unbound, 0)?,
            None => 1e-6,
        };
        let (from, to) = match &a.over {
            Some(r) => {
                let rr = c.range_of(r).ok_or_else(|| format!("unknown range '{r}' in assert"))?;
                (rr.start, rr.end)
            }
            None => (0, n - 1),
        };
        let mut used = Vec::new();
        assert_dims(&a.lhs, &c.measures, &c.index, &mut used);
        assert_dims(&a.rhs, &c.measures, &c.index, &mut used);
        used.sort_unstable();
        // Enumerate assignments over the used dims.
        let mut asgs = vec![unbound.clone()];
        for &d in &used {
            let mut next = Vec::new();
            for asg in &asgs {
                for cidx in 0..c.dims[d].members.len() {
                    let mut a2 = asg.clone();
                    a2[d] = cidx;
                    next.push(a2);
                }
            }
            asgs = next;
        }
        let mut max_dev = 0.0f64;
        let mut first_fail = None;
        for asg in &asgs {
            for t in from..=to {
                let l = ctx.eval(&a.lhs, asg, t)?;
                let r = ctx.eval(&a.rhs, asg, t)?;
                let dev = match a.op {
                    CmpOp::Eq => (l - r).abs(),
                    CmpOp::Ge => (r - l).max(0.0),
                    CmpOp::Le => (l - r).max(0.0),
                };
                max_dev = max_dev.max(dev);
                if dev > tol + 1e-9 && first_fail.is_none() {
                    let member_tag = used
                        .iter()
                        .filter(|&&d| asg[d] != UNBOUND)
                        .map(|&d| c.dims[d].members[asg[d]].clone())
                        .collect::<Vec<_>>()
                        .join(",");
                    let prefix = if member_tag.is_empty() {
                        String::new()
                    } else {
                        format!("{member_tag}@")
                    };
                    first_fail = Some(format!("{prefix}{}", c.calendar.label(t)));
                }
            }
        }
        out.push(AssertResult {
            name: a.name.clone(),
            passed: first_fail.is_none(),
            max_deviation: max_dev,
            first_failure: first_fail,
        });
    }
    Ok(out)
}

pub fn collect_result(
    c: &Checked,
    values: &Values,
    asserts: Vec<AssertResult>,
    iterations: Vec<(String, Vec<u32>)>,
) -> EvalResult {
    let n = c.calendar.len;
    let mut series = Vec::new();
    let mut scalars = Vec::new();
    for (i, mi) in c.measures.iter().enumerate() {
        for mb in 0..c.tuple_count(i) {
            let label = c.tuple_label(i, mb);
            let display = if label.is_empty() {
                mi.name.clone()
            } else {
                format!("{}[{}]", mi.name, label)
            };
            if mi.is_series {
                series.push((display, values[i][mb].clone()));
            } else {
                scalars.push((display, values[i][mb][0]));
            }
        }
    }
    EvalResult {
        period_labels: (0..n).map(|t| c.calendar.label(t)).collect(),
        series,
        scalars,
        asserts,
        solve_iterations: iterations,
    }
}

pub fn evaluate(c: &Checked) -> Result<EvalResult, String> {
    let mut values = new_values(c);
    init_inputs(c, &mut values)?;
    let tols = compute_tols(c, &mut values)?;
    let mut iterations: Vec<(String, Vec<u32>)> =
        c.solves.iter().map(|s| (s.name.clone(), Vec::new())).collect();
    for step in &c.steps {
        exec_step(c, &mut values, &tols, &mut iterations, step)?;
    }
    let asserts = run_asserts(c, &mut values)?;
    Ok(collect_result(c, &values, asserts, iterations))
}
