//! Collaboration primitives (design doc 07 §3): dimension-subspace write
//! ACLs and an append-only event log, with replay. The server binary is a
//! thin HTTP shell over these; every mutation passes through ONE gate —
//! `authorize` then `apply_event` — identically for humans and machines.
//!
//! Trust model (v2): identity is a bearer token `user.<hmac>` minted from
//! the server secret — "alice" is cryptography, not a claim. The event
//! log is a hash chain: each event is signed over the previous signature,
//! so any edit, deletion or reorder of history breaks replay.

use crate::crypto::{ct_eq, hex, hmac_sha256};
use crate::live::Session;
use std::collections::HashSet;

// ---- the people: departments and roles ----------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Finance/HQ: formulas, process control, every model, every measure.
    Admin,
    /// May alter the INPUTS granted to their department (and nothing
    /// structural — /patch reaches only input literals by construction).
    Editor,
    /// Read-only.
    Viewer,
}

#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub dept: String,
    pub role: Role,
}

#[derive(Clone, Debug, Default)]
pub struct Directory {
    pub users: Vec<User>,
}

impl Directory {
    /// Parse users.cfg:
    /// ```text
    /// # user: department role
    /// alice: marketing editor
    /// carol: finance   admin
    /// dave:  marketing viewer
    /// ```
    pub fn parse(src: &str) -> Result<Directory, String> {
        let mut users = Vec::new();
        for (ln, line) in src.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, rest) = line
                .split_once(':')
                .ok_or_else(|| format!("users line {}: expected 'user: dept role'", ln + 1))?;
            let mut parts = rest.split_whitespace();
            let dept = parts.next().ok_or_else(|| format!("users line {}: missing department", ln + 1))?;
            let role = match parts.next() {
                Some("admin") => Role::Admin,
                Some("editor") => Role::Editor,
                Some("viewer") => Role::Viewer,
                other => return Err(format!("users line {}: role must be admin|editor|viewer, got {other:?}", ln + 1)),
            };
            users.push(User { name: name.trim().to_string(), dept: dept.to_string(), role });
        }
        Ok(Directory { users })
    }

    pub fn find(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name == name)
    }
}

// ---- the models: department read access + write grants ------------------

#[derive(Clone, Debug)]
pub struct ModelAccess {
    pub file: String,
    /// Departments that may READ this model at all.
    pub departments: Vec<String>,
    /// (department, grants) — what each department's EDITORS may write.
    pub write: Vec<(String, Vec<Grant>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Access {
    pub models: Vec<ModelAccess>,
}

impl Access {
    /// Parse access.cfg:
    /// ```text
    /// model team_budget.fml
    ///   departments marketing engineering operations finance
    ///   write marketing:   marketing_spend
    ///   write engineering: engineering_spend
    ///   write finance:     *
    /// ```
    pub fn parse(src: &str) -> Result<Access, String> {
        let mut models: Vec<ModelAccess> = Vec::new();
        for (ln, raw) in src.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(file) = line.strip_prefix("model ") {
                models.push(ModelAccess { file: file.trim().to_string(), departments: Vec::new(), write: Vec::new() });
            } else if let Some(rest) = line.strip_prefix("departments ") {
                let m = models.last_mut().ok_or_else(|| format!("access line {}: 'departments' before 'model'", ln + 1))?;
                m.departments = rest.split_whitespace().map(String::from).collect();
            } else if let Some(rest) = line.strip_prefix("write ") {
                let m = models.last_mut().ok_or_else(|| format!("access line {}: 'write' before 'model'", ln + 1))?;
                let (dept, gs) = rest
                    .split_once(':')
                    .ok_or_else(|| format!("access line {}: expected 'write dept: grants'", ln + 1))?;
                let mut grants = Vec::new();
                for g in gs.split_whitespace() {
                    if g == "*" {
                        grants.push(Grant { measure: "*".into(), member: None });
                    } else if let Some((mm, rest2)) = g.split_once('[') {
                        let member = rest2
                            .strip_suffix(']')
                            .ok_or_else(|| format!("access line {}: bad grant '{g}'", ln + 1))?;
                        grants.push(Grant { measure: mm.to_string(), member: Some(member.to_string()) });
                    } else {
                        grants.push(Grant { measure: g.to_string(), member: None });
                    }
                }
                m.write.push((dept.trim().to_string(), grants));
            } else {
                return Err(format!("access line {}: unrecognized '{line}'", ln + 1));
            }
        }
        Ok(Access { models })
    }

    pub fn model(&self, file: &str) -> Option<&ModelAccess> {
        self.models.iter().find(|m| m.file == file)
    }
}

impl ModelAccess {
    pub fn can_read(&self, u: &User) -> bool {
        u.role == Role::Admin || self.departments.iter().any(|d| *d == u.dept)
    }

    /// The grants a user actually holds here: admins everything, editors
    /// their department's write list, viewers nothing.
    pub fn effective_grants(&self, u: &User) -> Vec<Grant> {
        match u.role {
            Role::Admin => vec![Grant { measure: "*".into(), member: None }],
            Role::Viewer => Vec::new(),
            Role::Editor => self
                .write
                .iter()
                .find(|(d, _)| *d == u.dept)
                .map(|(_, g)| g.clone())
                .unwrap_or_default(),
        }
    }
}

// ---- the budget round: process state folded from signed events ----------

#[derive(Clone, Debug, Default)]
pub struct Process {
    /// Departments that have SUBMITTED their numbers this round.
    pub submitted: HashSet<String>,
    /// A locked budget accepts no further writes (final).
    pub locked: bool,
}

/// A write-side action, gated as one unit.
pub enum Action<'a> {
    Patch { measure: &'a str, member: Option<&'a str> },
    Formula,
    Submit,
    Reopen { dept: &'a str },
    Lock,
}

/// THE gate, extended for the budget process: role → process state →
/// grants. (Read access is checked by the caller via `can_read`.)
pub fn gate(u: &User, ma: &ModelAccess, proc_: &Process, action: &Action) -> Result<(), String> {
    match action {
        Action::Patch { measure, member } => {
            if u.role == Role::Viewer {
                return Err(format!("{} is a viewer — read-only", u.name));
            }
            if proc_.locked {
                return Err("the budget is locked".into());
            }
            if u.role != Role::Admin && proc_.submitted.contains(&u.dept) {
                return Err(format!(
                    "{} has submitted — ask finance to reopen the round",
                    u.dept
                ));
            }
            let grants = ma.effective_grants(u);
            let ok = grants.iter().any(|g| {
                g.measure == "*"
                    || (g.measure == *measure
                        && match (&g.member, member) {
                            (None, _) => true,
                            (Some(gm), Some(m)) => gm == m,
                            (Some(_), None) => false,
                        })
            });
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "{} may not write {}{}",
                    u.name,
                    measure,
                    member.map(|m| format!("[{m}]")).unwrap_or_default()
                ))
            }
        }
        Action::Formula => {
            if u.role != Role::Admin {
                return Err(format!("{}: only finance admins may change formulas", u.name));
            }
            if proc_.locked {
                return Err("the budget is locked".into());
            }
            Ok(())
        }
        Action::Submit => {
            if u.role == Role::Viewer {
                return Err(format!("{} is a viewer — read-only", u.name));
            }
            if proc_.locked {
                return Err("the budget is locked".into());
            }
            Ok(())
        }
        Action::Reopen { .. } | Action::Lock => {
            if u.role != Role::Admin {
                return Err(format!("{}: only finance admins control the round", u.name));
            }
            Ok(())
        }
    }
}

/// Mint a bearer token for `user`: `user.<hmac(secret, "tok:user")>`.
pub fn make_token(secret: &[u8], user: &str) -> String {
    format!("{user}.{}", hex(&hmac_sha256(secret, format!("tok:{user}").as_bytes())))
}

/// Verify a token and return the authenticated user (constant-time MAC
/// comparison). None = forged, malformed, or wrong secret.
pub fn verify_token(secret: &[u8], token: &str) -> Option<String> {
    let (user, mac) = token.split_once('.')?;
    if user.is_empty() {
        return None;
    }
    let want = hex(&hmac_sha256(secret, format!("tok:{user}").as_bytes()));
    if ct_eq(mac.as_bytes(), want.as_bytes()) {
        Some(user.to_string())
    } else {
        None
    }
}

/// The chain tag of the empty log.
pub const GENESIS: &str = "genesis";

/// Signature for an event line given the previous link of the chain.
pub fn sign_line(secret: &[u8], prev_sig: &str, line: &str) -> String {
    hex(&hmac_sha256(secret, format!("{prev_sig}\n{line}").as_bytes()))
}

/// One grant: a measure, optionally narrowed to a dimension member.
/// `measure == "*"` grants everything.
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub measure: String,
    pub member: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Acl {
    /// (user, grants)
    pub users: Vec<(String, Vec<Grant>)>,
}

impl Acl {
    /// Parse the owners file:
    /// ```text
    /// # comments
    /// alice: expenses[Marketing]
    /// bob:   expenses[Engineering] expenses[Operations]
    /// cfo:   *
    /// ```
    pub fn parse(src: &str) -> Result<Acl, String> {
        let mut users = Vec::new();
        for (ln, line) in src.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (user, rest) = line
                .split_once(':')
                .ok_or_else(|| format!("owners line {}: expected 'user: grants'", ln + 1))?;
            let mut grants = Vec::new();
            for g in rest.split_whitespace() {
                if g == "*" {
                    grants.push(Grant { measure: "*".into(), member: None });
                } else if let Some((m, rest)) = g.split_once('[') {
                    let member = rest
                        .strip_suffix(']')
                        .ok_or_else(|| format!("owners line {}: bad grant '{g}'", ln + 1))?;
                    grants.push(Grant { measure: m.to_string(), member: Some(member.to_string()) });
                } else {
                    grants.push(Grant { measure: g.to_string(), member: None });
                }
            }
            if grants.is_empty() {
                return Err(format!("owners line {}: user '{}' has no grants", ln + 1, user.trim()));
            }
            users.push((user.trim().to_string(), grants));
        }
        Ok(Acl { users })
    }

    /// May `user` write (measure, member)? A measure-level grant covers all
    /// members; a member-level grant covers exactly that member.
    pub fn authorize(&self, user: &str, measure: &str, member: Option<&str>) -> bool {
        let Some((_, grants)) = self.users.iter().find(|(u, _)| u == user) else {
            return false;
        };
        grants.iter().any(|g| {
            if g.measure == "*" {
                return true;
            }
            if g.measure != measure {
                return false;
            }
            match (&g.member, member) {
                (None, _) => true,
                (Some(gm), Some(m)) => gm == m,
                (Some(_), None) => false,
            }
        })
    }
}

/// One committed event: a value patch, a formula change, or a budget
/// process transition (submit / reopen / lock) — all through the same
/// signed log, so process history is as tamper-evident as the numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub user: String,
    /// "patch" | "formula" | "submit" | "reopen" | "lock".
    pub kind: String,
    /// Measure (patch/formula) or department (submit/reopen); "-" unused.
    pub name: String,
    pub member: Option<String>,
    pub period: Option<usize>,
    pub value: f64,
    /// Formula body for kind "formula" (escaped in the log line).
    pub text: Option<String>,
}

fn esc_field(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn unesc_field(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Event {
    pub fn patch(seq: u64, user: &str, name: &str, member: Option<String>, period: Option<usize>, value: f64) -> Event {
        Event { seq, user: user.into(), kind: "patch".into(), name: name.into(), member, period, value, text: None }
    }

    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.seq,
            self.user,
            self.kind,
            self.name,
            self.member.as_deref().unwrap_or("-"),
            self.period.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            self.value,
            self.text.as_deref().map(esc_field).unwrap_or_else(|| "-".into())
        )
    }

    pub fn from_line(line: &str) -> Result<Event, String> {
        let parts: Vec<&str> = line.split('\t').collect();
        // Legacy 6-field lines are patches; current lines carry 8 fields.
        match parts.len() {
            6 => Ok(Event {
                seq: parts[0].parse().map_err(|_| "bad seq")?,
                user: parts[1].to_string(),
                kind: "patch".into(),
                name: parts[2].to_string(),
                member: if parts[3] == "-" { None } else { Some(parts[3].to_string()) },
                period: if parts[4] == "-" { None } else { Some(parts[4].parse().map_err(|_| "bad period")?) },
                value: parts[5].parse().map_err(|_| "bad value")?,
                text: None,
            }),
            8 => Ok(Event {
                seq: parts[0].parse().map_err(|_| "bad seq")?,
                user: parts[1].to_string(),
                kind: parts[2].to_string(),
                name: parts[3].to_string(),
                member: if parts[4] == "-" { None } else { Some(parts[4].to_string()) },
                period: if parts[5] == "-" { None } else { Some(parts[5].parse().map_err(|_| "bad period")?) },
                value: parts[6].parse().map_err(|_| "bad value")?,
                text: if parts[7] == "-" { None } else { Some(unesc_field(parts[7])) },
            }),
            _ => Err(format!("bad event line: {line}")),
        }
    }
}

/// Apply a formula change: rewrite the owning file, re-expand, reload.
pub fn apply_formula(session: &mut Session, name: &str, body: &str) -> Result<(), String> {
    let files = session.replace_formula(name, body)?;
    let exp = crate::expand_includes_with_map(&files[0].0, &files[0].1, &mut |p| {
        files
            .iter()
            .find(|(n, _)| n == p)
            .map(|(_, t)| t.clone())
            .ok_or_else(|| format!("missing include \"{p}\""))
    })?;
    session.reload(exp)?;
    Ok(())
}

/// Apply one event: patches and formula changes mutate the session,
/// process events fold into the round state. The log is authoritative:
/// state = model files + replayed log.
pub fn apply_event(session: &mut Session, proc_: &mut Process, ev: &Event) -> Result<(), String> {
    match ev.kind.as_str() {
        "patch" => {
            session.patch_input(&ev.name, ev.member.as_deref(), ev.period, ev.value)?;
            session.recalc()?;
            Ok(())
        }
        "formula" => {
            let body = ev.text.as_deref().ok_or("formula event without a body")?;
            apply_formula(session, &ev.name, body)
        }
        "submit" => {
            proc_.submitted.insert(ev.name.clone());
            Ok(())
        }
        "reopen" => {
            proc_.submitted.remove(&ev.name);
            proc_.locked = false;
            Ok(())
        }
        "lock" => {
            proc_.locked = true;
            Ok(())
        }
        other => Err(format!("unknown event kind '{other}'")),
    }
}

/// Replay a SIGNED log, verifying the hash chain link by link. Returns
/// (last seq, chain tip). A signature mismatch means the log has been
/// modified after the fact — the server must refuse to serve from it.
/// (Tail truncation alone is not detectable by the chain; it loses the
/// newest commits but cannot forge or reorder history.)
pub fn replay_signed(
    session: &mut Session,
    proc_: &mut Process,
    log: &str,
    secret: &[u8],
) -> Result<(u64, String), String> {
    let mut prev = GENESIS.to_string();
    let mut last = 0;
    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((evpart, sig)) = line.rsplit_once('\t') else {
            return Err("event log has unsigned lines — archive it and start a fresh log".into());
        };
        // The final field must be a 64-hex chain signature; anything else
        // is an unsigned (pre-authentication) line.
        if sig.len() != 64 || !sig.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(
                "event log has unsigned lines — it predates authentication; archive it and start a fresh log"
                    .into(),
            );
        }
        let want = sign_line(secret, &prev, evpart);
        if !ct_eq(sig.as_bytes(), want.as_bytes()) {
            let seq = evpart.split('\t').next().unwrap_or("?");
            return Err(format!(
                "event {seq}: signature mismatch — the log has been modified (or the secret changed); refusing to serve"
            ));
        }
        let ev = Event::from_line(evpart)?;
        apply_event(session, proc_, &ev).map_err(|e| format!("replaying event {}: {e}", ev.seq))?;
        last = ev.seq;
        prev = sig.to_string();
    }
    Ok((last, prev))
}

/// Replay a log (e.g. on boot) in order. Fails loudly on a corrupt line —
/// an event log that cannot replay is a real incident, not a warning.
pub fn replay(session: &mut Session, log: &str) -> Result<u64, String> {
    let mut proc_ = Process::default();
    let mut last = 0;
    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let ev = Event::from_line(line)?;
        apply_event(session, &mut proc_, &ev).map_err(|e| format!("replaying event {}: {e}", ev.seq))?;
        last = ev.seq;
    }
    Ok(last)
}
