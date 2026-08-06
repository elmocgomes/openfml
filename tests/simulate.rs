//! The `simulate` leg: distribution inputs, deterministic-by-default at the
//! median, reproducible Monte Carlo with percentile bands.

use fml::check::Dist;
use fml::Session;

const ROLLING: &str = include_str!("../models/rolling.fml");

#[test]
fn metalog_fits_its_quantiles_exactly() {
    let d = Dist::Metalog {
        a1: 0.03,
        a2: (0.06 - 0.01) / (2.0 * (9.0f64).ln()),
        a3: (0.06 + 0.01 - 2.0 * 0.03) / (0.8 * (9.0f64).ln()),
    };
    assert!((d.quantile(0.10) - 0.01).abs() < 1e-12);
    assert!((d.quantile(0.50) - 0.03).abs() < 1e-12);
    assert!((d.quantile(0.90) - 0.06).abs() < 1e-12);
}

#[test]
fn deterministic_default_is_the_median() {
    // The rolling model's growth is now ~ metalog{1%,3%,6%}: base evaluation
    // must equal the old deterministic 3% model exactly.
    let r = fml::run(ROLLING).expect("model runs");
    let s: std::collections::HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();
    assert!((s["sales"][6] - 118.0 * 1.03).abs() < 1e-9);
}

#[test]
fn simulate_produces_reproducible_bands() {
    let mut s = Session::new(ROLLING).unwrap();
    s.run_full().unwrap();
    let base_fy = s.get("fy_profit", None, None).unwrap();

    let sim1 = s.simulate(400).unwrap();
    let sim2 = s.simulate(400).unwrap();
    // Deterministic seeds: identical runs.
    for (a, b) in sim1.cells.iter().zip(sim2.cells.iter()) {
        assert_eq!(a.2, b.2, "simulation must be reproducible");
    }
    // The session is fully restored (median world).
    assert_eq!(s.get("fy_profit", None, None).unwrap(), base_fy);

    // fy_profit bands: p10 < p50 < p90, median near the deterministic value
    // (growth and margin are correlated 0.7, which shifts the product's
    // median slightly off the median product — allow a few percent).
    let fy = sim1.cells.iter().find(|c| c.0 == "fy_profit").unwrap();
    let [p10, p50, p90] = fy.2[0];
    assert!(p10 < p50 && p50 < p90, "bands ordered: {p10} {p50} {p90}");
    assert!((p50 - base_fy).abs() / base_fy < 0.03, "median near deterministic");
    // Closed months' SALES have zero spread — actuals are certain.
    let sales = sim1.cells.iter().find(|c| c.0 == "sales").unwrap();
    let [a10, _, a90] = sales.2[0];
    assert!((a90 - a10).abs() < 1e-9, "January sales is a booked actual");
    // December sales is uncertain (growth compounds through H2).
    let [d10, _, d90] = sales.2[11];
    assert!(d90 - d10 > 0.0, "December sales carries growth uncertainty");
    // Profit is banded even in January: margin uncertainty applies to
    // every month, actual or not.
    let profit = sim1.cells.iter().find(|c| c.0 == "profit").unwrap();
    let [j10, _, j90] = profit.2[0];
    assert!(j90 - j10 > 0.0, "January profit carries margin uncertainty");
}
