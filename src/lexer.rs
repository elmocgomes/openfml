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
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "{s}"),
            Tok::Num(n) => write!(f, "{n}"),
            Tok::Pct(p) => write!(f, "{}%", p * 100.0),
            Tok::Sym(s) => write!(f, "{s}"),
        }
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
            '\n' => {
                line += 1;
                i += 1;
            }
            ' ' | '\t' | '\r' => i += 1,
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
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
    }
    Ok(out)
}
