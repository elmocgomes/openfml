//! Collaboration primitives: subspace ACLs and the event-sourced log.

use fml::server::{apply_event, replay, Acl, Event};
use fml::Session;

const BUDGET: &str = include_str!("../models/budget.fml");

#[test]
fn acl_subspace_authorization() {
    let acl = Acl::parse(
        "# owners\nalice: expenses[Marketing]\nbob: expenses[Engineering] expenses[Operations]\ncfo: *\n",
    )
    .unwrap();
    assert!(acl.authorize("alice", "expenses", Some("Marketing")));
    assert!(!acl.authorize("alice", "expenses", Some("Engineering")));
    assert!(!acl.authorize("alice", "budget_cap", None));
    assert!(acl.authorize("bob", "expenses", Some("Operations")));
    assert!(acl.authorize("cfo", "expenses", Some("Marketing")));
    assert!(acl.authorize("cfo", "budget_cap", None));
    assert!(!acl.authorize("mallory", "expenses", Some("Marketing")));
}

#[test]
fn event_log_replay_reproduces_state() {
    // Live session: two owners submit numbers.
    let mut live = Session::new(BUDGET).unwrap();
    live.run_full().unwrap();
    let events = vec![
        Event { seq: 1, user: "alice".into(), name: "expenses".into(), member: Some("Marketing".into()), period: Some(1), value: 480.0 },
        Event { seq: 2, user: "bob".into(), name: "expenses".into(), member: Some("Operations".into()), period: Some(0), value: 330.0 },
        Event { seq: 3, user: "cfo".into(), name: "budget_cap".into(), member: None, period: Some(1), value: 1_900.0 },
    ];
    let mut log = String::new();
    for ev in &events {
        apply_event(&mut live, ev).unwrap();
        log.push_str(&ev.to_line());
        log.push('\n');
    }

    // Cold boot: fresh session + replay must reproduce identical state.
    let mut cold = Session::new(BUDGET).unwrap();
    cold.run_full().unwrap();
    let last = replay(&mut cold, &log).unwrap();
    assert_eq!(last, 3);
    for (m, mi) in live.checked.measures.iter().enumerate() {
        for mb in 0..live.checked.tuple_count(m) {
            for (slot, (a, b)) in live.values[m][mb].iter().zip(cold.values[m][mb].iter()).enumerate() {
                let same = (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-9;
                assert!(same, "{}[{mb}][{slot}]: live {a} vs replayed {b}", mi.name);
            }
        }
    }
    // And the write-back sources agree byte-for-byte.
    assert_eq!(live.source(), cold.source());
    // Event round-trip through the wire format.
    for ev in &events {
        assert_eq!(&Event::from_line(&ev.to_line()).unwrap(), ev);
    }
}
