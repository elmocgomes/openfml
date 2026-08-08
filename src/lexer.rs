//! Hand-rolled lexer for the Phase-1 .fml subset. Zero dependencies.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Num(f64),
    /// `15%` stored as 0.15.
    Pct(f64),
    /// Single- and multi-char symbols: : = { } ( ) [ ] , + - * / . .. == ±
    Sym(&'static str),
    /// Trivia (lex_full only): a run of whitespace.
    Ws,
    /// Trivia (lex_full only): a `// …` comment (newline not included).
    Comment,
    /// A whole `include "…"` line (newline not included) — handled by the
    /// include expander, opaque to the parser.
    Directive,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "{s}"),
            Tok::Num(n) => write!(f, "{n}"),
            Tok::Pct(p) => write!(f, "{}%", p * 100.0),
            Tok::Sym(s) => write!(f, "{s}"),
            Tok::Ws => write!(f, " "),
            Tok::Comment => write!(f, "//"),
            Tok::Directive => write!(f, "include"),
        }
    }
}

impl Tok {
    pub fn is_trivia(&self) -> bool {
        matches!(self, Tok::Ws | Tok::Comment)
    }
}

#[derive(Clone, Debug)]
pub struct SpannedTok {
    pub tok: Tok,
    pub line: usize,
    /// Byte offsets into the source — the basis for lossless span patching.
    pub start: usize,
    pub end: usize,
}

pub fn lex(src: &str) -> Result<Vec<SpannedTok>, String> {
    let toks = lex_full(src)?;
    if toks.iter().any(|t| t.tok == Tok::Directive) {
        return Err("include directives must be expanded before compiling — resolve includes first".into());
    }
    Ok(toks.into_iter().filter(|t| !t.tok.is_trivia()).collect())
}

/// Lossless lexing: every byte of the source lands in exactly one token —
/// whitespace runs and comments become trivia tokens, whole `include "…"`
/// lines become directives. The CST is assembled from this stream; the
/// parser consumes the trivia-filtered view (`lex`).
pub fn lex_full(src: &str) -> Result<Vec<SpannedTok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    // Byte offset of each char index (chars may be multi-byte in comments).
    let mut bpos = Vec::with_capacity(chars.len() + 1);
    let mut acc = 0usize;
    for c in &chars {
        bpos.push(acc);
        acc += c.len_utf8();
    }
    bpos.push(acc);
    let mut i = 0usize;
    let mut line = 1usize;
    // Has this line seen a non-trivia token yet? (Directives are only
    // recognized as the first thing on a line, mirroring expand_includes.)
    let mut line_has_real = false;
    while i < chars.len() {
        let c = chars[i];
        let tstart = i;
        // Emit with byte span [tstart, i) — call only after advancing `i`.
        macro_rules! emit {
            ($tok:expr) => {
                out.push(SpannedTok { tok: $tok, line, start: bpos[tstart], end: bpos[i] })
            };
        }
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                while i < chars.len() && matches!(chars[i], ' ' | '\t' | '\r' | '\n') {
                    if chars[i] == '\n' {
                        line += 1;
                        line_has_real = false;
                    }
                    i += 1;
                }
                emit!(Tok::Ws);
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                emit!(Tok::Comment);
            }
            'i' if !line_has_real
                && chars[i..].starts_with(&['i', 'n', 'c', 'l', 'u', 'd', 'e'])
                && chars.get(i + 7).map(|c| *c == ' ' || *c == '\t').unwrap_or(false) =>
            {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                line_has_real = true;
                emit!(Tok::Directive);
            }
            c if c.is_ascii_digit() => {
                let start = i;
                let mut text = String::new();
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || chars[i] == '_' || chars[i] == '.')
                {
                    // `..` range operator must not be swallowed by a number
                    if chars[i] == '.' && i + 1 < chars.len() && chars[i + 1] == '.' {
                        break;
                    }
                    if chars[i] != '_' {
                        text.push(chars[i]);
                    }
                    i += 1;
                }
                let n: f64 = text
                    .parse()
                    .map_err(|_| format!("line {line}: bad number starting at '{}'", &src[start..(start + 8).min(src.len())]))?;
                if i < chars.len() && chars[i] == '%' {
                    i += 1;
                    emit!(Tok::Pct(n / 100.0));
                } else {
                    emit!(Tok::Num(n));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut text = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    text.push(chars[i]);
                    i += 1;
                }
                emit!(Tok::Ident(text));
            }
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    i += 2;
                    emit!(Tok::Sym(".."));
                } else {
                    i += 1;
                    emit!(Tok::Sym("."));
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    emit!(Tok::Sym("=="));
                } else {
                    i += 1;
                    emit!(Tok::Sym("="));
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    emit!(Tok::Sym(">="));
                } else {
                    return Err(format!("line {line}: '>' is only valid as '>='"));
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                    emit!(Tok::Sym("<="));
                } else {
                    return Err(format!("line {line}: '<' is only valid as '<='"));
                }
            }
            '\\' => {
                i += 1;
                emit!(Tok::Sym("\\"));
            }
            '^' => {
                i += 1;
                emit!(Tok::Sym("^"));
            }
            '-' if i + 1 < chars.len() && chars[i + 1] == '>' => {
                i += 2;
                emit!(Tok::Sym("->"));
            }
            '±' => {
                i += 1;
                emit!(Tok::Sym("±"));
            }
            '~' => {
                i += 1;
                emit!(Tok::Sym("~"));
            }
            ':' | '{' | '}' | '(' | ')' | '[' | ']' | ',' | '+' | '-' | '*' | '/' => {
                let s: &'static str = match c {
                    ':' => ":",
                    '{' => "{",
                    '}' => "}",
                    '(' => "(",
                    ')' => ")",
                    '[' => "[",
                    ']' => "]",
                    ',' => ",",
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    _ => unreachable!(),
                };
                i += 1;
                emit!(Tok::Sym(s));
            }
            other => return Err(format!("line {line}: unexpected character '{other}'")),
        }
        if out.last().map(|t| !t.tok.is_trivia()).unwrap_or(false) {
            line_has_real = true;
        }
    }
    Ok(out)
}
