//! The lossless concrete syntax tree — slice 1 of the CST plan.
//!
//! A **green tree** of position-independent nodes whose tokens carry the
//! exact source bytes (comments, whitespace, `1_700` spellings, include
//! directives), plus a lazy **red cursor** that computes absolute offsets
//! on the way down. The defining property, tested over every model in the
//! repo: `reprint(parse_cst(text)) == text`, byte for byte.
//!
//! Granularity in this slice is the top-level declaration: the root's
//! children are declaration nodes (each owning its leading trivia, so a
//! comment above a measure moves with it), assembled from the trivia
//! lexer and the parser's recorded declaration boundaries — no separate
//! grammar, no drift. Structural edits (replace/remove/insert a
//! declaration) rebuild only the root spine; every untouched declaration
//! is shared by reference.

use crate::lexer::{lex_full, Tok};
use crate::parser::Parser;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    StrLit,
    DefDecl,
    // Tokens.
    Whitespace,
    Comment,
    Ident,
    Num,
    Pct,
    Sym,
    Directive,
    // Nodes.
    Root,
    ModelHeader,
    CalendarDecl,
    PeriodDecl,
    DimensionDecl,
    FunctionalDecl,
    CurrencyDecl,
    UnitsDecl,
    InputDecl,
    MeasureDecl,
    AllocateDecl,
    SolveDecl,
    AssertDecl,
    ScenarioDecl,
    EliminateDecl,
    CorrelateDecl,
    IncludeDirective,
    /// A declaration that failed to parse — the file's CST still builds
    /// and reprints losslessly; only this node is semantically opaque.
    ErrorDecl,
    /// The right-hand side of a measure/input declaration (after `=`).
    Body,
    /// One `period: value` entry of a map literal.
    MapEntry,
    /// One `Member -> …` arm of an input `match Dim { … }` body.
    MatchArm,
}

fn tag_kind(tag: &str) -> SyntaxKind {
    match tag {
        "model" => SyntaxKind::ModelHeader,
        "calendar" => SyntaxKind::CalendarDecl,
        "period" => SyntaxKind::PeriodDecl,
        "dimension" => SyntaxKind::DimensionDecl,
        "functional" => SyntaxKind::FunctionalDecl,
        "currency" => SyntaxKind::CurrencyDecl,
        "unit" => SyntaxKind::UnitsDecl,
        "input" => SyntaxKind::InputDecl,
        "solve" => SyntaxKind::SolveDecl,
        "assert" => SyntaxKind::AssertDecl,
        "scenario" => SyntaxKind::ScenarioDecl,
        "eliminate" => SyntaxKind::EliminateDecl,
        "correlate" => SyntaxKind::CorrelateDecl,
        "allocate" => SyntaxKind::AllocateDecl,
        "def" => SyntaxKind::DefDecl,
        "error" => SyntaxKind::ErrorDecl,
        _ => SyntaxKind::MeasureDecl,
    }
}

fn tok_kind(t: &Tok) -> SyntaxKind {
    match t {
        Tok::Ws => SyntaxKind::Whitespace,
        Tok::Comment => SyntaxKind::Comment,
        Tok::Directive => SyntaxKind::Directive,
        Tok::Ident(_) => SyntaxKind::Ident,
        Tok::Num(_) => SyntaxKind::Num,
        Tok::Pct(_) => SyntaxKind::Pct,
        Tok::Sym(_) => SyntaxKind::Sym,
        Tok::Str(_) => SyntaxKind::StrLit,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GreenToken {
    pub kind: SyntaxKind,
    /// The exact source bytes — `1_700` stays `1_700`.
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub struct GreenNode {
    pub kind: SyntaxKind,
    /// Total byte width of this subtree (cached at construction).
    pub width: usize,
    pub children: Vec<GreenChild>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GreenChild {
    Node(Rc<GreenNode>),
    Token(GreenToken),
}

impl GreenChild {
    pub fn width(&self) -> usize {
        match self {
            GreenChild::Node(n) => n.width,
            GreenChild::Token(t) => t.text.len(),
        }
    }
}

impl GreenNode {
    pub fn new(kind: SyntaxKind, children: Vec<GreenChild>) -> Rc<GreenNode> {
        let width = children.iter().map(|c| c.width()).sum();
        Rc::new(GreenNode { kind, width, children })
    }

    pub fn reprint(&self, out: &mut String) {
        for c in &self.children {
            match c {
                GreenChild::Node(n) => n.reprint(out),
                GreenChild::Token(t) => out.push_str(&t.text),
            }
        }
    }

    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.width);
        self.reprint(&mut s);
        s
    }

    /// A new node with child `i` replaced — every other child is shared.
    pub fn with_child_replaced(&self, i: usize, new: GreenChild) -> Rc<GreenNode> {
        let mut ch = self.children.clone();
        ch[i] = new;
        GreenNode::new(self.kind, ch)
    }

    pub fn with_child_removed(&self, i: usize) -> Rc<GreenNode> {
        let mut ch = self.children.clone();
        ch.remove(i);
        GreenNode::new(self.kind, ch)
    }

    pub fn with_child_inserted(&self, i: usize, new: GreenChild) -> Rc<GreenNode> {
        let mut ch = self.children.clone();
        ch.insert(i, new);
        GreenNode::new(self.kind, ch)
    }

    /// Every token of this subtree in order, with absolute offsets
    /// (given this node's own offset) — nesting flattened away.
    pub fn flat_tokens<'a>(&'a self, base: usize, out: &mut Vec<(SyntaxKind, &'a str, usize)>) {
        let mut off = base;
        for c in &self.children {
            match c {
                GreenChild::Node(n) => n.flat_tokens(off, out),
                GreenChild::Token(t) => out.push((t.kind, t.text.as_str(), off)),
            }
            off += c.width();
        }
    }
}

/// True for token kinds that carry no meaning (whitespace, comments).
pub fn is_trivia_kind(k: SyntaxKind) -> bool {
    matches!(k, SyntaxKind::Whitespace | SyntaxKind::Comment)
}

/// The semantic fingerprint of a subtree: an FNV-1a hash over its kind
/// and every non-trivia token — two declarations with equal fingerprints
/// mean the SAME code, whatever the formatting or comments around it.
/// This is the early-cutoff key of the incremental (salsa-style) reload.
pub fn semantic_fingerprint(n: &GreenNode) -> u64 {
    fn fnv(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
    }
    fn go(n: &GreenNode, h: &mut u64) {
        fnv(h, &[n.kind as u8, 0xfe]);
        for c in &n.children {
            match c {
                GreenChild::Node(inner) => go(inner, h),
                GreenChild::Token(t) => {
                    if !is_trivia_kind(t.kind) {
                        fnv(h, &[t.kind as u8]);
                        fnv(h, t.text.as_bytes());
                        fnv(h, &[0xff]);
                    }
                }
            }
        }
    }
    let mut h = 0xcbf29ce484222325u64;
    go(n, &mut h);
    h
}

/// The canonical name of a declaration node, if it has one: the measure /
/// input / dimension / … name (the first identifier after the leading
/// keyword; measures have no keyword, so their first identifier).
pub fn decl_name(n: &GreenNode) -> Option<String> {
    let mut idents = n.children.iter().filter_map(|c| match c {
        GreenChild::Token(t) if t.kind == SyntaxKind::Ident => Some(t.text.clone()),
        _ => None,
    });
    match n.kind {
        SyntaxKind::MeasureDecl => idents.next(),
        SyntaxKind::ModelHeader
        | SyntaxKind::CalendarDecl
        | SyntaxKind::PeriodDecl
        | SyntaxKind::DimensionDecl
        | SyntaxKind::CurrencyDecl
        | SyntaxKind::UnitsDecl
        | SyntaxKind::InputDecl
        | SyntaxKind::AllocateDecl
        | SyntaxKind::SolveDecl
        | SyntaxKind::AssertDecl
        | SyntaxKind::ScenarioDecl
        | SyntaxKind::DefDecl
        | SyntaxKind::EliminateDecl => idents.nth(1),
        _ => None,
    }
}

// ---- the red layer: absolute offsets, computed on the way down ----------

#[derive(Clone, Copy)]
pub struct Red<'a> {
    pub green: &'a GreenNode,
    pub offset: usize,
}

pub enum RedChild<'a> {
    Node(Red<'a>),
    Token { kind: SyntaxKind, text: &'a str, offset: usize },
}

impl<'a> Red<'a> {
    pub fn root(green: &'a GreenNode) -> Red<'a> {
        Red { green, offset: 0 }
    }

    pub fn range(&self) -> (usize, usize) {
        (self.offset, self.offset + self.green.width)
    }

    pub fn children(&self) -> Vec<RedChild<'a>> {
        let mut off = self.offset;
        let mut out = Vec::with_capacity(self.green.children.len());
        for c in &self.green.children {
            match c {
                GreenChild::Node(n) => out.push(RedChild::Node(Red { green: n, offset: off })),
                GreenChild::Token(t) => {
                    out.push(RedChild::Token { kind: t.kind, text: &t.text, offset: off })
                }
            }
            off += c.width();
        }
        out
    }

    /// The child declaration nodes (skipping root-level trivia tokens).
    pub fn decls(&self) -> Vec<Red<'a>> {
        self.children()
            .into_iter()
            .filter_map(|c| match c {
                RedChild::Node(n) => Some(n),
                RedChild::Token { .. } => None,
            })
            .collect()
    }

    /// The declaration node containing the byte offset, if any.
    pub fn decl_at(&self, offset: usize) -> Option<Red<'a>> {
        self.decls().into_iter().find(|d| {
            let (s, e) = d.range();
            offset >= s && offset < e
        })
    }
}

fn sub_kind(tag: &str) -> SyntaxKind {
    match tag {
        "entry" => SyntaxKind::MapEntry,
        "arm" => SyntaxKind::MatchArm,
        _ => SyntaxKind::Body,
    }
}

/// Lex a replacement fragment into green tokens (raw byte slices).
pub fn lex_green_tokens(text: &str) -> Result<Vec<GreenToken>, String> {
    Ok(lex_full(text)?
        .into_iter()
        .map(|st| GreenToken { kind: tok_kind(&st.tok), text: text[st.start..st.end].to_string() })
        .collect())
}

/// Replace token children `first..=last` of the node at `path` (child
/// indices from the root) with `reps`, rebuilding only the spine — every
/// untouched sibling subtree is shared by reference.
pub fn replace_tokens_at(
    root: &Rc<GreenNode>,
    path: &[usize],
    first: usize,
    last: usize,
    reps: Vec<GreenToken>,
) -> Result<Rc<GreenNode>, String> {
    if path.is_empty() {
        let mut ch = root.children.clone();
        if last >= ch.len() || first > last {
            return Err("replace_tokens_at: token range out of bounds".into());
        }
        ch.splice(first..=last, reps.into_iter().map(GreenChild::Token));
        return Ok(GreenNode::new(root.kind, ch));
    }
    let GreenChild::Node(inner) = &root.children[path[0]] else {
        return Err("replace_tokens_at: path descends into a token".into());
    };
    let new_inner = replace_tokens_at(inner, &path[1..], first, last, reps)?;
    Ok(root.with_child_replaced(path[0], GreenChild::Node(new_inner)))
}

// ---- the builder ---------------------------------------------------------

/// Parse a source file into its lossless CST. The tree reprints to the
/// input byte-for-byte (checked here — a width mismatch is a bug, never a
/// silent loss). `include "…"` lines become IncludeDirective nodes.
pub fn parse_cst(src: &str) -> Result<Rc<GreenNode>, String> {
    let full = lex_full(src)?;
    // The parser sees the directive-stripped view (same real tokens).
    let has_directives = full.iter().any(|t| t.tok == Tok::Directive);
    let parser_src: String;
    let parse_input: &str = if has_directives {
        parser_src = src
            .lines()
            .map(|l| if l.trim_start().starts_with("include ") { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        &parser_src
    } else {
        src
    };
    // Resilient: broken declarations become ErrorDecl nodes — the CST
    // exists (and reprints losslessly) even while the file is mid-edit.
    let (spans, subs) = Parser::parse_tree_spans(parse_input)?;
    // Map each real (trivia-filtered) token index to its declaration.
    let n_real = full.iter().filter(|t| !t.tok.is_trivia() && t.tok != Tok::Directive).count();
    let mut decl_of = vec![usize::MAX; n_real];
    for (di, (s, e, _)) in spans.iter().enumerate() {
        for slot in decl_of.iter_mut().take(*e).skip(*s) {
            *slot = di;
        }
    }

    // Same-line trailing trivia stays with the PREVIOUS declaration: the
    // pending prefix up to the first newline-bearing whitespace token.
    fn take_trailing(pending: &mut Vec<GreenToken>) -> Vec<GreenToken> {
        let cut = pending
            .iter()
            .position(|t| t.kind == SyntaxKind::Whitespace && t.text.contains('\n'))
            .unwrap_or(pending.len());
        pending.drain(..cut).collect()
    }

    // Nest a declaration's flat (real-index, token) list into Body /
    // MapEntry / MatchArm nodes per the parser's sub-spans (which nest by
    // containment; sorted start-asc, end-desc so outer nodes open first).
    fn nest(tokens: Vec<(Option<usize>, GreenToken)>, subs: &[(usize, usize, SyntaxKind)]) -> Vec<GreenChild> {
        let mut order: Vec<&(usize, usize, SyntaxKind)> = subs.iter().collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut it = order.into_iter().peekable();
        let mut stack: Vec<(usize, SyntaxKind, Vec<GreenChild>)> = Vec::new();
        let mut top: Vec<GreenChild> = Vec::new();
        for (ridx, tok) in tokens {
            if let Some(r) = ridx {
                while let Some((end, _, _)) = stack.last() {
                    if *end <= r {
                        let (_, kind, ch) = stack.pop().expect("checked");
                        let node = GreenChild::Node(GreenNode::new(kind, ch));
                        match stack.last_mut() {
                            Some((_, _, parent)) => parent.push(node),
                            None => top.push(node),
                        }
                    } else {
                        break;
                    }
                }
                while let Some((s0, e0, k)) = it.peek() {
                    if *s0 == r {
                        stack.push((*e0, *k, Vec::new()));
                        it.next();
                    } else {
                        break;
                    }
                }
            }
            match stack.last_mut() {
                Some((_, _, ch)) => ch.push(GreenChild::Token(tok)),
                None => top.push(GreenChild::Token(tok)),
            }
        }
        while let Some((_, kind, ch)) = stack.pop() {
            let node = GreenChild::Node(GreenNode::new(kind, ch));
            match stack.last_mut() {
                Some((_, _, parent)) => parent.push(node),
                None => top.push(node),
            }
        }
        top
    }

    let mut root_children: Vec<GreenChild> = Vec::new();
    let mut pending: Vec<GreenToken> = Vec::new();
    // (decl id, flat (real-idx, token) list)
    let mut cur: Option<(usize, Vec<(Option<usize>, GreenToken)>)> = None;
    let mut real_idx = 0usize;

    let close = |cur: &mut Option<(usize, Vec<(Option<usize>, GreenToken)>)>,
                 pending: &mut Vec<GreenToken>,
                 out: &mut Vec<GreenChild>,
                 spans: &[(usize, usize, &'static str)],
                 subs: &[(usize, usize, &'static str)]| {
        if let Some((di, mut toks)) = cur.take() {
            toks.extend(take_trailing(pending).into_iter().map(|t| (None, t)));
            let (ds, de, tag) = spans[di];
            let my_subs: Vec<(usize, usize, SyntaxKind)> = subs
                .iter()
                .filter(|(s0, e0, _)| *s0 >= ds && *e0 <= de)
                .map(|(s0, e0, t)| (*s0, *e0, sub_kind(t)))
                .collect();
            out.push(GreenChild::Node(GreenNode::new(tag_kind(tag), nest(toks, &my_subs))));
        }
    };

    for st in &full {
        let text = src[st.start..st.end].to_string();
        let token = GreenToken { kind: tok_kind(&st.tok), text };
        match &st.tok {
            Tok::Ws | Tok::Comment => pending.push(token),
            Tok::Directive => {
                let mid_decl = cur.as_ref().map(|(di, _)| real_idx < spans[*di].1).unwrap_or(false);
                if mid_decl {
                    let (_, toks) = cur.as_mut().expect("mid_decl implies open decl");
                    toks.extend(pending.drain(..).map(|t| (None, t)));
                    toks.push((None, token));
                } else {
                    close(&mut cur, &mut pending, &mut root_children, &spans, &subs);
                    let mut ch: Vec<GreenChild> =
                        pending.drain(..).map(GreenChild::Token).collect();
                    ch.push(GreenChild::Token(token));
                    root_children
                        .push(GreenChild::Node(GreenNode::new(SyntaxKind::IncludeDirective, ch)));
                }
            }
            _ => {
                let di = decl_of[real_idx];
                let r = real_idx;
                real_idx += 1;
                match cur.as_mut() {
                    Some((cd, toks)) if *cd == di => {
                        toks.extend(pending.drain(..).map(|t| (None, t)));
                        toks.push((Some(r), token));
                    }
                    _ => {
                        close(&mut cur, &mut pending, &mut root_children, &spans, &subs);
                        let mut toks: Vec<(Option<usize>, GreenToken)> =
                            pending.drain(..).map(|t| (None, t)).collect();
                        toks.push((Some(r), token));
                        cur = Some((di, toks));
                    }
                }
            }
        }
    }
    close(&mut cur, &mut pending, &mut root_children, &spans, &subs);
    // Trailing file trivia lives at root level.
    root_children.extend(pending.drain(..).map(GreenChild::Token));

    let root = GreenNode::new(SyntaxKind::Root, root_children);
    if root.width != src.len() {
        return Err(format!(
            "internal: CST width {} != source length {} — losslessness violated",
            root.width,
            src.len()
        ));
    }
    Ok(root)
}
