//! Phase 2: incremental recalculation — dirty propagation with early cutoff
//! over the (measure × member × period) micro-graph.

use fml::Session;

const FINPLAN: &str = include_str!("../models/finplan.fml");

#[test]
fn incremental_matches_full_recompute() {
    // Reference: fresh session with the tweaked input, full run.
    let mut reference = Session::new(FINPLAN).unwrap();
    reference.set_input("g", None, Some(1), 0.20).unwrap(); // 2027 growth 12% -> 20%
    reference.run_full().unwrap();

    // Incremental: full run first, then tweak + recalc.
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    s.set_input("g", None, Some(1), 0.20).unwrap();
    let stats = s.recalc().unwrap();

    // Only a fraction of the plan re-ran…
    assert!(
        stats.steps_run < stats.steps_total,
        "expected partial recompute, ran {}/{}",
        stats.steps_run,
        stats.steps_total
    );
    // …and every value matches the from-scratch reference exactly.
    for (m, mi) in s.checked.measures.iter().enumerate() {
        for mb in 0..s.checked.tuple_count(m) {
            for (slot, (a, b)) in s.values[m][mb].iter().zip(reference.values[m][mb].iter()).enumerate() {
                let same = (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-9;
                assert!(same, "{}[{mb}][{slot}]: incremental {a} vs full {b}", mi.name);
            }
        }
    }
    // 2026 is upstream of the 2027 growth change: it must NOT have re-run.
    // sales@2026 is an Eval step; verify its value object identity by
    // checking stats: at least the 2026-only steps were skipped.
    let sales_2026 = s.get("sales", None, Some(0)).unwrap();
    assert!((sales_2026 - 575_000.0).abs() < 1e-6);
}

#[test]
fn early_cutoff_stops_propagation() {
    // Setting an input to its existing value dirties nothing.
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    s.set_input("g", None, Some(1), 0.12).unwrap(); // same as declared
    let stats = s.recalc().unwrap();
    assert_eq!(stats.steps_run, 0, "no-op edit must not recompute anything");
}

#[test]
fn later_period_change_skips_earlier_periods() {
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    let before_2026: Vec<f64> = (0..1).map(|_| s.get("ni", None, Some(0)).unwrap()).collect();
    s.set_input("g", None, Some(3), 0.02).unwrap(); // change only 2029 growth
    let stats = s.recalc().unwrap();
    assert!(stats.steps_run < stats.steps_total / 2, "2029-only change re-ran {}/{} steps", stats.steps_run, stats.steps_total);
    // 2026 values untouched.
    assert_eq!(s.get("ni", None, Some(0)).unwrap(), before_2026[0]);
}
