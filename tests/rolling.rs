//! The actuals switchover (`actuals X until closed else E`) and the
//! tornado sensitivity ranking.

use fml::Session;

const ROLLING: &str = include_str!("fixtures/rolling.fml");

#[test]
fn switchover_blends_actuals_and_forecast() {
    let r = fml::run(ROLLING).expect("rolling model runs");
    let s: std::collections::HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();
    // Closed months read the actuals verbatim.
    assert_eq!(s["sales"][0], 100.0);
    assert_eq!(s["sales"][5], 118.0);
    // First forecast month grows off the LAST ACTUAL (the boundary handoff).
    assert!((s["sales"][6] - 118.0 * 1.03).abs() < 1e-9);
    assert!((s["sales"][7] - 118.0 * 1.03 * 1.03).abs() < 1e-9);
}

#[test]
fn advancing_the_close_reblends() {
    // July's actual lands: extend `closed` and the actuals map.
    let advanced = ROLLING
        .replace("period closed = 2026-01 .. 2026-06", "period closed = 2026-01 .. 2026-07")
        .replace("2026-06: 118 }", "2026-06: 118, 2026-07: 125 }");
    let r = fml::run(&advanced).expect("advanced model runs");
    let s: std::collections::HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();
    assert_eq!(s["sales"][6], 125.0); // July now an actual, not 121.54 forecast
    assert!((s["sales"][7] - 125.0 * 1.03).abs() < 1e-9); // August grows off it
}

#[test]
fn tornado_ranks_drivers_by_impact() {
    let mut s = Session::new(ROLLING).unwrap();
    s.run_full().unwrap();
    let base = s.get("fy_profit", None, None).unwrap();
    let bars = s.tornado("fy_profit", None, None, 0.10).unwrap();
    assert!(!bars.is_empty());
    // The rolling-forecast lesson, discovered by the tornado itself: the
    // LAST ACTUAL (June) re-bases the whole forecast, so it outranks every
    // earlier actual month (which only move their own period).
    assert!(bars[0].0.contains("2026-06"), "top driver: {}", bars[0].0);
    let june_rank = bars.iter().position(|b| b.0.contains("2026-06")).unwrap();
    let jan_rank = bars.iter().position(|b| b.0.contains("2026-01")).unwrap();
    assert!(june_rank < jan_rank, "June must outrank January");
    // The session is fully restored afterwards.
    assert_eq!(s.get("fy_profit", None, None).unwrap(), base);
    // growth and margin are DISTRIBUTION inputs now (and correlated):
    // their uncertainty belongs to `simulate`, not the tornado — neither
    // may appear as a bar.
    assert!(
        bars.iter().all(|b| !b.0.starts_with("growth") && !b.0.starts_with("margin")),
        "distribution inputs are not tornado-perturbable"
    );
}
