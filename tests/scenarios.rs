//! Scenarios: named input-overlay patches, evaluated as incremental deltas
//! from Base. Round-trip theorem: a scenario's values must equal a fresh
//! compile of the model with the overrides written in directly.

use openfml::Session;

const FINPLAN: &str = include_str!("fixtures/finplan.fml");

fn with_scenarios(extra: &str) -> String {
    format!("{FINPLAN}\n{extra}\n")
}

#[test]
fn scenario_overlay_matches_fresh_compile() {
    let src = with_scenarios(
        "scenario TestRec from Base {\n  g = { 2026: 2%, 2027: 0%, 2028: 1%, 2029: 3% }\n}\n",
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    let base_sales_2027 = s.get("sales", None, Some(1)).unwrap();

    let (vals, stats) = s.eval_scenario("TestRec").unwrap();
    // Incremental: only part of the plan re-ran.
    assert!(stats.steps_run > 0 && stats.steps_run <= stats.steps_total);

    // Base is untouched after scenario evaluation.
    assert_eq!(s.get("sales", None, Some(1)).unwrap(), base_sales_2027);

    // Reference: fresh model with the override baked into the source.
    let baked = FINPLAN.replace(
        "input g           : rate over plan = { 2026: 15%, 2027: 12%, 2028: 10%, 2029: 8% }",
        "input g           : rate over plan = { 2026: 2%, 2027: 0%, 2028: 1%, 2029: 3% }",
    );
    let mut fresh = Session::new(&baked).unwrap();
    fresh.run_full().unwrap();
    for (m, mi) in s.checked.measures.iter().enumerate() {
        for mb in 0..s.checked.tuple_count(m) {
            for (slot, (a, b)) in vals[m][mb].iter().zip(fresh.values[m][mb].iter()).enumerate() {
                let same = (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-6;
                assert!(same, "{}[{mb}][{slot}]: scenario {a} vs baked {b}", mi.name);
            }
        }
    }
}

#[test]
fn scenario_chaining_applies_parent_first() {
    let src = with_scenarios(
        "scenario Downside from Base {\n  g = { 2026: 5%, 2027: 5%, 2028: 5%, 2029: 5% }\n  i_new = 9%\n}\nscenario Severe from Downside {\n  g = { 2026: 0%, 2027: 0%, 2028: 0%, 2029: 0% }\n}\n",
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    let (severe, _) = s.eval_scenario("Severe").unwrap();
    // Severe overrides g to 0% but inherits Downside's i_new = 9%.
    let g_idx = s.checked.index["g"];
    let inew_idx = s.checked.index["i_new"];
    assert_eq!(severe[g_idx][0][0], 0.0);
    assert!((severe[inew_idx][0][0] - 0.09).abs() < 1e-12);
    // sales flat at 500k in Severe (0% growth).
    let sales_idx = s.checked.index["sales"];
    assert!((severe[sales_idx][0][3] - 500_000.0).abs() < 1e-6);
}

#[test]
fn scenario_validation_rejects_computed_targets() {
    let bad = with_scenarios("scenario X from Base { ebit = 5 }\n");
    let err = openfml::compile(&bad).expect_err("computed override must fail");
    assert!(err.contains("computed"), "unexpected: {err}");

    let bad_unit = with_scenarios("scenario Y from Base { pfd_div = 3 share }\n");
    let err2 = openfml::compile(&bad_unit).expect_err("unit mismatch must fail");
    assert!(err2.contains("unit") || err2.contains("USD"), "unexpected: {err2}");
}
