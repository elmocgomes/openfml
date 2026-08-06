//! Semantic analysis. Evaluation order is topological over the
//! (measure × dimension-tuple × period) micro-graph. Contexts are member
//! *assignments* over all declared dimensions: reading a measure projects
//! the assignment onto its dimension set (broadcasting); reading a measure
//! whose dimension is unbound in context is a compile error ("aggregate or
//! index"). `sum[Dim](…)` aggregates a dimension; `x[Member]` pins one
//! coordinate; `x[Group]` rolls up a tree dimension. Units are Kennedy-style
//! abelian groups with scale; `local` units resolve through the functional
//! dimension's currency map, per member.

use crate::ast::*;
use crate::calendar::{Calendar, Grain, PeriodRange};
use crate::units::Unit;
use std::collections::HashMap;

pub const UNBOUND: usize = usize::MAX;

#[derive(Clone, Debug, PartialEq)]
pub enum MUnit {
    Uniform(Unit),
    /// Member-dependent: resolves through the functional dimension.
    Local,
}

#[derive(Clone, Debug)]
pub struct DimInfo {
    pub name: String,
    /// Roll-up member name for tree dimensions; None for list dimensions.
    pub group: Option<String>,
    pub members: Vec<String>,
    /// Functional currency per member; non-empty only for the functional dim.
    pub currencies: Vec<Unit>,
}

/// A fitted distribution for a stochastic input. Deterministic evaluation
/// uses the median (Q(0.5)); `simulate` samples via the quantile function
/// (SIPmath posture: deterministic, portable, reproducible).
#[derive(Clone, Debug, PartialEq)]
pub enum Dist {
    /// 3-term Keelin metalog fitted from p10/p50/p90.
    Metalog { a1: f64, a2: f64, a3: f64 },
    Uniform { a: f64, b: f64 },
    Normal { mu: f64, sd: f64 },
}

impl Dist {
    pub fn quantile(&self, u: f64) -> f64 {
        let u = u.clamp(1e-9, 1.0 - 1e-9);
        match self {
            Dist::Metalog { a1, a2, a3 } => {
                let l = (u / (1.0 - u)).ln();
                a1 + a2 * l + a3 * (u - 0.5) * l
            }
            Dist::Uniform { a, b } => a + (b - a) * u,
            Dist::Normal { mu, sd } => mu + sd * inv_norm(u),
        }
    }

    pub fn median(&self) -> f64 {
        self.quantile(0.5)
    }
}

/// Standard normal CDF via Abramowitz–Stegun 7.1.26 (|err| < 1.5e-7) —
/// the forward map of the Gaussian copula used by `correlate`.
pub(crate) fn norm_cdf(z: f64) -> f64 {
    let x = z / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let poly = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    let erf = 1.0 - poly * (-x * x).exp();
    0.5 * (1.0 + if x >= 0.0 { erf } else { -erf })
}

/// Cholesky factor (row-major lower L) of a symmetric matrix; None if the
/// matrix is not positive definite — i.e. the declared correlations are
/// mutually inconsistent.
pub(crate) fn cholesky(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if s <= 1e-12 {
                    return None;
                }
                l[i * n + i] = s.sqrt();
            } else {
                l[i * n + j] = s / l[j * n + j];
            }
        }
    }
    Some(l)
}

/// Connected components of `correlate` pairs over the distribution inputs
/// (each returned group sorted by measure index; singletons included).
pub(crate) fn corr_groups(dist_ms: &[usize], correlations: &[(usize, usize, f64)]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut placed: Vec<usize> = Vec::new();
    for &m in dist_ms {
        if placed.contains(&m) {
            continue;
        }
        let mut comp = vec![m];
        placed.push(m);
        let mut i = 0;
        while i < comp.len() {
            let cur = comp[i];
            for &(a, b, _) in correlations {
                let other = if a == cur { b } else if b == cur { a } else { continue };
                if dist_ms.contains(&other) && !placed.contains(&other) {
                    placed.push(other);
                    comp.push(other);
                }
            }
            i += 1;
        }
        comp.sort_unstable();
        groups.push(comp);
    }
    groups
}

/// Row-major correlation matrix for one group: 1s diagonal, declared rhos.
pub(crate) fn corr_matrix(group: &[usize], correlations: &[(usize, usize, f64)]) -> Vec<f64> {
    let n = group.len();
    let mut mat = vec![0.0; n * n];
    for i in 0..n {
        mat[i * n + i] = 1.0;
    }
    for &(a, b, rho) in correlations {
        if let (Some(i), Some(j)) =
            (group.iter().position(|&m| m == a), group.iter().position(|&m| m == b))
        {
            mat[i * n + j] = rho;
            mat[j * n + i] = rho;
        }
    }
    mat
}

/// Acklam's rational approximation of the inverse normal CDF.
pub(crate) fn inv_norm(p: f64) -> f64 {
    const A: [f64; 6] = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.383577518672690e2, -3.066479806614716e1, 2.506628277459239];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let plow = 0.02425;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - plow {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -inv_norm(1.0 - p)
    }
}

#[derive(Clone, Debug)]
pub struct MeasureInfo {
    pub name: String,
    pub munit: MUnit,
    pub kind: Option<Kind>,
    pub is_series: bool,
    /// Dimension ids this measure ranges over (sorted ascending).
    pub dims: Vec<usize>,
    pub is_input: bool,
    pub range: (usize, usize),
    pub init: Option<Expr>,
    pub body: Body,
    pub dist: Option<Dist>,
    /// `per period`: fresh draw every period during `simulate` (iid
    /// shocks) instead of one draw per trial (parameter uncertainty).
    pub dist_per_period: bool,
    pub solve: Option<usize>,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub enum SolveFormInfo {
    Block { members: Vec<usize> },
    Tearing { relaxes: Vec<(usize, Expr)> },
}

#[derive(Clone, Debug)]
pub struct ScenarioInfo {
    pub name: String,
    /// Parent scenario index (None = Base, the model as written).
    pub parent: Option<usize>,
    /// (input measure idx, override body).
    pub overrides: Vec<(usize, Body)>,
}

#[derive(Clone, Debug)]
pub struct SolveInfo {
    pub name: String,
    pub tolerance: Expr,
    pub max_iterations: u32,
    pub form: SolveFormInfo,
}

#[derive(Clone, Debug)]
pub enum Step {
    /// `mb` is the flattened dimension-tuple index.
    Eval { m: usize, mb: usize, t: usize },
    Gs { solve: usize, t: usize, members: Vec<usize> },
    Tear { solve: usize, relaxes: Vec<usize>, inner: Vec<(usize, usize)> },
}

#[derive(Clone, Debug)]
pub struct Checked {
    pub model_name: String,
    pub unit_reg: HashMap<String, Unit>,
    pub calendar: Calendar,
    pub ranges: Vec<PeriodRange>,
    pub range_index: HashMap<String, usize>,
    pub dims: Vec<DimInfo>,
    /// member name → (dim id, member idx); unique across dimensions.
    pub member_lookup: HashMap<String, (usize, usize)>,
    /// group name → dim id (tree dimensions only).
    pub group_lookup: HashMap<String, usize>,
    /// The dimension carrying functional currencies, if any.
    pub functional_dim: Option<usize>,
    pub measures: Vec<MeasureInfo>,
    pub index: HashMap<String, usize>,
    pub solves: Vec<SolveInfo>,
    pub scenarios: Vec<ScenarioInfo>,
    pub asserts: Vec<AssertDecl>,
    pub steps: Vec<Step>,
    /// Editable literal input sites: (measure, tuple index, period, span,
    /// kind). Single-dimension inputs get per-member sites via `match` arms.
    pub edit_sites: Vec<(usize, usize, Option<usize>, (usize, usize), LitKind)>,
    pub nodes: Vec<(usize, usize, usize)>,
    pub edges: Vec<Vec<usize>>,
    pub node_id: HashMap<(usize, usize, usize), usize>,
    /// Validated `correlate` pairs: (measure a, measure b, rho) with a < b.
    pub correlations: Vec<(usize, usize, f64)>,
}

impl Checked {
    pub fn range_of(&self, name: &str) -> Option<&PeriodRange> {
        self.range_index.get(name).map(|i| &self.ranges[*i])
    }

    pub fn resolve_bound(&self, b: &Bound, t: usize) -> Result<isize, String> {
        Ok(match b {
            Bound::Rel(k) => t as isize + *k as isize,
            Bound::RangeStart(r, k) => {
                let rr = self.range_of(r).ok_or_else(|| format!("unknown period range '{r}'"))?;
                rr.start as isize + *k as isize
            }
            Bound::RangeEnd(r, k) => {
                let rr = self.range_of(r).ok_or_else(|| format!("unknown period range '{r}'"))?;
                rr.end as isize + *k as isize
            }
        })
    }

    pub fn dim_by_name(&self, name: &str) -> Option<usize> {
        self.dims.iter().position(|d| d.name == name)
    }

    pub fn tuple_count(&self, m: usize) -> usize {
        self.measures[m]
            .dims
            .iter()
            .map(|&d| self.dims[d].members.len())
            .product::<usize>()
            .max(1)
    }

    /// Member names of measure `m`'s tuple `mb`, joined for display.
    pub fn tuple_label(&self, m: usize, mb: usize) -> String {
        let mi = &self.measures[m];
        if mi.dims.is_empty() {
            return String::new();
        }
        let mut coords = vec![0usize; mi.dims.len()];
        let mut rest = mb;
        for (k, &d) in mi.dims.iter().enumerate().rev() {
            let len = self.dims[d].members.len();
            coords[k] = rest % len;
            rest /= len;
        }
        mi.dims
            .iter()
            .zip(coords)
            .map(|(&d, c)| self.dims[d].members[c].clone())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Assignment (over all dims) reconstructed from measure `m`'s tuple.
    pub fn asg_of_tuple(&self, m: usize, mb: usize) -> Vec<usize> {
        let mut asg = vec![UNBOUND; self.dims.len()];
        let mi = &self.measures[m];
        let mut rest = mb;
        for &d in mi.dims.iter().rev() {
            let len = self.dims[d].members.len();
            asg[d] = rest % len;
            rest /= len;
        }
        asg
    }

    /// Flattened tuple index of measure `m` under an assignment.
    pub fn tuple_of(&self, m: usize, asg: &[usize]) -> Result<usize, String> {
        let mut idx = 0usize;
        for &d in &self.measures[m].dims {
            let coord = asg.get(d).copied().unwrap_or(UNBOUND);
            if coord == UNBOUND {
                return Err(format!(
                    "'{}' is dimensioned over {} — aggregate with sum[{}](…) or index a member",
                    self.measures[m].name, self.dims[d].name, self.dims[d].name
                ));
            }
            idx = idx * self.dims[d].members.len() + coord;
        }
        Ok(idx)
    }

    pub fn expr_unit(&self, e: &Expr, asg: &[usize]) -> Result<Option<Unit>, String> {
        let known: Vec<MUnit> = self.measures.iter().map(|m| m.munit.clone()).collect();
        let env = UnitEnv {
            known: &known,
            index: &self.index,
            dims: &self.dims,
            member_lookup: &self.member_lookup,
            group_lookup: &self.group_lookup,
            functional_dim: self.functional_dim,
            unit_reg: &self.unit_reg,
        };
        unit_of(e, asg, &env).map_err(|err| err.msg())
    }
}

/// All assignments over the given dims (others unbound).
fn enumerate_asgs(dim_ids: &[usize], dims: &[DimInfo], n_dims: usize) -> Vec<Vec<usize>> {
    let mut out = vec![vec![UNBOUND; n_dims]];
    for &d in dim_ids {
        let mut next = Vec::new();
        for asg in &out {
            for c in 0..dims[d].members.len() {
                let mut a = asg.clone();
                a[d] = c;
                next.push(a);
            }
        }
        out = next;
    }
    out
}

pub fn check(model: &Model) -> Result<Checked, String> {
    // ---- calendar & ranges -------------------------------------------------
    let cd = model.calendar.as_ref().ok_or("model needs a 'calendar' declaration")?;
    let grain = match cd.grain.as_str() {
        "yearly" => Grain::Yearly,
        "quarterly" => Grain::Quarterly,
        "monthly" => Grain::Monthly,
        other => return Err(format!("unsupported calendar grain '{other}' (yearly | quarterly | monthly)")),
    };
    let calendar = Calendar::new(cd.name.clone(), grain, cd.start, cd.end)?;
    let n = calendar.len;

    let mut ranges: Vec<PeriodRange> = Vec::new();
    let mut range_index: HashMap<String, usize> = HashMap::new();
    ranges.push(PeriodRange { name: calendar.name.clone(), start: 0, end: n - 1 });
    range_index.insert(calendar.name.clone(), 0);
    for pd in &model.period_ranges {
        if range_index.contains_key(&pd.name) {
            return Err(format!("period range '{}' declared twice", pd.name));
        }
        let start = calendar.index(&pd.start)?;
        let end = calendar.index(&pd.end)?;
        if end < start {
            return Err(format!("period range '{}' ends before it starts", pd.name));
        }
        range_index.insert(pd.name.clone(), ranges.len());
        ranges.push(PeriodRange { name: pd.name.clone(), start, end });
    }

    // ---- unit registry -----------------------------------------------------
    let mut unit_reg: HashMap<String, Unit> = HashMap::new();
    if let Some(c) = &model.currency {
        unit_reg.insert(c.clone(), Unit::base(c));
    }
    for ud in &model.units {
        let u = match &ud.scaled {
            None => Unit::base(&ud.name),
            Some((factor, base)) => {
                let bu = unit_reg
                    .get(base)
                    .ok_or_else(|| format!("unit '{}' is scaled from undeclared '{}'", ud.name, base))?;
                if *factor <= 0.0 {
                    return Err(format!("unit '{}': scale factor must be positive", ud.name));
                }
                Unit::scaled(bu, *factor)
            }
        };
        if unit_reg.insert(ud.name.clone(), u).is_some() {
            return Err(format!("unit '{}' declared twice", ud.name));
        }
    }

    // ---- dimensions --------------------------------------------------------
    let mut dims: Vec<DimInfo> = Vec::new();
    let mut member_lookup: HashMap<String, (usize, usize)> = HashMap::new();
    let mut group_lookup: HashMap<String, usize> = HashMap::new();
    for d in &model.dimensions {
        if dims.iter().any(|x| x.name == d.name) {
            return Err(format!("dimension '{}' declared twice", d.name));
        }
        let did = dims.len();
        for (i, m) in d.members.iter().enumerate() {
            if member_lookup.insert(m.clone(), (did, i)).is_some() {
                return Err(format!("member '{}' appears in more than one dimension", m));
            }
        }
        if let Some(g) = &d.group {
            if group_lookup.insert(g.clone(), did).is_some() || member_lookup.contains_key(g) {
                return Err(format!("group name '{}' collides with another name", g));
            }
        }
        dims.push(DimInfo {
            name: d.name.clone(),
            group: d.group.clone(),
            members: d.members.clone(),
            currencies: Vec::new(),
        });
    }
    let n_dims = dims.len();

    let mut functional_dim: Option<usize> = None;
    if let Some(f) = &model.functional {
        let did = dims
            .iter()
            .position(|d| d.name == f.dim)
            .ok_or_else(|| format!("functional map names unknown dimension '{}'", f.dim))?;
        let mut currencies = Vec::new();
        for m in &dims[did].members.clone() {
            let ccy = f
                .map
                .iter()
                .find(|(mm, _)| mm == m)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| format!("functional map is missing member '{m}'"))?;
            let u = unit_reg
                .get(&ccy)
                .cloned()
                .ok_or_else(|| format!("unknown currency/unit '{ccy}' in functional map"))?;
            currencies.push(u);
        }
        dims[did].currencies = currencies;
        functional_dim = Some(did);
    }

    // ---- collect measures --------------------------------------------------
    let mut measures: Vec<MeasureInfo> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut solves: Vec<SolveInfo> = Vec::new();
    let mut pending_tearing: Vec<(usize, Vec<RelaxDecl>)> = Vec::new();
    let mut decls: Vec<MeasureDecl> = Vec::new();

    {
        let mut push = |m: &MeasureDecl, solve: Option<usize>| -> Result<usize, String> {
            if index.contains_key(&m.name) {
                return Err(format!(
                    "line {}: '{}' is defined twice — one definition per measure",
                    m.line, m.name
                ));
            }
            let idx = measures.len();
            index.insert(m.name.clone(), idx);
            measures.push(MeasureInfo {
                name: m.name.clone(),
                munit: MUnit::Uniform(Unit::one()),
                kind: m.ann.kind,
                is_series: false,
                dims: Vec::new(),
                is_input: m.is_input,
                range: (0, n - 1),
                init: m.init.as_ref().map(|(_, e)| e.clone()),
                body: m.body.clone(),
                dist: None, // fitted below (needs the unit checker's helpers)
                dist_per_period: false,
                solve,
                line: m.line,
            });
            decls.push(m.clone());
            Ok(idx)
        };
        for item in &model.items {
            match item {
                Item::Measure(m) => {
                    push(m, None)?;
                }
                Item::Solve(s) => {
                    let solve_idx = solves.len();
                    match &s.form {
                        SolveForm::Block(members) => {
                            let mut idxs = Vec::new();
                            for m in members {
                                idxs.push(push(m, Some(solve_idx))?);
                            }
                            solves.push(SolveInfo {
                                name: s.name.clone(),
                                tolerance: s.tolerance.clone().unwrap_or(Expr::Num(1e-6)),
                                max_iterations: s.max_iterations,
                                form: SolveFormInfo::Block { members: idxs },
                            });
                        }
                        SolveForm::Tearing(relaxes) => {
                            solves.push(SolveInfo {
                                name: s.name.clone(),
                                tolerance: s.tolerance.clone().unwrap_or(Expr::Num(1e-6)),
                                max_iterations: s.max_iterations,
                                form: SolveFormInfo::Tearing { relaxes: Vec::new() },
                            });
                            pending_tearing.push((solve_idx, relaxes.clone()));
                        }
                    }
                }
                Item::Assert(_) => {}
            }
        }
    }
    for (solve_idx, relaxes) in pending_tearing {
        let mut resolved = Vec::new();
        for r in relaxes {
            let mi = *index
                .get(&r.name)
                .ok_or_else(|| format!("solve '{}' relaxes unknown measure '{}'", solves[solve_idx].name, r.name))?;
            resolved.push((mi, r.init.clone()));
        }
        solves[solve_idx].form = SolveFormInfo::Tearing { relaxes: resolved };
    }

    // ---- resolve names -----------------------------------------------------
    fn body_names_rec(body: &Body, out: &mut Vec<String>) {
        match body {
            Body::Expr(e) => all_names(e, out),
            Body::Map(entries) => {
                for (_, e) in entries {
                    all_names(e, out);
                }
            }
            Body::DimMatch { arms, default, .. } => {
                for (_, b) in arms {
                    body_names_rec(b, out);
                }
                if let Some(d) = default {
                    body_names_rec(d, out);
                }
            }
        }
    }
    let body_names = |body: &Body| -> Vec<String> {
        let mut out = Vec::new();
        body_names_rec(body, &mut out);
        out
    };
    for mi in &measures {
        for nref in body_names(&mi.body) {
            if !index.contains_key(&nref) {
                return Err(format!("line {}: '{}' references unknown measure '{}'", mi.line, mi.name, nref));
            }
        }
        if let Some(init) = &mi.init {
            let mut names = Vec::new();
            all_names(init, &mut names);
            for nref in names {
                if !index.contains_key(&nref) {
                    return Err(format!("line {}: init of '{}' references unknown measure '{}'", mi.line, mi.name, nref));
                }
            }
        }
    }
    for a in model.asserts() {
        let mut names = Vec::new();
        all_names(&a.lhs, &mut names);
        all_names(&a.rhs, &mut names);
        if let Some(t) = &a.tol {
            all_names(t, &mut names);
        }
        for nref in names {
            if !index.contains_key(&nref) {
                return Err(format!("line {}: assert '{}' references unknown measure '{}'", a.line, a.name, nref));
            }
        }
        if let Some(over) = &a.over {
            if !range_index.contains_key(over) {
                return Err(format!("line {}: assert '{}': unknown range '{}'", a.line, a.name, over));
            }
        }
    }

    // ---- over resolution ---------------------------------------------------
    for (i, d) in decls.iter().enumerate() {
        for name in &d.over {
            if let Some(did) = dims.iter().position(|x| x.name == *name) {
                if !measures[i].dims.contains(&did) {
                    measures[i].dims.push(did);
                }
                continue;
            }
            if let Some(ri) = range_index.get(name) {
                measures[i].is_series = true;
                measures[i].range = (ranges[*ri].start, ranges[*ri].end);
                continue;
            }
            return Err(format!(
                "line {}: '{}': unknown dimension/calendar/range '{}' in 'over'",
                d.line, d.name, name
            ));
        }
        measures[i].dims.sort_unstable();
        let has_map = match &d.body {
            Body::Map(_) => true,
            Body::DimMatch { arms, default, .. } => {
                arms.iter().any(|(_, b)| matches!(b, Body::Map(_)))
                    || default.as_ref().map(|b| matches!(**b, Body::Map(_))).unwrap_or(false)
            }
            Body::Expr(_) => false,
        };
        if has_map && !measures[i].is_series {
            return Err(format!(
                "line {}: input '{}' has a per-period map but no calendar in 'over'",
                d.line, d.name
            ));
        }
        if let Body::DimMatch { dim, .. } = &d.body {
            let did = dims
                .iter()
                .position(|x| x.name == *dim)
                .ok_or_else(|| format!("line {}: '{}': unknown dimension '{}' in match body", d.line, d.name, dim))?;
            if !measures[i].dims.contains(&did) {
                return Err(format!(
                    "line {}: '{}' matches on {} but is not declared over it",
                    d.line, d.name, dim
                ));
            }
        }
    }

    // ---- series inference --------------------------------------------------
    let range_names: Vec<String> = ranges.iter().map(|r| r.name.clone()).collect();
    fn t_dep(
        e: &Expr,
        series: &[bool],
        index: &HashMap<String, usize>,
        range_names: &[String],
    ) -> bool {
        match e {
            Expr::Ref(nm) => series[index[nm]],
            Expr::MemberIx { name, .. } => series[index[name]],
            Expr::Prev(_, _) | Expr::YearT | Expr::When { .. } | Expr::MatchT(_) => true,
            Expr::WindowSum { from, to, .. } => {
                matches!(from, Bound::Rel(_)) || matches!(to, Bound::Rel(_))
            }
            Expr::At { bound, .. } => matches!(bound, Bound::Rel(_)),
            Expr::Irr { .. } => false,
            Expr::RangeSum { range, body } => {
                if range_names.iter().any(|r| r == range) {
                    false // period aggregation absorbs t
                } else {
                    t_dep(body, series, index, range_names) // dimension sum
                }
            }
            Expr::Npv { rate, .. } => t_dep(rate, series, index, range_names),
            Expr::Conv { body, rate, .. } => {
                t_dep(body, series, index, range_names)
                    || rate
                        .as_ref()
                        .map(|r| t_dep(r, series, index, range_names))
                        .unwrap_or(false)
            }
            Expr::MatchDim { arms, default, .. } => {
                arms.iter().any(|(_, a)| t_dep(a, series, index, range_names))
                    || default
                        .as_ref()
                        .map(|d| t_dep(d, series, index, range_names))
                        .unwrap_or(false)
            }
            Expr::Neg(x) | Expr::Annualize(x) => t_dep(x, series, index, range_names),
            Expr::Bin(_, a, b) => {
                t_dep(a, series, index, range_names) || t_dep(b, series, index, range_names)
            }
            Expr::Call(_, args) => args.iter().any(|a| t_dep(a, series, index, range_names)),
            Expr::Num(_) | Expr::Qty(_, _) | Expr::Pct(_) => false,
        }
    }
    loop {
        let flags: Vec<bool> = measures.iter().map(|m| m.is_series).collect();
        let mut changed = false;
        for i in 0..measures.len() {
            if measures[i].is_series {
                continue;
            }
            fn body_tdep(
                b: &Body,
                flags: &[bool],
                index: &HashMap<String, usize>,
                range_names: &[String],
            ) -> bool {
                match b {
                    Body::Expr(e) => t_dep(e, flags, index, range_names),
                    Body::Map(_) => true,
                    Body::DimMatch { arms, default, .. } => {
                        arms.iter().any(|(_, a)| body_tdep(a, flags, index, range_names))
                            || default
                                .as_ref()
                                .map(|d| body_tdep(d, flags, index, range_names))
                                .unwrap_or(false)
                    }
                }
            }
            let dep = body_tdep(&measures[i].body, &flags, &index, &range_names);
            if dep {
                measures[i].is_series = true;
                measures[i].range = (0, n - 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for mi in &measures {
        if !mi.is_series && mi.kind.is_some() {
            return Err(format!("line {}: '{}' has a stock/flow kind but is a scalar", mi.line, mi.name));
        }
    }

    // ---- labelled inits ----------------------------------------------------
    for (i, d) in decls.iter().enumerate() {
        if let Some((Some(label), _)) = &d.init {
            let got = calendar
                .index_or_prev(label)
                .map_err(|e| format!("line {}: init label of '{}': {}", d.line, d.name, e))?;
            if got != measures[i].range.0 as isize - 1 {
                return Err(format!(
                    "line {}: init label of '{}' must name the period before its range start",
                    d.line, d.name
                ));
            }
        }
    }

    // ---- units -------------------------------------------------------------
    let resolve_unit_name = |name: &str| -> Result<Unit, String> {
        if name == "1" || name == "rate" || name == "ratio" {
            Ok(Unit::one())
        } else {
            unit_reg
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown unit '{name}'"))
        }
    };
    let mut known: Vec<Option<MUnit>> = vec![None; measures.len()];
    for (i, d) in decls.iter().enumerate() {
        if let Some(ua) = &d.ann.unit {
            if ua.num == "local" {
                if ua.den.is_some() {
                    return Err(format!("line {}: 'local' cannot take a denominator", d.line));
                }
                let fd = functional_dim.ok_or_else(|| {
                    format!("line {}: 'local' unit needs a 'functional' map", d.line)
                })?;
                if !measures[i].dims.contains(&fd) {
                    return Err(format!(
                        "line {}: '{}' is typed 'local' but is not over the functional dimension",
                        d.line, d.name
                    ));
                }
                known[i] = Some(MUnit::Local);
            } else {
                let unum = resolve_unit_name(&ua.num).map_err(|e| format!("line {}: {}", d.line, e))?;
                let u = match &ua.den {
                    Some(den) => unum.div(&resolve_unit_name(den).map_err(|e| format!("line {}: {}", d.line, e))?),
                    None => unum,
                };
                known[i] = Some(MUnit::Uniform(u));
            }
        }
    }
    for (i, mi) in measures.iter().enumerate() {
        if mi.is_input && known[i].is_none() {
            return Err(format!("line {}: input '{}' needs a unit annotation", mi.line, mi.name));
        }
        if !mi.dims.is_empty() && known[i].is_none() {
            return Err(format!(
                "line {}: dimensioned measure '{}' needs a unit annotation",
                mi.line, mi.name
            ));
        }
    }
    let unbound_asg = vec![UNBOUND; n_dims];
    loop {
        let known_m: Vec<MUnit> = known
            .iter()
            .map(|k| k.clone().unwrap_or(MUnit::Uniform(Unit::one())))
            .collect();
        let resolved: Vec<bool> = known.iter().map(|k| k.is_some()).collect();
        let env = UnitEnv {
            known: &known_m,
            index: &index,
            dims: &dims,
            member_lookup: &member_lookup,
            group_lookup: &group_lookup,
            functional_dim,
            unit_reg: &unit_reg,
        };
        let mut progressed = false;
        let mut deferred = Vec::new();
        for i in 0..measures.len() {
            if known[i].is_some() {
                continue;
            }
            let e = match &measures[i].body {
                Body::Expr(e) => e.clone(),
                Body::Map(_) | Body::DimMatch { .. } => unreachable!("inputs are annotated"),
            };
            let mut names = Vec::new();
            all_names(&e, &mut names);
            if names.iter().any(|nm| !resolved[index[nm.as_str()]]) {
                deferred.push(measures[i].name.clone());
                continue;
            }
            match unit_of(&e, &unbound_asg, &env) {
                Ok(u) => {
                    known[i] = Some(MUnit::Uniform(u.unwrap_or_else(Unit::one)));
                    progressed = true;
                }
                Err(err) => {
                    return Err(format!("line {}: in '{}': {}", measures[i].line, measures[i].name, err.msg()))
                }
            }
        }
        if deferred.is_empty() {
            break;
        }
        if !progressed {
            deferred.sort();
            deferred.dedup();
            return Err(format!("cannot infer units for {} — add annotations", deferred.join(", ")));
        }
    }
    for (i, u) in known.iter().enumerate() {
        measures[i].munit = u.clone().unwrap();
    }

    // ---- distributions -----------------------------------------------------
    fn const_eval(e: &Expr) -> Result<f64, String> {
        Ok(match e {
            Expr::Num(v) | Expr::Qty(v, _) => *v,
            Expr::Pct(v) => *v,
            Expr::Neg(x) => -const_eval(x)?,
            Expr::Bin(op, a, b) => {
                let (x, y) = (const_eval(a)?, const_eval(b)?);
                match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div => x / y,
                    BinOp::Pow => x.powf(y),
                }
            }
            _ => return Err("distribution parameters must be constants".into()),
        })
    }
    for (i, d) in decls.iter().enumerate() {
        let Some(dd) = &d.dist else { continue };
        let get_keyed = |k: &str| -> Result<f64, String> {
            dd.params
                .iter()
                .find(|(key, _)| key.as_deref() == Some(k))
                .map(|(_, e)| const_eval(e))
                .transpose()?
                .ok_or_else(|| format!("line {}: metalog needs '{k}'", d.line))
        };
        let dist = match dd.kind.as_str() {
            "metalog" => {
                let (q10, q50, q90) = (get_keyed("p10")?, get_keyed("p50")?, get_keyed("p90")?);
                if !(q10 <= q50 && q50 <= q90) {
                    return Err(format!(
                        "line {}: metalog quantiles must satisfy p10 <= p50 <= p90",
                        d.line
                    ));
                }
                // 3-term closed-form fit (Keelin 2016).
                let l9 = (0.9f64 / 0.1).ln();
                Dist::Metalog {
                    a1: q50,
                    a2: (q90 - q10) / (2.0 * l9),
                    a3: (q90 + q10 - 2.0 * q50) / (0.8 * l9),
                }
            }
            "uniform" => {
                let (a, b) = (const_eval(&dd.params[0].1)?, const_eval(&dd.params[1].1)?);
                if b < a {
                    return Err(format!("line {}: uniform(a, b) needs a <= b", d.line));
                }
                Dist::Uniform { a, b }
            }
            "normal" => {
                let (mu, sd) = (const_eval(&dd.params[0].1)?, const_eval(&dd.params[1].1)?);
                if sd < 0.0 {
                    return Err(format!("line {}: normal(mean, sd) needs sd >= 0", d.line));
                }
                Dist::Normal { mu, sd }
            }
            _ => unreachable!("parser filters kinds"),
        };
        // Deterministic-by-default: the body becomes the median literal.
        let median = dist.median();
        measures[i].body = Body::Expr(Expr::Num(median));
        measures[i].dist = Some(dist);
        measures[i].dist_per_period = dd.per_period;
    }

    // ---- unit checking -----------------------------------------------------
    let munits: Vec<MUnit> = measures.iter().map(|m| m.munit.clone()).collect();
    let env = UnitEnv {
        known: &munits,
        index: &index,
        dims: &dims,
        member_lookup: &member_lookup,
        group_lookup: &group_lookup,
        functional_dim,
        unit_reg: &unit_reg,
    };
    let expected_in = |i: usize, asg: &[usize]| -> Unit {
        match &measures[i].munit {
            MUnit::Uniform(u) => u.clone(),
            MUnit::Local => {
                let fd = functional_dim.unwrap();
                dims[fd].currencies[asg[fd]].clone()
            }
        }
    };
    let check_expr = |e: &Expr, i: usize, asg: &[usize]| -> Result<(), String> {
        let expected = expected_in(i, asg);
        match unit_of(e, asg, &env) {
            Ok(Some(u)) => {
                if u != expected {
                    let ctx = measures[i]
                        .dims
                        .iter()
                        .filter(|&&d| asg[d] != UNBOUND)
                        .map(|&d| dims[d].members[asg[d]].clone())
                        .collect::<Vec<_>>()
                        .join(",");
                    Err(format!(
                        "line {}: '{}' is declared {} but its definition has unit {}{}",
                        measures[i].line,
                        measures[i].name,
                        expected,
                        u,
                        if ctx.is_empty() { String::new() } else { format!(" (member {ctx})") }
                    ))
                } else {
                    Ok(())
                }
            }
            Ok(None) => Ok(()),
            Err(err) => Err(format!("line {}: in '{}': {}", measures[i].line, measures[i].name, err.msg())),
        }
    };
    fn select_arm<'b>(
        body: &'b Body,
        asg: &[usize],
        dims: &[DimInfo],
    ) -> Result<&'b Body, String> {
        match body {
            Body::DimMatch { dim, arms, default } => {
                let did = dims
                    .iter()
                    .position(|d| d.name == *dim)
                    .ok_or_else(|| format!("unknown dimension '{dim}'"))?;
                let c = asg[did];
                if c == UNBOUND {
                    return Err(format!("match on {dim} outside a bound context"));
                }
                let mname = &dims[did].members[c];
                for (am, b) in arms {
                    if am == mname {
                        return select_arm(b, asg, dims);
                    }
                }
                match default {
                    Some(d) => select_arm(d, asg, dims),
                    None => Err(format!("no match arm for member '{mname}' (add an 'else')")),
                }
            }
            other => Ok(other),
        }
    }
    for i in 0..measures.len() {
        let mdims = measures[i].dims.clone();
        for asg in enumerate_asgs(&mdims, &dims, n_dims) {
            let body = measures[i].body.clone();
            let arm = select_arm(&body, &asg, &dims)
                .map_err(|e| format!("line {}: in '{}': {}", measures[i].line, measures[i].name, e))?;
            match arm {
                Body::Expr(e) => check_expr(e, i, &asg)?,
                Body::Map(entries) => {
                    for (_, e) in entries {
                        check_expr(e, i, &asg)?;
                    }
                }
                Body::DimMatch { .. } => unreachable!("select_arm resolves nested matches"),
            }
            if let Some(init) = &measures[i].init.clone() {
                check_expr(init, i, &asg)?;
            }
        }
    }
    for s in &solves {
        if let SolveFormInfo::Tearing { relaxes } = &s.form {
            for (mi, init) in relaxes {
                if measures[*mi].is_series || !measures[*mi].dims.is_empty() {
                    return Err(format!(
                        "solve '{}': relaxed variable '{}' must be an undimensioned scalar in Phase 1",
                        s.name, measures[*mi].name
                    ));
                }
                if measures[*mi].is_input {
                    return Err(format!("solve '{}': cannot relax input '{}'", s.name, measures[*mi].name));
                }
                check_expr(init, *mi, &unbound_asg)?;
            }
        }
        if let SolveFormInfo::Block { members } = &s.form {
            for mi in members {
                if !measures[*mi].dims.is_empty() {
                    return Err(format!(
                        "solve '{}': dimensioned measures inside solves are not supported in Phase 1 ('{}')",
                        s.name, measures[*mi].name
                    ));
                }
            }
        }
    }
    // Asserts: contexts from the dims used by bare refs.
    fn bare_dims(e: &Expr, measures: &[MeasureInfo], index: &HashMap<String, usize>, out: &mut Vec<usize>) {
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
                    bare_dims(a, measures, index, out);
                }
                if let Some(d) = default {
                    bare_dims(d, measures, index, out);
                }
            }
            Expr::Neg(x) | Expr::Annualize(x) => bare_dims(x, measures, index, out),
            Expr::Conv { body, rate, .. } => {
                bare_dims(body, measures, index, out);
                if let Some(r) = rate {
                    bare_dims(r, measures, index, out);
                }
            }
            Expr::Bin(_, a, b) => {
                bare_dims(a, measures, index, out);
                bare_dims(b, measures, index, out);
            }
            Expr::Call(_, args) => {
                for a in args {
                    bare_dims(a, measures, index, out);
                }
            }
            Expr::When { value, .. } => bare_dims(value, measures, index, out),
            Expr::MatchT(arms) => {
                for (_, a) in arms {
                    bare_dims(a, measures, index, out);
                }
            }
            Expr::RangeSum { body, .. } => bare_dims(body, measures, index, out),
            Expr::Npv { rate, body, .. } => {
                bare_dims(rate, measures, index, out);
                bare_dims(body, measures, index, out);
            }
            _ => {}
        }
    }
    for a in model.asserts() {
        let mut used = Vec::new();
        bare_dims(&a.lhs, &measures, &index, &mut used);
        bare_dims(&a.rhs, &measures, &index, &mut used);
        used.sort_unstable();
        for asg in enumerate_asgs(&used, &dims, n_dims) {
            let lu = unit_of(&a.lhs, &asg, &env).map_err(|e| format!("line {}: assert '{}': {}", a.line, a.name, e.msg()))?;
            let ru = unit_of(&a.rhs, &asg, &env).map_err(|e| format!("line {}: assert '{}': {}", a.line, a.name, e.msg()))?;
            if let (Some(l), Some(r)) = (&lu, &ru) {
                if l != r {
                    return Err(format!("line {}: assert '{}' compares {} with {}", a.line, a.name, l, r));
                }
            }
            if let Some(t) = &a.tol {
                let tu = unit_of(t, &asg, &env).map_err(|e| format!("line {}: assert '{}': {}", a.line, a.name, e.msg()))?;
                if let (Some(t), Some(s)) = (tu, lu.or(ru)) {
                    if t != s {
                        return Err(format!(
                            "line {}: assert '{}' tolerance unit {} does not match compared unit {}",
                            a.line, a.name, t, s
                        ));
                    }
                }
            }
        }
    }

    // ---- input discipline --------------------------------------------------
    for mi in &measures {
        if !mi.is_input {
            continue;
        }
        for nref in body_names(&mi.body) {
            if !measures[index[&nref]].is_input {
                return Err(format!(
                    "line {}: input '{}' may only reference other inputs (references '{}')",
                    mi.line, mi.name, nref
                ));
            }
        }
    }

    // ---- micro-graph over (measure, tuple, period) -------------------------
    let tuple_count_of = |m: usize| -> usize {
        measures[m]
            .dims
            .iter()
            .map(|&d| dims[d].members.len())
            .product::<usize>()
            .max(1)
    };
    let pre = Pre {
        n,
        measures: &measures,
        index: &index,
        ranges: &ranges,
        range_index: &range_index,
        dims: &dims,
        member_lookup: &member_lookup,
        group_lookup: &group_lookup,
    };
    let mut nodes: Vec<(usize, usize, usize)> = Vec::new();
    let mut node_id: HashMap<(usize, usize, usize), usize> = HashMap::new();
    for (i, mi) in measures.iter().enumerate() {
        for mb in 0..tuple_count_of(i) {
            if mi.is_series {
                for t in mi.range.0..=mi.range.1 {
                    node_id.insert((i, mb, t), nodes.len());
                    nodes.push((i, mb, t));
                }
            } else {
                node_id.insert((i, mb, 0), nodes.len());
                nodes.push((i, mb, 0));
            }
        }
    }
    let asg_of = |m: usize, mb: usize| -> Vec<usize> {
        let mut asg = vec![UNBOUND; n_dims];
        let mut rest = mb;
        for &d in measures[m].dims.iter().rev() {
            let len = dims[d].members.len();
            asg[d] = rest % len;
            rest /= len;
        }
        asg
    };
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (nid, (m, mb, t)) in nodes.iter().enumerate() {
        let mut deps = Vec::new();
        let asg = asg_of(*m, *mb);
        {
            let mut body_ref = &measures[*m].body;
            loop {
                match body_ref {
                    Body::Expr(e) => {
                        pre.walk(e, *m, &asg, *t, &mut deps)?;
                        break;
                    }
                    Body::Map(_) => break, // map literals: init-time values only
                    Body::DimMatch { dim, arms, default } => {
                        let did = dims.iter().position(|d| d.name == *dim).unwrap();
                        let mname = &dims[did].members[asg[did]];
                        let mut next: Option<&Body> = None;
                        for (am, b) in arms {
                            if am == mname {
                                next = Some(b);
                                break;
                            }
                        }
                        if next.is_none() {
                            next = default.as_deref();
                        }
                        match next {
                            Some(b) => body_ref = b,
                            None => break,
                        }
                    }
                }
            }
        }
        for (dm, dmb, dt) in deps {
            let key = if measures[dm].is_series { (dm, dmb, dt) } else { (dm, dmb, 0) };
            if let Some(&dep_id) = node_id.get(&key) {
                if !edges[dep_id].contains(&nid) {
                    edges[dep_id].push(nid);
                }
            }
        }
    }

    // ---- SCC condensation → execution plan --------------------------------
    let sccs = tarjan(nodes.len(), &edges);
    let mut steps: Vec<Step> = Vec::new();
    for comp in sccs.into_iter().rev() {
        if comp.len() == 1 {
            let nid = comp[0];
            let has_self = edges[nid].contains(&nid);
            let (m, mb, t) = nodes[nid];
            if measures[m].is_input {
                continue;
            }
            if !has_self {
                steps.push(Step::Eval { m, mb, t });
                continue;
            }
            if let Some(k) = measures[m].solve {
                steps.push(Step::Gs { solve: k, t, members: vec![m] });
                continue;
            }
            return Err(format!(
                "line {}: '{}' references itself in the same period — wrap it in a solve block or use prev()",
                measures[m].line, measures[m].name
            ));
        }
        let ms: Vec<(usize, usize, usize)> = comp.iter().map(|&nid| nodes[nid]).collect();
        let same_t = ms.iter().all(|(_, _, t)| *t == ms[0].2);
        let blocks: Vec<Option<usize>> = ms.iter().map(|(m, _, _)| measures[*m].solve).collect();
        if same_t && blocks.iter().all(|b| b.is_some() && *b == blocks[0]) {
            let k = blocks[0].unwrap();
            let mut members: Vec<usize> = match &solves[k].form {
                SolveFormInfo::Block { members } => members
                    .iter()
                    .cloned()
                    .filter(|mi| ms.iter().any(|(m, _, _)| m == mi))
                    .collect(),
                _ => unreachable!(),
            };
            members.dedup();
            steps.push(Step::Gs { solve: k, t: ms[0].2, members });
            continue;
        }
        let mut chosen: Option<usize> = None;
        for (k, s) in solves.iter().enumerate() {
            if let SolveFormInfo::Tearing { relaxes } = &s.form {
                if relaxes.iter().any(|(rm, _)| ms.iter().any(|(m, _, _)| m == rm)) {
                    chosen = Some(k);
                    break;
                }
            }
        }
        let Some(k) = chosen else {
            let mut names: Vec<String> = ms
                .iter()
                .map(|(m, _, t)| format!("{}@{}", measures[*m].name, calendar.label(*t)))
                .collect();
            names.sort();
            names.dedup();
            names.truncate(8);
            return Err(format!(
                "circular reference not covered by any solve (involving: {}) — wrap it in a solve block or add 'solve {{ relax ... }}'",
                names.join(", ")
            ));
        };
        let relaxes: Vec<usize> = match &solves[k].form {
            SolveFormInfo::Tearing { relaxes } => relaxes.iter().map(|(m, _)| *m).collect(),
            _ => unreachable!(),
        };
        for rm in &relaxes {
            if !ms.iter().any(|(m, _, _)| m == rm) {
                return Err(format!(
                    "solve '{}': relaxed variable '{}' is not part of the circular component it must cut",
                    solves[k].name, measures[*rm].name
                ));
            }
        }
        let comp_set: HashMap<usize, ()> = comp.iter().map(|&c| (c, ())).collect();
        let keep: Vec<usize> = comp
            .iter()
            .cloned()
            .filter(|&nid| !relaxes.contains(&nodes[nid].0))
            .collect();
        let keep_pos: HashMap<usize, usize> = keep.iter().enumerate().map(|(p, &nid)| (nid, p)).collect();
        let mut indeg = vec![0usize; keep.len()];
        let mut sub_edges: Vec<Vec<usize>> = vec![Vec::new(); keep.len()];
        for &nid in &keep {
            for &to in &edges[nid] {
                if comp_set.contains_key(&to) {
                    if let (Some(&a), Some(&b)) = (keep_pos.get(&nid), keep_pos.get(&to)) {
                        sub_edges[a].push(b);
                        indeg[b] += 1;
                    }
                }
            }
        }
        let mut queue: Vec<usize> = (0..keep.len()).filter(|&i| indeg[i] == 0).collect();
        let mut inner = Vec::new();
        while let Some(p) = queue.pop() {
            let (m, _mb, t) = nodes[keep[p]];
            inner.push((m, t));
            for &q in &sub_edges[p] {
                indeg[q] -= 1;
                if indeg[q] == 0 {
                    queue.push(q);
                }
            }
        }
        if inner.len() != keep.len() {
            return Err(format!(
                "solve '{}': relaxing {} does not break the circularity — add more 'relax' variables",
                solves[k].name,
                relaxes.iter().map(|m| measures[*m].name.clone()).collect::<Vec<_>>().join(", ")
            ));
        }
        steps.push(Step::Tear { solve: k, relaxes, inner });
    }

    // ---- scenarios ---------------------------------------------------------
    let mut scenarios: Vec<ScenarioInfo> = Vec::new();
    for sc in &model.scenarios {
        if sc.name == "Base" && sc.overrides.is_empty() {
            continue; // declaring the implicit baseline is allowed and a no-op
        }
        if sc.name == "Base" {
            return Err(format!("line {}: 'Base' is the model as written and cannot carry overrides", sc.line));
        }
        if scenarios.iter().any(|x| x.name == sc.name) {
            return Err(format!("line {}: scenario '{}' declared twice", sc.line, sc.name));
        }
        let parent = match &sc.from {
            None => None,
            Some(f) if f == "Base" => None,
            Some(f) => Some(
                scenarios
                    .iter()
                    .position(|x| x.name == *f)
                    .ok_or_else(|| format!("line {}: scenario '{}' inherits unknown scenario '{}'", sc.line, sc.name, f))?,
            ),
        };
        let mut overrides = Vec::new();
        for (target, body, oline) in &sc.overrides {
            let m = *index
                .get(target)
                .ok_or_else(|| format!("line {oline}: scenario '{}' overrides unknown measure '{}'", sc.name, target))?;
            if !measures[m].is_input {
                return Err(format!(
                    "line {oline}: scenario '{}' overrides '{}', which is computed — only inputs can be overridden",
                    sc.name, target
                ));
            }
            // Unit-check the override against the input's declared unit,
            // in every member context the input has.
            let mdims = measures[m].dims.clone();
            for asg in enumerate_asgs(&mdims, &dims, n_dims) {
                let expected = expected_in(m, &asg);
                let check_one = |e: &Expr| -> Result<(), String> {
                    match unit_of(e, &asg, &env) {
                        Ok(Some(u)) if u != expected => Err(format!(
                            "line {oline}: scenario '{}' overrides '{}' ({}) with unit {}",
                            sc.name, target, expected, u
                        )),
                        Ok(_) => Ok(()),
                        Err(err) => Err(format!("line {oline}: in scenario '{}': {}", sc.name, err.msg())),
                    }
                };
                match body {
                    Body::Expr(e) => check_one(e)?,
                    Body::Map(entries) => {
                        if !measures[m].is_series {
                            return Err(format!(
                                "line {oline}: scenario '{}' gives a period map for scalar input '{}'",
                                sc.name, target
                            ));
                        }
                        for (lit, e) in entries {
                            calendar
                                .index(lit)
                                .map_err(|e2| format!("line {oline}: in scenario '{}': {}", sc.name, e2))?;
                            check_one(e)?;
                        }
                    }
                    Body::DimMatch { .. } => {
                        return Err(format!(
                            "line {oline}: scenario '{}': match bodies in overrides are not supported yet",
                            sc.name
                        ))
                    }
                }
            }
            // Override expressions may only reference inputs (same rule as inputs).
            let mut names = Vec::new();
            body_names_rec(body, &mut names);
            for nm in names {
                let d = *index
                    .get(&nm)
                    .ok_or_else(|| format!("line {oline}: scenario '{}' references unknown '{}'", sc.name, nm))?;
                if !measures[d].is_input {
                    return Err(format!(
                        "line {oline}: scenario '{}' override may only reference inputs (references '{}')",
                        sc.name, nm
                    ));
                }
            }
            overrides.push((m, body.clone()));
        }
        scenarios.push(ScenarioInfo { name: sc.name.clone(), parent, overrides });
    }

    let edit_sites = {
        let mut sites = Vec::new();
        for es in &model.edit_sites {
            let Some(&m) = index.get(&es.measure) else { continue };
            let mb = match (&es.member, measures[m].dims.len()) {
                (None, 0) => 0,
                (Some(mm), 1) => {
                    let Some(&(dim, idx)) = member_lookup.get(mm) else { continue };
                    if dim != measures[m].dims[0] {
                        continue;
                    }
                    idx
                }
                // Broadcast literals on dimensioned inputs and multi-dim
                // inputs are not span-patchable yet.
                _ => continue,
            };
            let t = match &es.period {
                Some(lit) => match calendar.index(lit) {
                    Ok(idx) => Some(idx),
                    Err(_) => continue,
                },
                None => None,
            };
            sites.push((m, mb, t, es.span, es.kind.clone()));
        }
        sites
    };

    // ---- distributions: per-period + correlate validation ------------------
    for m in &measures {
        if m.dist_per_period && !m.is_series {
            return Err(format!(
                "line {}: 'per period' needs a series input — give '{}' an 'over' clause",
                m.line, m.name
            ));
        }
    }
    let mut correlations: Vec<(usize, usize, f64)> = Vec::new();
    for c in &model.correlations {
        fn lit_num(e: &crate::ast::Expr) -> Option<f64> {
            match e {
                Expr::Num(v) => Some(*v),
                Expr::Pct(v) => Some(*v),
                Expr::Neg(inner) => lit_num(inner).map(|v| -v),
                _ => None,
            }
        }
        let resolve = |n: &str| -> Result<usize, String> {
            let m = *index
                .get(n)
                .ok_or_else(|| format!("line {}: correlate: unknown measure '{n}'", c.line))?;
            if measures[m].dist.is_none() {
                return Err(format!(
                    "line {}: correlate: '{n}' has no '~' distribution",
                    c.line
                ));
            }
            Ok(m)
        };
        let (ma, mb) = (resolve(&c.a)?, resolve(&c.b)?);
        if ma == mb {
            return Err(format!("line {}: correlate needs two distinct inputs", c.line));
        }
        if measures[ma].dist_per_period != measures[mb].dist_per_period {
            return Err(format!(
                "line {}: correlate: '{}' and '{}' draw at different frequencies — both or neither must be 'per period'",
                c.line, c.a, c.b
            ));
        }
        if measures[ma].dist_per_period && measures[ma].range != measures[mb].range {
            return Err(format!(
                "line {}: correlate: per-period inputs '{}' and '{}' must share the same range",
                c.line, c.a, c.b
            ));
        }
        let rho = lit_num(&c.rho).ok_or_else(|| {
            format!("line {}: correlation must be a numeric literal", c.line)
        })?;
        if rho.abs() >= 1.0 {
            return Err(format!(
                "line {}: correlation must be within (-1, 1), got {rho}",
                c.line
            ));
        }
        let key = (ma.min(mb), ma.max(mb));
        if correlations.iter().any(|(x, y, _)| (*x, *y) == key) {
            return Err(format!(
                "line {}: duplicate correlate for '{}' and '{}'",
                c.line, c.a, c.b
            ));
        }
        correlations.push((key.0, key.1, rho));
    }
    // Every correlated group's matrix must be positive definite — fail at
    // compile time, not mid-simulation.
    {
        let dist_ms: Vec<usize> = measures
            .iter()
            .enumerate()
            .filter(|(_, m)| m.dist.is_some())
            .map(|(i, _)| i)
            .collect();
        for group in corr_groups(&dist_ms, &correlations) {
            if group.len() > 1 && cholesky(&corr_matrix(&group, &correlations), group.len()).is_none() {
                let names: Vec<&str> =
                    group.iter().map(|&m| measures[m].name.as_str()).collect();
                return Err(format!(
                    "the correlations among {} are mutually inconsistent (matrix not positive definite) — lower them",
                    names.join(", ")
                ));
            }
        }
    }

    Ok(Checked {
        model_name: model.name.clone(),
        unit_reg,
        calendar,
        ranges,
        range_index,
        dims,
        member_lookup,
        group_lookup,
        functional_dim,
        measures,
        index,
        solves,
        scenarios,
        asserts: model.asserts().into_iter().cloned().collect(),
        steps,
        edit_sites,
        nodes,
        edges,
        node_id,
        correlations,
    })
}

// ---- t- and assignment-aware dependency extraction -------------------------
struct Pre<'a> {
    n: usize,
    measures: &'a [MeasureInfo],
    index: &'a HashMap<String, usize>,
    ranges: &'a [PeriodRange],
    range_index: &'a HashMap<String, usize>,
    dims: &'a [DimInfo],
    member_lookup: &'a HashMap<String, (usize, usize)>,
    group_lookup: &'a HashMap<String, usize>,
}

impl<'a> Pre<'a> {
    fn range(&self, name: &str) -> Result<&PeriodRange, String> {
        self.range_index
            .get(name)
            .map(|i| &self.ranges[*i])
            .ok_or_else(|| format!("unknown period range '{name}'"))
    }

    fn bound(&self, b: &Bound, t: usize) -> Result<isize, String> {
        Ok(match b {
            Bound::Rel(k) => t as isize + *k as isize,
            Bound::RangeStart(r, k) => self.range(r)?.start as isize + *k as isize,
            Bound::RangeEnd(r, k) => self.range(r)?.end as isize + *k as isize,
        })
    }

    fn tuple_of(&self, target: usize, asg: &[usize], ctx: usize) -> Result<usize, String> {
        let mut idx = 0usize;
        for &d in &self.measures[target].dims {
            let coord = asg[d];
            if coord == UNBOUND {
                return Err(format!(
                    "line {}: '{}' reads '{}' with dimension {} unbound — aggregate with sum[{}](…) or index a member",
                    self.measures[ctx].line,
                    self.measures[ctx].name,
                    self.measures[target].name,
                    self.dims[d].name,
                    self.dims[d].name
                ));
            }
            idx = idx * self.dims[d].members.len() + coord;
        }
        Ok(idx)
    }

    fn read_dep(
        &self,
        target: usize,
        asg: &[usize],
        at: isize,
        ctx: usize,
        out: &mut Vec<(usize, usize, usize)>,
    ) -> Result<(), String> {
        let mb = self.tuple_of(target, asg, ctx)?;
        let ti = &self.measures[target];
        if !ti.is_series {
            out.push((target, mb, 0));
            return Ok(());
        }
        if at < 0 || at as usize >= self.n {
            if ti.kind == Some(Kind::Stock) {
                return Err(format!(
                    "line {}: '{}' reads stock '{}' outside the calendar",
                    self.measures[ctx].line, self.measures[ctx].name, ti.name
                ));
            }
            return Ok(());
        }
        let at = at as usize;
        if at < ti.range.0 || at > ti.range.1 {
            if ti.kind == Some(Kind::Stock) {
                return Err(format!(
                    "line {}: '{}' reads stock '{}' outside its declared range",
                    self.measures[ctx].line, self.measures[ctx].name, ti.name
                ));
            }
            return Ok(());
        }
        out.push((target, mb, at));
        Ok(())
    }

    fn walk(
        &self,
        e: &Expr,
        m: usize,
        asg: &[usize],
        t: usize,
        out: &mut Vec<(usize, usize, usize)>,
    ) -> Result<(), String> {
        match e {
            Expr::Ref(name) => self.read_dep(self.index[name], asg, t as isize, m, out),
            Expr::MemberIx { name, members } => {
                let target = self.index[name];
                let mut asgs = vec![asg.to_vec()];
                for mname in members {
                    if let Some(&(dim, idx)) = self.member_lookup.get(mname) {
                        if !self.measures[target].dims.contains(&dim) {
                            return Err(format!(
                                "line {}: '{}' is not over dimension {} — '[{}]' is invalid",
                                self.measures[m].line, name, self.dims[dim].name, mname
                            ));
                        }
                        for a in asgs.iter_mut() {
                            a[dim] = idx;
                        }
                    } else if let Some(&dim) = self.group_lookup.get(mname) {
                        if !self.measures[target].dims.contains(&dim) {
                            return Err(format!(
                                "line {}: '{}' is not over dimension {} — '[{}]' is invalid",
                                self.measures[m].line, name, self.dims[dim].name, mname
                            ));
                        }
                        let mut next = Vec::new();
                        for a in &asgs {
                            for idx in 0..self.dims[dim].members.len() {
                                let mut a2 = a.clone();
                                a2[dim] = idx;
                                next.push(a2);
                            }
                        }
                        asgs = next;
                    } else {
                        return Err(format!("line {}: unknown member '{}'", self.measures[m].line, mname));
                    }
                }
                for a in &asgs {
                    self.read_dep(target, a, t as isize, m, out)?;
                }
                Ok(())
            }
            Expr::Prev(name, inline_init) => {
                let target = self.index[name];
                let ti = &self.measures[target];
                if !ti.is_series {
                    return Err(format!(
                        "line {}: prev({}) — '{}' is a scalar",
                        self.measures[m].line, name, name
                    ));
                }
                let ts = t as isize - 1;
                if ts >= ti.range.0 as isize {
                    let mb = self.tuple_of(target, asg, m)?;
                    out.push((target, mb, ts as usize));
                    Ok(())
                } else if let Some(init) = inline_init {
                    self.walk(init, m, asg, t, out)
                } else if let Some(init) = &ti.init {
                    let init = init.clone();
                    self.walk(&init, m, asg, t, out)
                } else {
                    Err(format!(
                        "line {}: 'prev({})' reaches the start of '{}' — give it an 'init' value",
                        self.measures[m].line, name, name
                    ))
                }
            }
            Expr::At { name, bound } => {
                let at = self.bound(bound, t)?;
                self.read_dep(self.index[name], asg, at, m, out)
            }
            Expr::WindowSum { name, from, to } => {
                let a = self.bound(from, t)?;
                let b = self.bound(to, t)?;
                for at in a..=b {
                    self.read_dep(self.index[name], asg, at, m, out)?;
                }
                Ok(())
            }
            Expr::RangeSum { range, body } => {
                if let Some(did) = self.dims.iter().position(|d| d.name == *range) {
                    for c in 0..self.dims[did].members.len() {
                        let mut a = asg.to_vec();
                        a[did] = c;
                        self.walk(body, m, &a, t, out)?;
                    }
                    return Ok(());
                }
                let r = self.range(range)?.clone();
                for p in r.start..=r.end {
                    self.walk(body, m, asg, p, out)?;
                }
                Ok(())
            }
            Expr::Npv { rate, body, range } => {
                self.walk(rate, m, asg, t, out)?;
                let r = self.range(range)?.clone();
                for p in r.start..=r.end {
                    self.walk(body, m, asg, p, out)?;
                }
                Ok(())
            }
            Expr::Irr { name, .. } => {
                let target = self.index[name];
                let mb = self.tuple_of(target, asg, m)?;
                let ti = &self.measures[target];
                for p in ti.range.0..=ti.range.1 {
                    out.push((target, mb, p));
                }
                Ok(())
            }
            Expr::When { value, pos, range } => {
                let r = self.range(range)?;
                let boundary = match pos {
                    FirstLast::First => r.start,
                    FirstLast::Last => r.end,
                };
                if t == boundary {
                    self.walk(value, m, asg, t, out)?;
                }
                Ok(())
            }
            Expr::MatchT(arms) => {
                for (set, arm) in arms {
                    let base = self.range(&set.base)?;
                    let excluded = match &set.minus {
                        Some(x) => self.range(x)?.contains(t),
                        None => false,
                    };
                    if base.contains(t) && !excluded {
                        return self.walk(arm, m, asg, t, out);
                    }
                }
                Ok(())
            }
            Expr::MatchDim { dim, arms, default } => {
                let did = self
                    .dims
                    .iter()
                    .position(|d| d.name == *dim)
                    .ok_or_else(|| format!("line {}: unknown dimension '{}'", self.measures[m].line, dim))?;
                let c = asg[did];
                if c == UNBOUND {
                    return Err(format!(
                        "line {}: '{}' matches on {} outside a {}-bound context",
                        self.measures[m].line, self.measures[m].name, dim, dim
                    ));
                }
                let mname = &self.dims[did].members[c];
                for (arm_member, arm) in arms {
                    if arm_member == mname {
                        return self.walk(arm, m, asg, t, out);
                    }
                }
                if let Some(def) = default {
                    return self.walk(def, m, asg, t, out);
                }
                Err(format!(
                    "line {}: '{}': no match arm for member '{}' (add an 'else')",
                    self.measures[m].line, self.measures[m].name, mname
                ))
            }
            Expr::Conv { body, rate, .. } => {
                self.walk(body, m, asg, t, out)?;
                if let Some(rate) = rate {
                    self.walk(rate, m, asg, t, out)?;
                }
                Ok(())
            }
            Expr::Neg(x) | Expr::Annualize(x) => self.walk(x, m, asg, t, out),
            Expr::Bin(_, a, b) => {
                self.walk(a, m, asg, t, out)?;
                self.walk(b, m, asg, t, out)
            }
            Expr::Call(_, args) => {
                for a in args {
                    self.walk(a, m, asg, t, out)?;
                }
                Ok(())
            }
            Expr::Num(_) | Expr::Qty(_, _) | Expr::Pct(_) | Expr::YearT => Ok(()),
        }
    }
}

// ---- unit inference over expressions --------------------------------------
pub(crate) enum UnitErr {
    Hard(String),
}

impl UnitErr {
    pub(crate) fn msg(self) -> String {
        match self {
            UnitErr::Hard(m) => m,
        }
    }
}

pub(crate) struct UnitEnv<'a> {
    pub known: &'a [MUnit],
    pub index: &'a HashMap<String, usize>,
    pub dims: &'a [DimInfo],
    pub member_lookup: &'a HashMap<String, (usize, usize)>,
    pub group_lookup: &'a HashMap<String, usize>,
    pub functional_dim: Option<usize>,
    pub unit_reg: &'a HashMap<String, Unit>,
}

pub(crate) fn unit_of(e: &Expr, asg: &[usize], env: &UnitEnv) -> Result<Option<Unit>, UnitErr> {
    let resolve = |i: usize, asg: &[usize]| -> Result<Unit, UnitErr> {
        match &env.known[i] {
            MUnit::Uniform(u) => Ok(u.clone()),
            MUnit::Local => {
                let fd = env
                    .functional_dim
                    .ok_or_else(|| UnitErr::Hard("local unit without a functional dimension".into()))?;
                let c = asg.get(fd).copied().unwrap_or(UNBOUND);
                if c == UNBOUND {
                    return Err(UnitErr::Hard(
                        "member-dependent ('local') unit used outside a member context".into(),
                    ));
                }
                Ok(env.dims[fd].currencies[c].clone())
            }
        }
    };
    match e {
        Expr::Num(_) => Ok(None),
        Expr::Pct(_) | Expr::YearT => Ok(Some(Unit::one())),
        Expr::Qty(_, u) => Ok(Some(env.unit_reg.get(u).cloned().unwrap_or_else(|| Unit::base(u)))),
        Expr::Ref(n) | Expr::At { name: n, .. } | Expr::WindowSum { name: n, .. } => {
            Ok(Some(resolve(env.index[n], asg)?))
        }
        Expr::MemberIx { name, members } => {
            let i = env.index[name];
            let mut a = asg.to_vec();
            if a.len() < env.dims.len() {
                a.resize(env.dims.len(), UNBOUND);
            }
            let mut has_group = false;
            for mname in members {
                if let Some(&(dim, idx)) = env.member_lookup.get(mname) {
                    a[dim] = idx;
                } else if env.group_lookup.contains_key(mname) {
                    has_group = true;
                } else {
                    return Err(UnitErr::Hard(format!("unknown member '{mname}'")));
                }
            }
            if has_group {
                if let MUnit::Local = &env.known[i] {
                    // Legal only when the functional coordinate is pinned.
                    if env.functional_dim.map(|fd| a[fd] == UNBOUND).unwrap_or(true) {
                        return Err(UnitErr::Hard(format!(
                            "'{name}[…]': cannot aggregate member-dependent ('local') units — translate to one currency first"
                        )));
                    }
                }
            }
            Ok(Some(resolve(i, &a)?))
        }
        Expr::Prev(n, init) => {
            let u = Some(resolve(env.index[n], asg)?);
            if let Some(i) = init {
                let iu = unit_of(i, asg, env)?;
                return join_additive(u, iu);
            }
            Ok(u)
        }
        Expr::Conv { body, target, rate } => {
            let bu = unit_of(body, asg, env)?
                .ok_or_else(|| UnitErr::Hard("cannot convert a bare literal — give it a unit".into()))?;
            let tu = env
                .unit_reg
                .get(target)
                .cloned()
                .unwrap_or_else(|| Unit::base(target));
            match rate {
                Some(rate) => {
                    let ru = unit_of(rate, asg, env)?
                        .ok_or_else(|| UnitErr::Hard("conversion rate needs a unit (e.g. USD/EUR)".into()))?;
                    if bu == tu || bu.mul(&ru) == tu || bu.div(&ru) == tu {
                        Ok(Some(tu))
                    } else {
                        Err(UnitErr::Hard(format!("cannot convert {bu} to {tu} with a rate in {ru}")))
                    }
                }
                None => {
                    if bu.same_dimension(&tu) {
                        Ok(Some(tu))
                    } else {
                        Err(UnitErr::Hard(format!(
                            "cannot convert {bu} to {tu} without a rate — they differ in dimension"
                        )))
                    }
                }
            }
        }
        Expr::Irr { .. } => Ok(Some(Unit::one())),
        Expr::Annualize(x) => {
            let u = unit_of(x, asg, env)?;
            if let Some(u) = &u {
                if !u.is_dimensionless() {
                    return Err(UnitErr::Hard(format!("annualize needs a dimensionless rate, got {u}")));
                }
            }
            Ok(Some(Unit::one()))
        }
        Expr::Neg(x) => unit_of(x, asg, env),
        Expr::When { value, .. } => unit_of(value, asg, env),
        Expr::MatchT(arms) => {
            let mut acc: Option<Unit> = None;
            for (_, a) in arms {
                let u = unit_of(a, asg, env)?;
                acc = join_additive(acc, u)?;
            }
            Ok(acc)
        }
        Expr::MatchDim { dim, arms, default } => {
            let did = env
                .dims
                .iter()
                .position(|d| d.name == *dim)
                .ok_or_else(|| UnitErr::Hard(format!("unknown dimension '{dim}'")))?;
            let c = asg.get(did).copied().unwrap_or(UNBOUND);
            if c == UNBOUND {
                return Err(UnitErr::Hard(format!("match on {dim} outside a {dim}-bound context")));
            }
            let mname = &env.dims[did].members[c];
            for (arm_member, a) in arms {
                if arm_member == mname {
                    return unit_of(a, asg, env);
                }
            }
            match default {
                Some(def) => unit_of(def, asg, env),
                None => Err(UnitErr::Hard(format!("no match arm for member '{mname}' (add an 'else')"))),
            }
        }
        Expr::RangeSum { range, body } => {
            if let Some(did) = env.dims.iter().position(|d| d.name == *range) {
                let mut acc: Option<Unit> = None;
                for c in 0..env.dims[did].members.len() {
                    let mut a = asg.to_vec();
                    a[did] = c;
                    let u = unit_of(body, &a, env)?;
                    acc = join_additive(acc, u).map_err(|_| {
                        UnitErr::Hard(format!(
                            "sum[{range}]: member units differ — translate to one currency first"
                        ))
                    })?;
                }
                return Ok(acc);
            }
            unit_of(body, asg, env)
        }
        Expr::Npv { rate, body, .. } => {
            let ru = unit_of(rate, asg, env)?;
            if let Some(ru) = &ru {
                if !ru.is_dimensionless() {
                    return Err(UnitErr::Hard(format!("npv rate must be dimensionless, got {ru}")));
                }
            }
            unit_of(body, asg, env)
        }
        Expr::Call(_, args) => {
            let mut acc: Option<Unit> = None;
            for a in args {
                let u = unit_of(a, asg, env)?;
                acc = join_additive(acc, u)?;
            }
            Ok(acc)
        }
        Expr::Bin(op, a, b) => {
            let ua = unit_of(a, asg, env)?;
            let ub = unit_of(b, asg, env)?;
            match op {
                BinOp::Add | BinOp::Sub => join_additive(ua, ub),
                BinOp::Mul => Ok(match (ua, ub) {
                    (None, None) => None,
                    (None, Some(u)) | (Some(u), None) => {
                        if u.is_dimensionless() {
                            None
                        } else {
                            Some(u)
                        }
                    }
                    (Some(x), Some(y)) => Some(x.mul(&y)),
                }),
                BinOp::Div => Ok(match (ua, ub) {
                    (None, None) => None,
                    (Some(x), None) => {
                        if x.is_dimensionless() {
                            None
                        } else {
                            Some(x)
                        }
                    }
                    (None, Some(y)) => {
                        if y.is_dimensionless() {
                            None
                        } else {
                            Some(y.inv())
                        }
                    }
                    (Some(x), Some(y)) => Some(x.div(&y)),
                }),
                BinOp::Pow => {
                    for u in [&ua, &ub] {
                        if let Some(u) = u {
                            if !u.is_dimensionless() {
                                return Err(UnitErr::Hard(format!(
                                    "'^' needs dimensionless base and exponent, got {u}"
                                )));
                            }
                        }
                    }
                    Ok(Some(Unit::one()))
                }
            }
        }
    }
}

fn join_additive(a: Option<Unit>, b: Option<Unit>) -> Result<Option<Unit>, UnitErr> {
    match (a, b) {
        (None, x) | (x, None) => Ok(x),
        (Some(x), Some(y)) => {
            if x == y {
                Ok(Some(x))
            } else {
                Err(UnitErr::Hard(format!("cannot add/compare {x} with {y}")))
            }
        }
    }
}

// ---- Tarjan SCC (iterative) ------------------------------------------------
fn tarjan(n: usize, edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy)]
    struct Frame {
        v: usize,
        edge_i: usize,
    }
    let mut idx = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut out: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if idx[root] != usize::MAX {
            continue;
        }
        let mut call: Vec<Frame> = vec![Frame { v: root, edge_i: 0 }];
        while let Some(fr) = call.last().copied() {
            let v = fr.v;
            if fr.edge_i == 0 {
                idx[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if fr.edge_i < edges[v].len() {
                let w = edges[v][fr.edge_i];
                call.last_mut().unwrap().edge_i += 1;
                if idx[w] == usize::MAX {
                    call.push(Frame { v: w, edge_i: 0 });
                } else if on_stack[w] {
                    low[v] = low[v].min(idx[w]);
                }
            } else {
                call.pop();
                if let Some(parent) = call.last() {
                    let pv = parent.v;
                    low[pv] = low[pv].min(low[v]);
                }
                if low[v] == idx[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    out.push(comp);
                }
            }
        }
    }
    out
}
