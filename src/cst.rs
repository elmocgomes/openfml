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
    let (_, spans) = Parser::parse_with_spans(parse_input)?;
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

    let mut root_children: Vec<GreenChild> = Vec::new();
    let mut pending: Vec<GreenToken> = Vec::new();
    let mut cur: Option<(usize, Vec<GreenChild>)> = None;
    let mut real_idx = 0usize;

    let close = |cur: &mut Option<(usize, Vec<GreenChild>)>,
                 pending: &mut Vec<GreenToken>,
                 out: &mut Vec<GreenChild>,
                 spans: &[(usize, usize, &'static str)]| {
        if let Some((di, mut ch)) = cur.take() {
            ch.extend(take_trailing(pending).into_iter().map(GreenChild::Token));
            out.push(GreenChild::Node(GreenNode::new(tag_kind(spans[di].2), ch)));
        }
    };

    for st in &full {
        let text = src[st.start..st.end].to_string();
        let token = GreenToken { kind: tok_kind(&st.tok), text };
        match &st.tok {
            Tok::Ws | Tok::Comment => pending.push(token),
            Tok::Directive => {
                // Inside a declaration's token range (e.g. between a solve
                // block's members) the directive stays inside it —
                // losslessness first, structure second. AFTER a completed
                // declaration it is a top-level node.
                let mid_decl = cur.as_ref().map(|(di, _)| real_idx < spans[*di].1).unwrap_or(false);
                if mid_decl {
                    let (_, ch) = cur.as_mut().expect("mid_decl implies open decl");
                    ch.extend(pending.drain(..).map(GreenChild::Token));
                    ch.push(GreenChild::Token(token));
                } else {
                    close(&mut cur, &mut pending, &mut root_children, &spans);
                    let mut ch: Vec<GreenChild> =
                        pending.drain(..).map(GreenChild::Token).collect();
                    ch.push(GreenChild::Token(token));
                    root_children
                        .push(GreenChild::Node(GreenNode::new(SyntaxKind::IncludeDirective, ch)));
                }
            }
            _ => {
                let di = decl_of[real_idx];
                real_idx += 1;
                match cur.as_mut() {
                    Some((cd, ch)) if *cd == di => {
                        ch.extend(pending.drain(..).map(GreenChild::Token));
                        ch.push(GreenChild::Token(token));
                    }
                    _ => {
                        close(&mut cur, &mut pending, &mut root_children, &spans);
                        let mut ch: Vec<GreenChild> =
                            pending.drain(..).map(GreenChild::Token).collect();
                        ch.push(GreenChild::Token(token));
                        cur = Some((di, ch));
                    }
                }
            }
        }
    }
    close(&mut cur, &mut pending, &mut root_children, &spans);
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
