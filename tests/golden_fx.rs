//! Golden test #3: two-entity FX consolidation (IAS 21 current-rate).
//! Expected values from finmodel-lang-research/10-fx-consolidation-golden.md
//! (independent Python reference implementation; ties at 1e-6).

use std::collections::HashMap;

#[test]
fn fx_consolidation_matches_reference_implementation() {
    let path = format!("{}/tests/fixtures/fx_consol.fml", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fx_consol.fml");
    let result = fml::run(&src).expect("compile + evaluate fx_consol");

    let series: HashMap<String, Vec<f64>> = result.series.iter().cloned().collect();
    let scalars: HashMap<String, f64> = result.scalars.iter().cloned().collect();

    // ---- monthly samples (kEUR): months are 0-based indices ---------------
    let close = |name: &str, idx: usize, expected: f64, tol: f64| {
        let got = series[name][idx];
        assert!(
            (got - expected).abs() <= tol,
            "{name}[{idx}] = {got:.4}, expected {expected:.4}"
        );
    };
    close("cons_revenue", 0, 1_168.66, 0.02);
    close("cons_revenue", 5, 1_152.42, 0.02);
    close("cons_revenue", 11, 1_144.83, 0.02);
    close("cons_ni", 0, 448.85, 0.02);
    close("cons_ni", 5, 437.89, 0.02);
    close("cons_ni", 11, 475.86, 0.02);
    close("cta_us", 0, -12.00, 0.02);
    close("cta_us", 1, -23.99, 0.02);
    close("cta_us", 5, -71.42, 0.02);
    close("cta_us", 10, -97.50, 0.02);
    close("cta_us", 11, -97.50, 0.02);
    close("grp_assets", 11, 15_646.12, 0.02);
    close("grp_equity", 11, 15_646.12, 0.02);

    // CTA is zero for the EUR-functional entity, by construction.
    for t in 0..12 {
        let v = series["cta[PT_Co]"][t];
        assert!(v.abs() < 1e-6, "cta[PT_Co][{t}] = {v}");
    }

    // The EUR-denominated payable translates back to exactly the receivable.
    for t in 0..12 {
        let v = series["liabs_tr[US_Co]"][t];
        assert!((v - 5_000.0).abs() < 1e-9, "liabs_tr[US_Co][{t}] = {v}");
    }

    // December's rate is flat month-over-month: remeasurement and ΔCTA
    // vanish on their own (emergent behavior pinned).
    assert!(series["fx_remeasure[US_Co]"][11].abs() < 1e-9);
    assert!((series["cta_us"][11] - series["cta_us"][10]).abs() < 1e-9);

    // ---- FY totals --------------------------------------------------------
    assert!((scalars["fy_revenue"] - 13_855.77).abs() < 0.02, "fy_revenue = {}", scalars["fy_revenue"]);
    assert!((scalars["fy_ni"] - 5_447.33).abs() < 0.02, "fy_ni = {}", scalars["fy_ni"]);
    assert!((scalars["fy_fx"] - -357.29).abs() < 0.02, "fy_fx = {}", scalars["fy_fx"]);

    // ---- all five invariant families hold ---------------------------------
    for a in &result.asserts {
        assert!(
            a.passed,
            "model assert '{}' failed (max deviation {}, first failure {:?})",
            a.name, a.max_deviation, a.first_failure
        );
    }
    assert_eq!(result.asserts.len(), 4);
}

#[test]
fn local_units_are_member_checked() {
    // Adding a kEUR quantity to a USD-functional entity's local measure
    // must fail for that member.
    let src = r#"
model bad.local
calendar m = monthly 2026-01 .. 2026-03
dimension Entity = tree { Group -> { A, B } }
currency kEUR
unit kUSD
functional Entity = { A: kEUR, B: kUSD }
x : local flow over Entity, m = 5 kEUR
"#;
    let err = fml::compile(src).expect_err("expected a member unit error");
    assert!(err.contains("member B") || err.contains("kUSD"), "unexpected error: {err}");
}

#[test]
fn group_aggregation_of_local_units_is_rejected() {
    let src = r#"
model bad.group
calendar m = monthly 2026-01 .. 2026-03
dimension Entity = tree { Group -> { A, B } }
currency kEUR
unit kUSD
functional Entity = { A: kEUR, B: kUSD }
x : local flow over Entity, m = match Entity { A -> 1, B -> 2 }
y : kEUR flow over m = x[Group]
"#;
    let err = fml::compile(src).expect_err("expected a group aggregation error");
    assert!(err.contains("translate"), "unexpected error: {err}");
}
