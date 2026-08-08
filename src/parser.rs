//! Recursive-descent parser for the Phase-1 .fml subset (extended:
//! dimensions, `local` currencies, conversion, member indexing, monthly
//! calendars).

use crate::ast::*;
use crate::calendar::PeriodLit;
use crate::lexer::{lex, SpannedTok, Tok};

pub struct Parser {
    toks: Vec<SpannedTok>,
    pos: usize,
    unit_names: Vec<String>,
    dim_members: Vec<String>, // leaf members + group name
    /// Literal spans collected while parsing the current input body.
    site_buf: Vec<(Option<String>, Option<PeriodLit>, (usize, usize), LitKind)>,
    /// Member context while parsing a DimMatch input arm.
    cur_member: Option<String>,
    calendar_name: Option<String>,
    edit_sites: Vec<EditSite>,
    dim_names: Vec<String>,
    /// Set while parsing an `allocate` declaration: the body is
    /// `<total> by <driver>`, desugared to a proportional split.
    alloc_mode: bool,
    alloc_parts: Option<(Expr, String)>,
}

const DIMLESS_ALIASES: [&str; 2] = ["rate", "ratio"];
const KEYWORDS: [&str; 33] = [
    "model", "calendar", "currency", "unit", "input", "solve", "assert", "period",
    "over", "init", "tolerance", "max_iterations", "prev", "relax", "when", "match", "in",
    "dimension", "functional", "at", "eliminate", "against", "scenario", "from", "actuals", "until", "metalog", "uniform", "normal", "correlate", "per", "allocate", "by",
];

fn lit_kind(e: &Expr) -> Option<LitKind> {
    match e {
        Expr::Num(_) => Some(LitKind::Num),
        Expr::Pct(_) => Some(LitKind::Pct),
        Expr::Qty(_, u) => Some(LitKind::Qty(u.clone())),
        Expr::Neg(x) => lit_kind(x),
        _ => None,
    }
}

impl Parser {
    pub fn parse(src: &str) -> Result<Model, String> {
        let toks = lex(src)?;
        let mut p = Parser {
            toks,
            pos: 0,
            unit_names: Vec::new(),
            dim_members: Vec::new(),
            site_buf: Vec::new(),
            cur_member: None,
            calendar_name: None,
            edit_sites: Vec::new(),
            dim_names: Vec::new(),
            alloc_mode: false,
            alloc_parts: None,
        };
        p.model()
    }

    // ---- token helpers -------------------------------------------------
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k).map(|t| &t.tok)
    }

    fn line(&self) -> usize {
        self.toks
            .get(self.pos.min(self.toks.len().saturating_sub(1)))
            .map(|t| t.line)
            .unwrap_or(0)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.tok.clone());
        self.pos += 1;
        t
    }

    fn eat_sym(&mut self, s: &str) -> bool {
        if let Some(Tok::Sym(x)) = self.peek() {
            if *x == s {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn expect_sym(&mut self, s: &str) -> Result<(), String> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(format!(
                "line {}: expected '{}', found {}",
                self.line(),
                s,
                self.peek().map(|t| t.to_string()).unwrap_or_else(|| "end of file".into())
            ))
        }
    }

    fn peek_ident(&self) -> Option<&str> {
        if let Some(Tok::Ident(s)) = self.peek() {
            Some(s.as_str())
        } else {
            None
        }
    }

    fn ident_at(&self, k: usize) -> Option<&str> {
        if let Some(Tok::Ident(s)) = self.peek_at(k) {
            Some(s.as_str())
        } else {
            None
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek_ident() == Some(kw) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            other => Err(format!(
                "line {}: expected identifier, found {}",
                self.line(),
                other.map(|t| t.to_string()).unwrap_or_else(|| "end of file".into())
            )),
        }
    }

    fn expect_num(&mut self) -> Result<f64, String> {
        match self.bump() {
            Some(Tok::Num(n)) => Ok(n),
            other => Err(format!(
                "line {}: expected number, found {}",
                self.line(),
                other.map(|t| t.to_string()).unwrap_or_else(|| "end of file".into())
            )),
        }
    }

    fn cur_start(&self) -> usize {
        self.toks.get(self.pos).map(|t| t.start).unwrap_or(0)
    }

    fn prev_end(&self) -> usize {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].end
        }
    }

    fn is_unit_name(&self, s: &str) -> bool {
        self.unit_names.iter().any(|u| u == s)
    }

    fn is_member(&self, s: &str) -> bool {
        self.dim_members.iter().any(|m| m == s)
    }

    /// `2026`, `2026-Q3`, or `2026-07` (only in period contexts).
    fn period_lit(&mut self) -> Result<PeriodLit, String> {
        let year = self.expect_num()? as i64;
        if let Some(Tok::Sym("-")) = self.peek() {
            match self.peek_at(1) {
                Some(Tok::Ident(q)) => {
                    if let Some(rest) = q.strip_prefix('Q') {
                        if let Ok(qn) = rest.parse::<u8>() {
                            self.pos += 2;
                            return Ok(PeriodLit { year, sub: Some(qn), q_form: true });
                        }
                    }
                }
                Some(Tok::Num(m)) => {
                    let m = *m;
                    if m.fract() == 0.0 && (1.0..=12.0).contains(&m) {
                        self.pos += 2;
                        return Ok(PeriodLit { year, sub: Some(m as u8), q_form: false });
                    }
                }
                _ => {}
            }
        }
        Ok(PeriodLit { year, sub: None, q_form: false })
    }

    // ---- grammar -------------------------------------------------------
    fn model(&mut self) -> Result<Model, String> {
        if !self.eat_kw("model") {
            return Err(format!("line {}: file must start with 'model <name>'", self.line()));
        }
        let mut name = self.expect_ident()?;
        while self.eat_sym(".") {
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        let mut model = Model {
            name,
            calendar: None,
            period_ranges: Vec::new(),
            dimensions: Vec::new(),
            functional: None,
            currency: None,
            units: Vec::new(),
            items: Vec::new(),
            scenarios: Vec::new(),
            edit_sites: Vec::new(),
            correlations: Vec::new(),
        };
        while self.peek().is_some() {
            match self.peek_ident() {
                Some("calendar") => {
                    self.pos += 1;
                    let cname = self.expect_ident()?;
                    self.expect_sym("=")?;
                    let grain = self.expect_ident()?;
                    let start = self.period_lit()?;
                    self.expect_sym("..")?;
                    let end = self.period_lit()?;
                    self.calendar_name = Some(cname.clone());
                    model.calendar = Some(CalendarDecl { name: cname, grain, start, end });
                }
                Some("period") => {
                    self.pos += 1;
                    let pname = self.expect_ident()?;
                    self.expect_sym("=")?;
                    let start = self.period_lit()?;
                    let end = if self.eat_sym("..") { self.period_lit()? } else { start };
                    model.period_ranges.push(PeriodDecl { name: pname, start, end });
                }
                Some("dimension") => {
                    self.pos += 1;
                    let dname = self.expect_ident()?;
                    self.expect_sym("=")?;
                    let (group, members) = if self.eat_kw("tree") {
                        self.expect_sym("{")?;
                        let g = self.expect_ident()?;
                        self.expect_sym("->")?;
                        self.expect_sym("{")?;
                        let mut ms = Vec::new();
                        loop {
                            ms.push(self.expect_ident()?);
                            if !self.eat_sym(",") {
                                break;
                            }
                        }
                        self.expect_sym("}")?;
                        self.expect_sym("}")?;
                        (Some(g), ms)
                    } else if self.eat_kw("list") {
                        self.expect_sym("{")?;
                        let mut ms = Vec::new();
                        loop {
                            ms.push(self.expect_ident()?);
                            if !self.eat_sym(",") {
                                break;
                            }
                        }
                        self.expect_sym("}")?;
                        (None, ms)
                    } else {
                        return Err(format!(
                            "line {}: expected 'tree {{ Group -> {{ … }} }}' or 'list {{ … }}'",
                            self.line()
                        ));
                    };
                    for m in &members {
                        self.dim_members.push(m.clone());
                    }
                    if let Some(g) = &group {
                        self.dim_members.push(g.clone());
                    }
                    self.dim_names.push(dname.clone());
                    model.dimensions.push(DimensionDecl { name: dname, group, members });
                }
                Some("functional") => {
                    self.pos += 1;
                    let dim = self.expect_ident()?;
                    self.expect_sym("=")?;
                    self.expect_sym("{")?;
                    let mut map = Vec::new();
                    loop {
                        let member = self.expect_ident()?;
                        self.expect_sym(":")?;
                        let ccy = self.expect_ident()?;
                        map.push((member, ccy));
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                    self.expect_sym("}")?;
                    model.functional = Some(FunctionalDecl { dim, map });
                }
                Some("currency") => {
                    self.pos += 1;
                    let c = self.expect_ident()?;
                    self.unit_names.push(c.clone());
                    model.currency = Some(c);
                }
                Some("unit") => {
                    self.pos += 1;
                    loop {
                        let u = self.expect_ident()?;
                        self.unit_names.push(u.clone());
                        // `unit kEUR = 1000 EUR` — a scaled unit
                        let scaled = if self.eat_sym("=") {
                            let factor = self.expect_num()?;
                            let base = self.expect_ident()?;
                            Some((factor, base))
                        } else {
                            None
                        };
                        model.units.push(UnitDecl { name: u, scaled });
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                }
                Some("input") => {
                    self.pos += 1;
                    let m = self.measure(true)?;
                    model.items.push(Item::Measure(m));
                }
                Some("solve") => {
                    let s = self.solve()?;
                    model.items.push(Item::Solve(s));
                }
                Some("assert") => {
                    let a = self.assert_decl()?;
                    model.items.push(Item::Assert(a));
                }
                Some("scenario") => {
                    let line = self.line();
                    self.pos += 1;
                    let name = self.expect_ident()?;
                    let from = if self.eat_kw("from") { Some(self.expect_ident()?) } else { None };
                    let mut overrides = Vec::new();
                    if self.eat_sym("{") {
                        while !self.eat_sym("}") {
                            if self.peek().is_none() {
                                return Err(format!("line {}: unterminated scenario '{name}'", self.line()));
                            }
                            let oline = self.line();
                            let target = self.expect_ident()?;
                            self.expect_sym("=")?;
                            let body = if matches!(self.peek(), Some(Tok::Sym("{"))) {
                                self.map_literal()?
                            } else {
                                Body::Expr(self.expr()?)
                            };
                            overrides.push((target, body, oline));
                        }
                    }
                    // Scenario-override literals are not grid edit sites.
                    self.site_buf.clear();
                    model.scenarios.push(ScenarioDecl { name, from, overrides, line });
                }
                Some("allocate") => {
                    self.pos += 1;
                    self.alloc_mode = true;
                    let m = self.measure(false);
                    self.alloc_mode = false;
                    let m = m?;
                    let (total, dim) = self.alloc_parts.take().expect("allocate sets parts");
                    // Conservation by construction, proven by a tie-assert:
                    // the allocated pieces must re-add to the total.
                    let over_cal = m.over.iter().find(|o| **o != dim).cloned();
                    model.items.push(Item::Measure(m.clone()));
                    model.items.push(Item::Assert(AssertDecl {
                        name: format!("allocate_{}", m.name),
                        over: over_cal,
                        lhs: Expr::RangeSum { range: dim, body: Box::new(Expr::Ref(m.name.clone())) },
                        op: CmpOp::Eq,
                        rhs: total,
                        tol: Some(Expr::Num(1e-6)),
                        line: m.line,
                    }));
                }
                Some("correlate") => {
                    let line = self.line();
                    self.pos += 1;
                    let a = self.expect_ident()?;
                    self.expect_sym(",")?;
                    let b = self.expect_ident()?;
                    self.expect_sym("=")?;
                    let rho = self.expr()?;
                    model.correlations.push(CorrDecl { a, b, rho, line });
                }
                Some("eliminate") => {
                    let line = self.line();
                    self.pos += 1;
                    let ename = self.expect_ident()?;
                    let over = if self.eat_kw("over") { Some(self.expect_ident()?) } else { None };
                    self.expect_sym(":")?;
                    let lhs = self.expr()?;
                    if !self.eat_kw("against") {
                        return Err(format!(
                            "line {}: eliminate needs '<lhs> against <rhs>'",
                            self.line()
                        ));
                    }
                    let rhs = self.expr()?;
                    let tol = if self.eat_sym("±") { Some(self.expr()?) } else { None };
                    // An elimination pair is a conservation-checked tie:
                    // it desugars to an equality assert (the contra-entry
                    // posting side lives in explicit Group formulas for now).
                    model.items.push(Item::Assert(AssertDecl {
                        name: format!("eliminate_{ename}"),
                        over,
                        lhs,
                        op: CmpOp::Eq,
                        rhs,
                        tol,
                        line,
                    }));
                }
                Some(_) => {
                    let m = self.measure(false)?;
                    model.items.push(Item::Measure(m));
                }
                None => {
                    return Err(format!(
                        "line {}: expected a declaration, found {}",
                        self.line(),
                        self.peek().map(|t| t.to_string()).unwrap_or_default()
                    ))
                }
            }
        }
        model.edit_sites = std::mem::take(&mut self.edit_sites);
        Ok(model)
    }

    fn measure(&mut self, is_input: bool) -> Result<MeasureDecl, String> {
        let line = self.line();
        let name = self.expect_ident()?;
        if KEYWORDS.contains(&name.as_str()) {
            return Err(format!("line {line}: '{name}' is a keyword and cannot name a measure"));
        }
        let mut ann = TypeAnn::default();
        let mut over = Vec::new();
        let mut init = None;
        if self.eat_sym(":") {
            let unit = match self.peek() {
                Some(Tok::Num(n)) if *n == 1.0 => {
                    self.pos += 1;
                    Some(UnitAst { num: "1".into(), den: None })
                }
                Some(Tok::Ident(s))
                    if self.is_unit_name(s)
                        || DIMLESS_ALIASES.contains(&s.as_str())
                        || s == "local" =>
                {
                    let num = self.expect_ident()?;
                    let den = if self.eat_sym("/") { Some(self.expect_ident()?) } else { None };
                    Some(UnitAst { num, den })
                }
                _ => None,
            };
            ann.unit = unit;
            ann.kind = match self.peek_ident() {
                Some("stock") => {
                    self.pos += 1;
                    Some(Kind::Stock)
                }
                Some("flow") => {
                    self.pos += 1;
                    Some(Kind::Flow)
                }
                _ => None,
            };
            if ann.unit.is_none() && ann.kind.is_none() {
                return Err(format!(
                    "line {}: expected a unit or kind after ':' (declare units with 'unit'/'currency')",
                    self.line()
                ));
            }
            if self.eat_kw("over") {
                loop {
                    over.push(self.expect_ident()?);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            }
            if self.eat_kw("init") {
                let label = if self.eat_sym(":") {
                    None
                } else {
                    let l = self.period_lit()?;
                    self.expect_sym(":")?;
                    Some(l)
                };
                let e = self.expr()?;
                init = Some((label, e));
            }
        }
        if self.eat_sym("~") {
            if !is_input {
                return Err(format!(
                    "line {line}: '~' distributions are for inputs only"
                ));
            }
            let kind = self.expect_ident()?;
            let mut params = Vec::new();
            match kind.as_str() {
                "metalog" => {
                    self.expect_sym("{")?;
                    loop {
                        let key = self.expect_ident()?;
                        self.expect_sym(":")?;
                        params.push((Some(key), self.expr()?));
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                    self.expect_sym("}")?;
                }
                "uniform" | "normal" => {
                    self.expect_sym("(")?;
                    params.push((None, self.expr()?));
                    self.expect_sym(",")?;
                    params.push((None, self.expr()?));
                    self.expect_sym(")")?;
                }
                other => {
                    return Err(format!(
                        "line {}: unknown distribution '{other}' (metalog | uniform | normal)",
                        self.line()
                    ))
                }
            }
            // `per period`: an independent draw each period (iid shocks)
            // rather than one draw per trial (parameter uncertainty).
            let per_period = if self.eat_kw("per") {
                if !self.eat_kw("period") {
                    return Err(format!("line {}: expected 'per period'", self.line()));
                }
                true
            } else {
                false
            };
            self.site_buf.clear();
            return Ok(MeasureDecl {
                name,
                is_input,
                ann,
                over,
                init,
                // Placeholder; the checker substitutes the median so the
                // model stays deterministic until `simulate` is invoked.
                body: Body::Expr(Expr::Num(f64::NAN)),
                dist: Some(DistDecl { kind, params, per_period }),
                line,
            });
        }
        if self.alloc_mode {
            // `allocate x : u over Dim, cal = <total> by <driver>` desugars
            // to the proportional split total * driver / sum[Dim](driver).
            self.expect_sym("=")?;
            let total = self.expr()?;
            if !self.eat_kw("by") {
                return Err(format!(
                    "line {}: allocate needs '= <total> by <driver>'",
                    self.line()
                ));
            }
            let driver = self.expr()?;
            let dims: Vec<&String> = over.iter().filter(|o| self.dim_names.contains(o)).collect();
            if dims.len() != 1 {
                return Err(format!(
                    "line {line}: allocate '{name}' must range over exactly one dimension (found {})",
                    dims.len()
                ));
            }
            let dim = dims[0].clone();
            let body = Expr::Bin(
                BinOp::Mul,
                Box::new(total.clone()),
                Box::new(Expr::Bin(
                    BinOp::Div,
                    Box::new(driver.clone()),
                    Box::new(Expr::RangeSum { range: dim.clone(), body: Box::new(driver) }),
                )),
            );
            self.site_buf.clear();
            self.alloc_parts = Some((total, dim));
            return Ok(MeasureDecl {
                name,
                is_input: false,
                ann,
                over,
                init,
                body: Body::Expr(body),
                dist: None,
                line,
            });
        }
        self.expect_sym("=")?;
        self.site_buf.clear();
        let body = if matches!(self.peek(), Some(Tok::Sym("{"))) {
            // A `{` here is a period map (inputs). `match` bodies start with
            // the keyword, so no ambiguity.
            self.map_literal()?
        } else if is_input
            && self.peek_ident() == Some("match")
            && self.ident_at(1).map(|d| !self.is_member(d) && d != "t").unwrap_or(false)
        {
            self.dim_match_body()?
        } else {
            let s0 = self.cur_start();
            let e = self.expr()?;
            let s1 = self.prev_end();
            if let Some(k) = lit_kind(&e) {
                self.site_buf.push((None, None, (s0, s1), k));
            }
            Body::Expr(e)
        };
        if is_input {
            for (member, period, span, kind) in self.site_buf.drain(..) {
                self.edit_sites.push(EditSite { measure: name.clone(), member, period, span, kind });
            }
        } else {
            self.site_buf.clear();
        }
        Ok(MeasureDecl { name, is_input, ann, over, init, body, dist: None, line })
    }

    /// Input body: `match Dim { Member -> <map or expr> ... [else -> …] }`.
    fn dim_match_body(&mut self) -> Result<Body, String> {
        self.pos += 1; // 'match'
        let dim = self.expect_ident()?;
        self.expect_sym("{")?;
        let mut arms = Vec::new();
        let mut default = None;
        loop {
            let member = self.expect_ident()?;
            self.expect_sym("->")?;
            let is_default = member == "else";
            self.cur_member = if is_default { None } else { Some(member.clone()) };
            let arm = if matches!(self.peek(), Some(Tok::Sym("{"))) {
                self.map_literal()?
            } else {
                let s0 = self.cur_start();
                let e = self.expr()?;
                let s1 = self.prev_end();
                if !is_default {
                    if let Some(k) = lit_kind(&e) {
                        self.site_buf.push((self.cur_member.clone(), None, (s0, s1), k));
                    }
                }
                Body::Expr(e)
            };
            self.cur_member = None;
            if is_default {
                default = Some(Box::new(arm));
            } else {
                arms.push((member, arm));
            }
            let _ = self.eat_sym(",");
            if self.eat_sym("}") {
                break;
            }
        }
        Ok(Body::DimMatch { dim, arms, default })
    }

    fn map_literal(&mut self) -> Result<Body, String> {
        self.expect_sym("{")?;
        let mut entries = Vec::new();
        loop {
            let key = self.period_lit()?;
            self.expect_sym(":")?;
            let s0 = self.cur_start();
            let val = self.expr()?;
            let s1 = self.prev_end();
            if let Some(k) = lit_kind(&val) {
                self.site_buf.push((self.cur_member.clone(), Some(key), (s0, s1), k));
            }
            entries.push((key, val));
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym("}")?;
        Ok(Body::Map(entries))
    }

    fn solve(&mut self) -> Result<SolveDecl, String> {
        let line = self.line();
        self.pos += 1; // 'solve'
        let name = self.expect_ident()?;
        let mut tolerance = None;
        let mut max_iterations = 100u32;
        loop {
            if self.eat_kw("tolerance") {
                tolerance = Some(self.expr()?);
            } else if self.eat_kw("max_iterations") {
                max_iterations = self.expect_num()? as u32;
            } else {
                break;
            }
        }
        self.expect_sym("{")?;
        let mut relaxes: Vec<RelaxDecl> = Vec::new();
        let mut members: Vec<MeasureDecl> = Vec::new();
        while !matches!(self.peek(), Some(Tok::Sym("}"))) {
            if self.peek().is_none() {
                return Err(format!("line {}: unterminated solve block '{name}'", self.line()));
            }
            match self.peek_ident() {
                Some("relax") => {
                    self.pos += 1;
                    let rname = self.expect_ident()?;
                    if !self.eat_kw("init") {
                        return Err(format!(
                            "line {}: 'relax {rname}' needs an 'init <expr>' starting guess",
                            self.line()
                        ));
                    }
                    let init = self.expr()?;
                    relaxes.push(RelaxDecl { name: rname, init });
                }
                Some("tolerance") => {
                    self.pos += 1;
                    tolerance = Some(self.expr()?);
                }
                Some("max_iterations") => {
                    self.pos += 1;
                    max_iterations = self.expect_num()? as u32;
                }
                _ => members.push(self.measure(false)?),
            }
        }
        self.expect_sym("}")?;
        let form = match (relaxes.is_empty(), members.is_empty()) {
            (false, true) => SolveForm::Tearing(relaxes),
            (true, false) => SolveForm::Block(members),
            (false, false) => {
                return Err(format!(
                    "line {line}: solve '{name}' mixes 'relax' declarations with member measures — use one form"
                ))
            }
            (true, true) => return Err(format!("line {line}: solve '{name}' is empty")),
        };
        Ok(SolveDecl { name, tolerance, max_iterations, form, line })
    }

    fn assert_decl(&mut self) -> Result<AssertDecl, String> {
        let line = self.line();
        self.pos += 1; // 'assert'
        let name = self.expect_ident()?;
        let over = if self.eat_kw("over") { Some(self.expect_ident()?) } else { None };
        self.expect_sym(":")?;
        let lhs = self.expr()?;
        let op = if self.eat_sym("==") {
            CmpOp::Eq
        } else if self.eat_sym(">=") {
            CmpOp::Ge
        } else if self.eat_sym("<=") {
            CmpOp::Le
        } else {
            return Err(format!("line {}: expected '==', '>=' or '<=' in assert", self.line()));
        };
        let rhs = self.expr()?;
        let tol = if self.eat_sym("±") { Some(self.expr()?) } else { None };
        Ok(AssertDecl { name, over, lhs, op, rhs, tol, line })
    }

    // ---- expressions ---------------------------------------------------
    fn bound(&mut self) -> Result<Bound, String> {
        let base = self.expect_ident()?;
        if base == "t" {
            let k = self.bound_offset()?;
            return Ok(Bound::Rel(k));
        }
        self.expect_sym(".")?;
        let field = self.expect_ident()?;
        let k = self.bound_offset()?;
        match field.as_str() {
            "start" => Ok(Bound::RangeStart(base, k)),
            "end" => Ok(Bound::RangeEnd(base, k)),
            other => Err(format!(
                "line {}: expected '.start' or '.end' after '{base}', found '.{other}'",
                self.line()
            )),
        }
    }

    fn bound_offset(&mut self) -> Result<i32, String> {
        if self.eat_sym("+") {
            Ok(self.expect_num()? as i32)
        } else if self.eat_sym("-") {
            Ok(-(self.expect_num()? as i32))
        } else {
            Ok(0)
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.postfix_term()?;
        loop {
            if self.eat_sym("+") {
                let rhs = self.postfix_term()?;
                lhs = Expr::Bin(BinOp::Add, Box::new(lhs), Box::new(rhs));
            } else if self.eat_sym("-") {
                let rhs = self.postfix_term()?;
                lhs = Expr::Bin(BinOp::Sub, Box::new(lhs), Box::new(rhs));
            } else {
                return Ok(lhs);
            }
        }
    }

    /// A multiplicative term with optional postfix `when …` or `in <ccy> at <rate>`.
    fn postfix_term(&mut self) -> Result<Expr, String> {
        let mut t = self.term()?;
        loop {
            if self.peek_ident() == Some("when") {
                self.pos += 1;
                let f = self.expect_ident()?;
                let pos = match f.as_str() {
                    "first" => FirstLast::First,
                    "last" => FirstLast::Last,
                    other => {
                        return Err(format!(
                            "line {}: expected first(range) or last(range) after 'when', found '{other}'",
                            self.line()
                        ))
                    }
                };
                self.expect_sym("(")?;
                let range = self.expect_ident()?;
                self.expect_sym(")")?;
                t = Expr::When { value: Box::new(t), pos, range };
                continue;
            }
            // `in <currency> at <rate>` — only when the next two tokens
            // really are a unit name followed by 'at' (so that `match t {
            // in constr -> … }` arms are untouched).
            if self.peek_ident() == Some("in") {
                let unit_ahead = self
                    .ident_at(1)
                    .filter(|u| self.is_unit_name(u))
                    .map(|u| u.to_string());
                if let Some(target) = unit_ahead {
                    self.pos += 2; // 'in' <unit>
                    let rate = if self.eat_kw("at") {
                        Some(Box::new(self.term()?))
                    } else {
                        None // scale conversion (kEUR <-> EUR)
                    };
                    t = Expr::Conv { body: Box::new(t), target, rate };
                    continue;
                }
            }
            return Ok(t);
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut lhs = self.power()?;
        loop {
            if self.eat_sym("*") {
                let rhs = self.power()?;
                lhs = Expr::Bin(BinOp::Mul, Box::new(lhs), Box::new(rhs));
            } else if self.eat_sym("/") {
                let rhs = self.power()?;
                lhs = Expr::Bin(BinOp::Div, Box::new(lhs), Box::new(rhs));
            } else {
                return Ok(lhs);
            }
        }
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.factor()?;
        if self.eat_sym("^") {
            let exp = self.power()?;
            return Ok(Expr::Bin(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn factor(&mut self) -> Result<Expr, String> {
        if self.eat_sym("-") {
            return Ok(Expr::Neg(Box::new(self.factor()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::Sym("(")) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Some(Tok::Num(n)) => {
                self.pos += 1;
                if let Some(Tok::Ident(s)) = self.peek() {
                    if self.is_unit_name(s) {
                        let u = self.expect_ident()?;
                        return Ok(Expr::Qty(n, u));
                    }
                }
                Ok(Expr::Num(n))
            }
            Some(Tok::Pct(p)) => {
                self.pos += 1;
                Ok(Expr::Pct(p))
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                match name.as_str() {
                    "prev" => {
                        self.expect_sym("(")?;
                        let target = self.expect_ident()?;
                        self.expect_sym(")")?;
                        let init = if self.eat_kw("init") {
                            Some(Box::new(self.factor()?))
                        } else {
                            None
                        };
                        Ok(Expr::Prev(target, init))
                    }
                    "year" => {
                        self.expect_sym("(")?;
                        let arg = self.expect_ident()?;
                        if arg != "t" {
                            return Err(format!("line {}: year() takes 't'", self.line()));
                        }
                        self.expect_sym(")")?;
                        Ok(Expr::YearT)
                    }
                    "sum" => self.sum_expr(),
                    "npv" => {
                        self.expect_sym("(")?;
                        let rate = self.expr()?;
                        self.expect_sym(",")?;
                        let body = self.expr()?;
                        if !self.eat_kw("over") {
                            return Err(format!(
                                "line {}: npv needs 'npv(rate, expr over range)'",
                                self.line()
                            ));
                        }
                        let range = self.expect_ident()?;
                        self.expect_sym(")")?;
                        Ok(Expr::Npv { rate: Box::new(rate), body: Box::new(body), range })
                    }
                    "irr" => {
                        self.expect_sym("(")?;
                        let calendar = self.expect_ident()?;
                        self.expect_sym(",")?;
                        let target = self.expect_ident()?;
                        self.expect_sym(")")?;
                        Ok(Expr::Irr { calendar, name: target })
                    }
                    "annualize" => {
                        self.expect_sym("(")?;
                        let e = self.expr()?;
                        self.expect_sym(")")?;
                        Ok(Expr::Annualize(Box::new(e)))
                    }
                    "match" => self.match_expr(),
                    "actuals" => {
                        // actuals <measure> until <range> else <expr>
                        //   ≡ match t { in <range> -> <measure>,
                        //               in <calendar> \ <range> -> <expr> }
                        let source = self.expect_ident()?;
                        if !self.eat_kw("until") {
                            return Err(format!(
                                "line {}: expected 'until <range>' after 'actuals {source}'",
                                self.line()
                            ));
                        }
                        let boundary = self.expect_ident()?;
                        if !self.eat_kw("else") {
                            return Err(format!(
                                "line {}: expected 'else <forecast expr>' in actuals switchover",
                                self.line()
                            ));
                        }
                        let forecast = self.expr()?;
                        let cal = self.calendar_name.clone().ok_or_else(|| {
                            format!("line {}: 'actuals' needs the calendar declared first", self.line())
                        })?;
                        Ok(Expr::MatchT(vec![
                            (RangeSetRef { base: boundary.clone(), minus: None }, Expr::Ref(source)),
                            (RangeSetRef { base: cal, minus: Some(boundary) }, forecast),
                        ]))
                    }
                    "min" | "max" => {
                        self.expect_sym("(")?;
                        let mut args = vec![self.expr()?];
                        while self.eat_sym(",") {
                            args.push(self.expr()?);
                        }
                        self.expect_sym(")")?;
                        Ok(Expr::Call(name, args))
                    }
                    _ => {
                        if matches!(self.peek(), Some(Tok::Sym("("))) {
                            return Err(format!(
                                "line {}: unknown function '{name}' (builtins: prev, min, max, sum, npv, irr, annualize, year)",
                                self.line()
                            ));
                        }
                        if matches!(self.peek(), Some(Tok::Sym("["))) {
                            self.pos += 1;
                            // member index (possibly chained) or timeline bound
                            if let Some(Tok::Ident(s)) = self.peek() {
                                if self.is_member(s) {
                                    let mut members = vec![self.expect_ident()?];
                                    self.expect_sym("]")?;
                                    while matches!(self.peek(), Some(Tok::Sym("["))) {
                                        if let Some(Tok::Ident(s2)) = self.peek_at(1) {
                                            if self.is_member(s2) {
                                                self.pos += 1;
                                                members.push(self.expect_ident()?);
                                                self.expect_sym("]")?;
                                                continue;
                                            }
                                        }
                                        break;
                                    }
                                    return Ok(Expr::MemberIx { name, members });
                                }
                            }
                            let b = self.bound()?;
                            self.expect_sym("]")?;
                            return Ok(Expr::At { name, bound: b });
                        }
                        Ok(Expr::Ref(name))
                    }
                }
            }
            other => Err(format!(
                "line {}: expected an expression, found {}",
                self.line(),
                other.map(|t| t.to_string()).unwrap_or_else(|| "end of file".into())
            )),
        }
    }

    fn sum_expr(&mut self) -> Result<Expr, String> {
        if self.eat_sym("[") {
            let range = self.expect_ident()?;
            self.expect_sym("]")?;
            self.expect_sym("(")?;
            let body = self.expr()?;
            self.expect_sym(")")?;
            return Ok(Expr::RangeSum { range, body: Box::new(body) });
        }
        self.expect_sym("(")?;
        let name = self.expect_ident()?;
        self.expect_sym("[")?;
        let from = self.bound()?;
        self.expect_sym("..")?;
        let to = self.bound()?;
        self.expect_sym("]")?;
        self.expect_sym(")")?;
        Ok(Expr::WindowSum { name, from, to })
    }

    /// `match t { in r -> e, ... }` or `match <Dim> { Member -> e, else -> e }`.
    fn match_expr(&mut self) -> Result<Expr, String> {
        let arg = self.expect_ident()?;
        self.expect_sym("{")?;
        if arg == "t" {
            let mut arms = Vec::new();
            loop {
                if !self.eat_kw("in") {
                    return Err(format!("line {}: match arm must start with 'in <range>'", self.line()));
                }
                let base = self.expect_ident()?;
                let minus = if self.eat_sym("\\") { Some(self.expect_ident()?) } else { None };
                self.expect_sym("->")?;
                let e = self.expr()?;
                arms.push((RangeSetRef { base, minus }, e));
                let _ = self.eat_sym(",");
                if self.eat_sym("}") {
                    break;
                }
            }
            return Ok(Expr::MatchT(arms));
        }
        // dimension match
        let mut arms = Vec::new();
        let mut default = None;
        loop {
            let member = self.expect_ident()?;
            self.expect_sym("->")?;
            let e = self.expr()?;
            if member == "else" {
                default = Some(Box::new(e));
            } else {
                arms.push((member, e));
            }
            let _ = self.eat_sym(",");
            if self.eat_sym("}") {
                break;
            }
        }
        Ok(Expr::MatchDim { dim: arg, arms, default })
    }
}
