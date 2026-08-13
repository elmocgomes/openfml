//! The budget process: departments, roles, model-level read access, the
//! extended gate, and process state folded from the signed event log —
//! submit / reopen / lock are as tamper-evident as the numbers.

use openfml::server::{
    apply_event, gate, replay_signed, sign_line, Access, Action, Directory, Event, Process, Role,
    GENESIS,
};
use openfml::Session;

const USERS: &str = "\
# user: department role
alice: marketing   editor
bob:   engineering editor
carol: finance     admin
dave:  marketing   viewer
";

const ACCESS: &str = "\
model team_budget.fml
  departments marketing engineering operations finance
  write marketing:   marketing_spend
  write engineering: engineering_spend
  write operations:  operations_spend
  write finance:     *
model finance_only.fml
  departments finance
  write finance: *
";

fn setup() -> (Directory, Access) {
    (Directory::parse(USERS).unwrap(), Access::parse(ACCESS).unwrap())
}

#[test]
fn departments_restrict_model_access() {
    let (dir, acc) = setup();
    let alice = dir.find("alice").unwrap();
    let carol = dir.find("carol").unwrap();
    let shared = acc.model("team_budget.fml").unwrap();
    let private = acc.model("finance_only.fml").unwrap();
    assert!(shared.can_read(alice));
    assert!(!private.can_read(alice), "marketing cannot open the finance-only model");
    assert!(private.can_read(carol), "admins read everything");
    assert_eq!(dir.find("alice").unwrap().role, Role::Editor);
}

#[test]
fn roles_shape_effective_grants() {
    let (dir, acc) = setup();
    let ma = acc.model("team_budget.fml").unwrap();
    let g = |name: &str| ma.effective_grants(dir.find(name).unwrap());
    assert_eq!(g("alice").len(), 1);
    assert_eq!(g("alice")[0].measure, "marketing_spend");
    assert!(g("dave").is_empty(), "viewers hold no write grants");
    assert_eq!(g("carol")[0].measure, "*", "admins hold the wildcard");
}

#[test]
fn the_gate_enforces_role_process_and_grants() {
    let (dir, acc) = setup();
    let ma = acc.model("team_budget.fml").unwrap();
    let mut p = Process::default();
    let (alice, bob, carol, dave) = (
        dir.find("alice").unwrap(),
        dir.find("bob").unwrap(),
        dir.find("carol").unwrap(),
        dir.find("dave").unwrap(),
    );
    let patch = |m: &'static str| Action::Patch { measure: m, member: None };
    // Grants by department.
    assert!(gate(alice, ma, &p, &patch("marketing_spend")).is_ok());
    assert!(gate(alice, ma, &p, &patch("engineering_spend")).is_err(), "not her department");
    assert!(gate(dave, ma, &p, &patch("marketing_spend")).is_err(), "viewers are read-only");
    // Formulas are admin-only — editors alter INPUTS only.
    assert!(gate(alice, ma, &p, &Action::Formula).is_err());
    assert!(gate(carol, ma, &p, &Action::Formula).is_ok());
    // Submit freezes the department (admins pass through).
    p.submitted.insert("marketing".into());
    let e = gate(alice, ma, &p, &patch("marketing_spend")).unwrap_err();
    assert!(e.contains("submitted"), "{e}");
    assert!(gate(bob, ma, &p, &patch("engineering_spend")).is_ok(), "other departments continue");
    assert!(gate(carol, ma, &p, &patch("marketing_spend")).is_ok(), "admins may adjust");
    // Process control is admin-only.
    assert!(gate(alice, ma, &p, &Action::Lock).is_err());
    assert!(gate(alice, ma, &p, &Action::Reopen { dept: "marketing" }).is_err());
    assert!(gate(carol, ma, &p, &Action::Reopen { dept: "marketing" }).is_ok());
    // Lock stops everything except admin reopen.
    p.locked = true;
    assert!(gate(carol, ma, &p, &patch("marketing_spend")).is_err(), "locked is locked");
    assert!(gate(carol, ma, &p, &Action::Formula).is_err());
    assert!(gate(carol, ma, &p, &Action::Reopen { dept: "marketing" }).is_ok(), "reopen unlocks");
}

const MODEL: &str = "model demo.proc\ncalendar plan = yearly 2026 .. 2027\ncurrency EUR\n\
input spend : EUR flow over plan = 100\ntotal : EUR flow over plan = spend * 2\n";

#[test]
fn process_and_formula_events_replay_from_the_chain() {
    let secret = b"round-secret";
    let events = vec![
        Event::patch(1, "alice", "spend", None, None, 120.0),
        Event { seq: 2, user: "alice".into(), kind: "submit".into(), name: "marketing".into(), member: None, period: None, value: 0.0, text: None, ts: 0 },
        Event { seq: 3, user: "carol".into(), kind: "formula".into(), name: "total".into(), member: None, period: None, value: 0.0, text: Some("spend * 2 + 10".into()), ts: 0 },
        Event { seq: 4, user: "carol".into(), kind: "reopen".into(), name: "marketing".into(), member: None, period: None, value: 0.0, text: None, ts: 0 },
        Event { seq: 5, user: "alice".into(), kind: "patch".into(), name: "spend".into(), member: None, period: None, value: 130.0, text: None, ts: 0 },
        Event { seq: 6, user: "carol".into(), kind: "lock".into(), name: "-".into(), member: None, period: None, value: 0.0, text: None, ts: 0 },
    ];
    let mut log = String::new();
    let mut prev = GENESIS.to_string();
    for ev in &events {
        let line = ev.to_line();
        let sig = sign_line(secret, &prev, &line);
        log.push_str(&format!("{line}\t{sig}\n"));
        prev = sig;
    }
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    let mut p = Process::default();
    let (last, _) = replay_signed(&mut s, &mut p, &log, secret).unwrap();
    assert_eq!(last, 6);
    // Values reflect patches AND the admin's formula change.
    assert_eq!(s.get("total", None, Some(0)).unwrap(), 130.0 * 2.0 + 10.0);
    assert!(s.source().contains("= spend * 2 + 10"), "formula landed in the source");
    // Process state reflects the full history: submitted → reopened → locked.
    assert!(!p.submitted.contains("marketing"), "reopen cleared the submission");
    assert!(p.locked, "the final lock holds after replay");
    // Tampering with a PROCESS event breaks the chain like any number.
    let tampered = log.replace("\tlock\t", "\treopen\t");
    let mut s2 = Session::new(MODEL).unwrap();
    s2.run_full().unwrap();
    let err = replay_signed(&mut s2, &mut Process::default(), &tampered, secret).unwrap_err();
    assert!(err.contains("signature mismatch"), "{err}");
}

#[test]
fn formula_events_round_trip_escaped_bodies() {
    let ev = Event {
        seq: 7,
        user: "carol".into(),
        kind: "formula".into(),
        name: "total".into(),
        member: None,
        period: None,
        value: 0.0,
        text: Some("match t {\n  in plan -> spend\t* 2\n}".into()),
        ts: 1_770_000_000,
    };
    let back = Event::from_line(&ev.to_line()).unwrap();
    assert_eq!(back, ev, "tabs and newlines survive the log line");
}

#[test]
fn legacy_six_field_lines_still_parse_as_patches() {
    let ev = Event::from_line("4\talice\texpenses\tMarketing\t1\t480").unwrap();
    assert_eq!(ev.kind, "patch");
    assert_eq!(ev.member.as_deref(), Some("Marketing"));
    assert_eq!(ev.value, 480.0);
}

#[test]
fn editors_cannot_reach_formulas_even_through_patch() {
    // The structural guarantee behind "input-only": /patch can only land
    // on literal INPUT sites — a computed measure refuses.
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    let mut p = Process::default();
    let ev = Event::patch(1, "alice", "total", None, Some(0), 999.0);
    let err = apply_event(&mut s, &mut p, &ev).unwrap_err();
    assert!(err.contains("not literal-editable") || err.contains("not an input"), "{err}");
}
