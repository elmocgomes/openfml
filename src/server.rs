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

/// One committed edit. Serialized one-per-line in the append-only log.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub user: String,
    pub name: String,
    pub member: Option<String>,
    pub period: Option<usize>,
    pub value: f64,
}

impl Event {
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            self.seq,
            self.user,
            self.name,
            self.member.as_deref().unwrap_or("-"),
            self.period.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            self.value
        )
    }

    pub fn from_line(line: &str) -> Result<Event, String> {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 6 {
            return Err(format!("bad event line: {line}"));
        }
        Ok(Event {
            seq: parts[0].parse().map_err(|_| "bad seq")?,
            user: parts[1].to_string(),
            name: parts[2].to_string(),
            member: if parts[3] == "-" { None } else { Some(parts[3].to_string()) },
            period: if parts[4] == "-" {
                None
            } else {
                Some(parts[4].parse().map_err(|_| "bad period")?)
            },
            value: parts[5].parse().map_err(|_| "bad value")?,
        })
    }
}

/// Apply one event through the normal patch path (source write-back +
/// incremental recalc). The event log is authoritative: state = model file
/// + replayed log.
pub fn apply_event(session: &mut Session, ev: &Event) -> Result<(), String> {
    session.patch_input(&ev.name, ev.member.as_deref(), ev.period, ev.value)?;
    session.recalc()?;
    Ok(())
}

/// Replay a SIGNED log, verifying the hash chain link by link. Returns
/// (last seq, chain tip). A signature mismatch means the log has been
/// modified after the fact — the server must refuse to serve from it.
/// (Tail truncation alone is not detectable by the chain; it loses the
/// newest commits but cannot forge or reorder history.)
pub fn replay_signed(session: &mut Session, log: &str, secret: &[u8]) -> Result<(u64, String), String> {
    let mut prev = GENESIS.to_string();
    let mut last = 0;
    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.matches('\t').count() != 6 {
            return Err(
                "event log has unsigned lines — it predates authentication; archive it and start a fresh log"
                    .into(),
            );
        }
        let (evpart, sig) = line.rsplit_once('\t').expect("7 fields checked");
        let want = sign_line(secret, &prev, evpart);
        if !ct_eq(sig.as_bytes(), want.as_bytes()) {
            let seq = evpart.split('\t').next().unwrap_or("?");
            return Err(format!(
                "event {seq}: signature mismatch — the log has been modified (or the secret changed); refusing to serve"
            ));
        }
        let ev = Event::from_line(evpart)?;
        apply_event(session, &ev).map_err(|e| format!("replaying event {}: {e}", ev.seq))?;
        last = ev.seq;
        prev = sig.to_string();
    }
    Ok((last, prev))
}

/// Replay a log (e.g. on boot) in order. Fails loudly on a corrupt line —
/// an event log that cannot replay is a real incident, not a warning.
pub fn replay(session: &mut Session, log: &str) -> Result<u64, String> {
    let mut last = 0;
    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let ev = Event::from_line(line)?;
        apply_event(session, &ev).map_err(|e| format!("replaying event {}: {e}", ev.seq))?;
        last = ev.seq;
    }
    Ok(last)
}
