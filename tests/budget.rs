//! Cost-center budget template: per-member editable map arms, member-aware
//! grid→text write-back, and rollup covenants.

use fml::Session;

const BUDGET: &str = include_str!("../models/budget.fml");

#[test]
fn member_patch_edits_only_that_arm() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let before_eng = s.get("expenses", Some("Engineering"), Some(1)).unwrap();

    // Marketing's owner bumps 2027 in their own arm.
    s.patch_input("expenses", Some("Marketing"), Some(1), 480.0).unwrap();
    s.recalc().unwrap();

    // Source: only Marketing's arm changed.
    assert!(s.source().contains("2027: 480"), "src: {}", s.source());
    assert!(s.source().contains("2027: 990"), "Engineering untouched");
    // Values: Marketing changed, Engineering untouched, total re-rolled.
    assert_eq!(s.get("expenses", Some("Marketing"), Some(1)).unwrap(), 480.0);
    assert_eq!(s.get("expenses", Some("Engineering"), Some(1)).unwrap(), before_eng);
    assert_eq!(s.get("total_expenses", None, Some(1)).unwrap(), 480.0 + 990.0 + 315.0);

    // Round-trip: fresh compile of the patched source agrees.
    let mut fresh = Session::new(s.source()).unwrap();
    fresh.run_full().unwrap();
    assert_eq!(
        fresh.get("total_expenses", None, Some(1)).unwrap(),
        s.get("total_expenses", None, Some(1)).unwrap()
    );
}

#[test]
fn broadcast_arm_patch_changes_all_periods_of_that_member() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    // Operations is a broadcast literal arm: one edit changes every period,
    // but only for Operations.
    s.patch_input("expenses", Some("Operations"), Some(2), 330.0).unwrap();
    s.recalc().unwrap();
    assert!(s.source().contains("Operations  -> 330"), "src: {}", s.source());
    for t in 0..4 {
        assert_eq!(s.get("expenses", Some("Operations"), Some(t)).unwrap(), 330.0);
    }
    assert_eq!(s.get("expenses", Some("Marketing"), Some(0)).unwrap(), 420.0);
}

#[test]
fn envelope_covenant_and_scenario() {
    let r = fml::run(BUDGET).expect("budget model runs");
    for a in &r.asserts {
        assert!(a.passed, "assert '{}' failed", a.name);
    }
    // Under the Squeeze scenario the 2026 envelope is breached (1,635 > 1,600).
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let (vals, _) = s.eval_scenario("Squeeze").unwrap();
    let cap = s.checked.index["budget_cap"];
    let tot = s.checked.index["total_expenses"];
    assert!(vals[tot][0][0] > vals[cap][0][0], "Squeeze must breach 2026");
    let head = s.checked.index["headroom"];
    assert!(vals[head][0][0] < 0.0);
}
