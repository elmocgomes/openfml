//! Minimal zero-dependency JSON: a recursive-descent parser and a
//! serializer over one value enum — enough for the LSP's JSON-RPC.

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    B(bool),
    N(f64),
    S(String),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::O(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `get("a.b.c")` — dotted path lookup.
    pub fn path(&self, path: &str) -> Option<&J> {
        let mut cur = self;
        for part in path.split('.') {
            cur = cur.get(part)?;
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::S(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::N(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[J]> {
        match self {
            J::A(a) => Some(a),
            _ => None,
        }
    }

    pub fn obj(fields: Vec<(&str, J)>) -> J {
        J::O(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn s(v: &str) -> J {
        J::S(v.to_string())
    }

    pub fn n(v: f64) -> J {
        J::N(v)
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::B(b) => out.push_str(if *b { "true" } else { "false" }),
            J::N(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{n}"));
                }
            }
            J::S(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::A(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::O(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    J::S(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn parse(src: &str) -> Result<J, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let v = value(&chars, &mut pos)?;
    skip_ws(&chars, &mut pos);
    if pos != chars.len() {
        return Err(format!("trailing content at {pos}"));
    }
    Ok(v)
}

fn skip_ws(c: &[char], p: &mut usize) {
    while *p < c.len() && matches!(c[*p], ' ' | '\t' | '\n' | '\r') {
        *p += 1;
    }
}

fn value(c: &[char], p: &mut usize) -> Result<J, String> {
    skip_ws(c, p);
    match c.get(*p) {
        Some('{') => {
            *p += 1;
            let mut fields = Vec::new();
            skip_ws(c, p);
            if c.get(*p) == Some(&'}') {
                *p += 1;
                return Ok(J::O(fields));
            }
            loop {
                skip_ws(c, p);
                let J::S(key) = value(c, p)? else { return Err("object key must be a string".into()) };
                skip_ws(c, p);
                if c.get(*p) != Some(&':') {
                    return Err("expected ':'".into());
                }
                *p += 1;
                let v = value(c, p)?;
                fields.push((key, v));
                skip_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some('}') => {
                        *p += 1;
                        return Ok(J::O(fields));
                    }
                    _ => return Err("expected ',' or '}'".into()),
                }
            }
        }
        Some('[') => {
            *p += 1;
            let mut arr = Vec::new();
            skip_ws(c, p);
            if c.get(*p) == Some(&']') {
                *p += 1;
                return Ok(J::A(arr));
            }
            loop {
                arr.push(value(c, p)?);
                skip_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some(']') => {
                        *p += 1;
                        return Ok(J::A(arr));
                    }
                    _ => return Err("expected ',' or ']'".into()),
                }
            }
        }
        Some('"') => {
            *p += 1;
            let mut s = String::new();
            while let Some(&ch) = c.get(*p) {
                *p += 1;
                match ch {
                    '"' => return Ok(J::S(s)),
                    '\\' => {
                        let esc = c.get(*p).copied().ok_or("bad escape")?;
                        *p += 1;
                        match esc {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'u' => {
                                let hex: String = c.get(*p..*p + 4).ok_or("bad \\u")?.iter().collect();
                                *p += 4;
                                let mut cp = u32::from_str_radix(&hex, 16).map_err(|_| "bad \\u")?;
                                // Surrogate pair.
                                if (0xD800..0xDC00).contains(&cp)
                                    && c.get(*p) == Some(&'\\')
                                    && c.get(*p + 1) == Some(&'u')
                                {
                                    let hex2: String =
                                        c.get(*p + 2..*p + 6).ok_or("bad \\u")?.iter().collect();
                                    let lo = u32::from_str_radix(&hex2, 16).map_err(|_| "bad \\u")?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        *p += 6;
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    }
                                }
                                s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                            other => s.push(other),
                        }
                    }
                    other => s.push(other),
                }
            }
            Err("unterminated string".into())
        }
        Some('t') if c[*p..].starts_with(&['t', 'r', 'u', 'e']) => {
            *p += 4;
            Ok(J::B(true))
        }
        Some('f') if c[*p..].starts_with(&['f', 'a', 'l', 's', 'e']) => {
            *p += 5;
            Ok(J::B(false))
        }
        Some('n') if c[*p..].starts_with(&['n', 'u', 'l', 'l']) => {
            *p += 4;
            Ok(J::Null)
        }
        Some(ch) if ch.is_ascii_digit() || *ch == '-' => {
            let start = *p;
            while *p < c.len()
                && matches!(c[*p], '0'..='9' | '-' | '+' | '.' | 'e' | 'E')
            {
                *p += 1;
            }
            let text: String = c[start..*p].iter().collect();
            text.parse::<f64>().map(J::N).map_err(|_| format!("bad number '{text}'"))
        }
        other => Err(format!("unexpected {other:?}")),
    }
}
