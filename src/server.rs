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
    /// Wall-clock unix seconds, stamped by the server at append time
    /// (0 = unknown/legacy). Audit metadata — not part of the semantics.
    pub ts: u64,
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
        Event { seq, user: user.into(), kind: "patch".into(), name: name.into(), member, period, value, text: None, ts: 0 }
    }

    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.seq,
            self.user,
            self.kind,
            self.name,
            self.member.as_deref().unwrap_or("-"),
            self.period.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            self.value,
            self.text.as_deref().map(esc_field).unwrap_or_else(|| "-".into()),
            self.ts
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
                ts: 0,
            }),
            8 | 9 => Ok(Event {
                seq: parts[0].parse().map_err(|_| "bad seq")?,
                user: parts[1].to_string(),
                kind: parts[2].to_string(),
                name: parts[3].to_string(),
                member: if parts[4] == "-" { None } else { Some(parts[4].to_string()) },
                period: if parts[5] == "-" { None } else { Some(parts[5].parse().map_err(|_| "bad period")?) },
                value: parts[6].parse().map_err(|_| "bad value")?,
                text: if parts[7] == "-" { None } else { Some(unesc_field(parts[7])) },
                ts: parts.get(8).and_then(|t| t.parse().ok()).unwrap_or(0),
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

// ============================================================================
// The user store: CRUD-able accounts and ROLE OBJECTS, persisted as
// users.json in the config directory. Replaces the declarative users.cfg
// (which is auto-migrated on first boot). Passwords are salted iterated
// SHA-256; login exchanges credentials for the same HMAC tokens the API
// has always used. Privilege separation: a role's BASE (admin | editor |
// viewer) drives the model/process gate exactly as before, while the
// separate manage_users capability governs the user-management module —
// the seeded Super Admin holds it; ordinary finance admins do not.
// ============================================================================

#[derive(Clone, Debug)]
pub struct RoleDef {
    pub name: String,
    pub base: Role,
    pub manage_users: bool,
    /// Built-in roles (superadmin/admin/editor/viewer) cannot be edited
    /// or deleted.
    pub builtin: bool,
}

#[derive(Clone, Debug)]
pub struct Account {
    pub name: String,
    pub dept: String,
    /// Role NAME — resolved against the role table.
    pub role: String,
    /// "<salt hex>$<hash hex>"; None = no password set (token-only user).
    pub pass: Option<String>,
    pub must_change: bool,
}

pub struct UserStore {
    pub roles: Vec<RoleDef>,
    pub accounts: Vec<Account>,
    path: std::path::PathBuf,
}

fn base_from_str(s: &str) -> Option<Role> {
    match s {
        "admin" => Some(Role::Admin),
        "editor" => Some(Role::Editor),
        "viewer" => Some(Role::Viewer),
        _ => None,
    }
}

fn base_to_str(r: Role) -> &'static str {
    match r {
        Role::Admin => "admin",
        Role::Editor => "editor",
        Role::Viewer => "viewer",
    }
}

impl UserStore {
    fn builtin_roles() -> Vec<RoleDef> {
        vec![
            RoleDef { name: "superadmin".into(), base: Role::Admin, manage_users: true, builtin: true },
            RoleDef { name: "admin".into(), base: Role::Admin, manage_users: false, builtin: true },
            RoleDef { name: "editor".into(), base: Role::Editor, manage_users: false, builtin: true },
            RoleDef { name: "viewer".into(), base: Role::Viewer, manage_users: false, builtin: true },
        ]
    }

    /// Open (or create) the store: users.json if present; else migrate
    /// users.cfg; always guarantee a Super Admin exists. Returns the
    /// initial Super Admin password when one was just seeded.
    pub fn open(dir: &std::path::Path) -> Result<(UserStore, Option<(String, String)>), String> {
        let path = dir.join("users.json");
        let mut store = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| format!("read users.json: {e}"))?;
            Self::from_json(&raw, path.clone())?
        } else {
            let mut s = UserStore { roles: Self::builtin_roles(), accounts: Vec::new(), path: path.clone() };
            if let Ok(cfg) = std::fs::read_to_string(dir.join("users.cfg")) {
                let legacy = Directory::parse(&cfg)?;
                for u in legacy.users {
                    s.accounts.push(Account {
                        name: u.name,
                        dept: u.dept,
                        role: base_to_str(u.role).to_string(),
                        pass: None,
                        must_change: false,
                    });
                }
            }
            s
        };
        // Guarantee a Super Admin.
        let mut seeded = None;
        let has_super = store.accounts.iter().any(|a| store.can_manage_by_role(&a.role));
        if !has_super {
            let name = if store.find("admin").is_none() { "admin" } else { "superadmin" }.to_string();
            let pw = crate::crypto::hex(&crate::crypto::random_bytes16())[..16].to_string();
            let salt = crate::crypto::random_bytes16();
            store.accounts.push(Account {
                name: name.clone(),
                dept: "finance".into(),
                role: "superadmin".into(),
                pass: Some(crate::crypto::hash_password(&pw, &salt)),
                must_change: true,
            });
            seeded = Some((name, pw));
        }
        store.save()?;
        Ok((store, seeded))
    }

    fn from_json(raw: &str, path: std::path::PathBuf) -> Result<UserStore, String> {
        use crate::json::J;
        let j = crate::json::parse(raw)?;
        let s = |v: &J| -> String {
            match v { J::S(x) => x.clone(), _ => String::new() }
        };
        let b = |v: Option<&J>| matches!(v, Some(J::B(true)));
        let mut roles = Vec::new();
        if let Some(J::A(rs)) = j.get("roles") {
            for r in rs {
                let name = r.get("name").map(&s).unwrap_or_default();
                let base = base_from_str(&r.get("base").map(&s).unwrap_or_default())
                    .ok_or_else(|| format!("role '{name}': bad base"))?;
                roles.push(RoleDef {
                    name,
                    base,
                    manage_users: b(r.get("manage_users")),
                    builtin: b(r.get("builtin")),
                });
            }
        }
        // Built-ins always present and canonical.
        for bi in Self::builtin_roles() {
            match roles.iter_mut().find(|r| r.name == bi.name) {
                Some(slot) => *slot = bi,
                None => roles.push(bi),
            }
        }
        let mut accounts = Vec::new();
        if let Some(J::A(us)) = j.get("users") {
            for u in us {
                accounts.push(Account {
                    name: u.get("name").map(&s).unwrap_or_default(),
                    dept: u.get("dept").map(&s).unwrap_or_default(),
                    role: u.get("role").map(&s).unwrap_or_else(|| "viewer".into()),
                    pass: u.get("pass").map(&s).filter(|x| !x.is_empty()),
                    must_change: b(u.get("must_change")),
                });
            }
        }
        Ok(UserStore { roles, accounts, path })
    }

    pub fn save(&self) -> Result<(), String> {
        use crate::json::J;
        let roles = J::A(self.roles.iter().map(|r| J::O(vec![
            ("name".into(), J::S(r.name.clone())),
            ("base".into(), J::S(base_to_str(r.base).into())),
            ("manage_users".into(), J::B(r.manage_users)),
            ("builtin".into(), J::B(r.builtin)),
        ])).collect());
        let users = J::A(self.accounts.iter().map(|a| J::O(vec![
            ("name".into(), J::S(a.name.clone())),
            ("dept".into(), J::S(a.dept.clone())),
            ("role".into(), J::S(a.role.clone())),
            ("pass".into(), a.pass.clone().map_or(J::Null, J::S)),
            ("must_change".into(), J::B(a.must_change)),
        ])).collect());
        let doc = J::O(vec![("roles".into(), roles), ("users".into(), users)]).dump();
        std::fs::write(&self.path, doc).map_err(|e| format!("write users.json: {e}"))
    }

    pub fn find(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    pub fn role_def(&self, role: &str) -> Option<&RoleDef> {
        self.roles.iter().find(|r| r.name == role)
    }

    fn can_manage_by_role(&self, role: &str) -> bool {
        self.role_def(role).map(|r| r.manage_users).unwrap_or(false)
    }

    /// The legacy User the gate/grants machinery consumes.
    pub fn as_gate_user(&self, a: &Account) -> User {
        User {
            name: a.name.clone(),
            dept: a.dept.clone(),
            role: self.role_def(&a.role).map(|r| r.base).unwrap_or(Role::Viewer),
        }
    }

    pub fn can_manage(&self, a: &Account) -> bool {
        self.can_manage_by_role(&a.role)
    }

    pub fn login(&self, name: &str, pass: &str) -> bool {
        self.find(name)
            .and_then(|a| a.pass.as_deref())
            .map(|stored| crate::crypto::verify_password(pass, stored))
            .unwrap_or(false)
    }

    fn superadmin_count(&self) -> usize {
        self.accounts.iter().filter(|a| self.can_manage_by_role(&a.role)).count()
    }

    // ---- CRUD (validated; call save() after Ok) ------------------------
    pub fn create_user(&mut self, name: &str, dept: &str, role: &str, pass: Option<&str>) -> Result<(), String> {
        let ok = !name.is_empty() && name.len() <= 40
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
        if !ok {
            return Err("user names: letters, digits, _ - . (max 40)".into());
        }
        if self.find(name).is_some() {
            return Err(format!("user '{name}' already exists"));
        }
        if self.role_def(role).is_none() {
            return Err(format!("unknown role '{role}'"));
        }
        let hashed = pass.filter(|p| !p.is_empty()).map(|p| {
            crate::crypto::hash_password(p, &crate::crypto::random_bytes16())
        });
        self.accounts.push(Account {
            name: name.into(),
            dept: dept.into(),
            role: role.into(),
            pass: hashed,
            must_change: true,
        });
        Ok(())
    }

    pub fn update_user(
        &mut self,
        name: &str,
        dept: Option<&str>,
        role: Option<&str>,
        pass: Option<&str>,
    ) -> Result<(), String> {
        if let Some(r) = role {
            if self.role_def(r).is_none() {
                return Err(format!("unknown role '{r}'"));
            }
            // Demoting the LAST Super Admin would lock user management.
            let was_super = self.find(name).map(|a| self.can_manage_by_role(&a.role)).unwrap_or(false);
            if was_super && !self.can_manage_by_role(r) && self.superadmin_count() == 1 {
                return Err("cannot demote the last Super Admin".into());
            }
        }
        let a = self.accounts.iter_mut().find(|a| a.name == name)
            .ok_or_else(|| format!("unknown user '{name}'"))?;
        if let Some(d) = dept { a.dept = d.into(); }
        if let Some(r) = role { a.role = r.into(); }
        if let Some(p) = pass.filter(|p| !p.is_empty()) {
            a.pass = Some(crate::crypto::hash_password(p, &crate::crypto::random_bytes16()));
            a.must_change = true;
        }
        Ok(())
    }

    pub fn delete_user(&mut self, name: &str, actor: &str) -> Result<(), String> {
        if name == actor {
            return Err("you cannot delete yourself".into());
        }
        let idx = self.accounts.iter().position(|a| a.name == name)
            .ok_or_else(|| format!("unknown user '{name}'"))?;
        if self.can_manage_by_role(&self.accounts[idx].role) && self.superadmin_count() == 1 {
            return Err("cannot delete the last Super Admin".into());
        }
        self.accounts.remove(idx);
        Ok(())
    }

    pub fn set_own_password(&mut self, name: &str, old: &str, new: &str) -> Result<(), String> {
        if new.len() < 8 {
            return Err("passwords need at least 8 characters".into());
        }
        let a = self.accounts.iter().find(|a| a.name == name).ok_or("unknown user")?;
        if let Some(stored) = a.pass.as_deref() {
            if !crate::crypto::verify_password(old, stored) {
                return Err("current password is wrong".into());
            }
        }
        let a = self.accounts.iter_mut().find(|a| a.name == name).unwrap();
        a.pass = Some(crate::crypto::hash_password(new, &crate::crypto::random_bytes16()));
        a.must_change = false;
        Ok(())
    }

    pub fn create_role(&mut self, name: &str, base: &str, manage_users: bool) -> Result<(), String> {
        let ok = !name.is_empty() && name.len() <= 40
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err("role names: letters, digits, _ - (max 40)".into());
        }
        if self.role_def(name).is_some() {
            return Err(format!("role '{name}' already exists"));
        }
        let base = base_from_str(base).ok_or("base must be admin | editor | viewer")?;
        self.roles.push(RoleDef { name: name.into(), base, manage_users, builtin: false });
        Ok(())
    }

    pub fn update_role(&mut self, name: &str, base: Option<&str>, manage_users: Option<bool>) -> Result<(), String> {
        let r = self.roles.iter_mut().find(|r| r.name == name)
            .ok_or_else(|| format!("unknown role '{name}'"))?;
        if r.builtin {
            return Err(format!("'{name}' is a built-in role and cannot be edited"));
        }
        if let Some(b) = base {
            r.base = base_from_str(b).ok_or("base must be admin | editor | viewer")?;
        }
        if let Some(m) = manage_users {
            r.manage_users = m;
        }
        Ok(())
    }

    pub fn delete_role(&mut self, name: &str) -> Result<(), String> {
        let r = self.role_def(name).ok_or_else(|| format!("unknown role '{name}'"))?;
        if r.builtin {
            return Err(format!("'{name}' is a built-in role and cannot be deleted"));
        }
        if let Some(u) = self.accounts.iter().find(|a| a.role == name) {
            return Err(format!("role '{name}' is assigned to '{}' — reassign first", u.name));
        }
        self.roles.retain(|x| x.name != name);
        Ok(())
    }
}
