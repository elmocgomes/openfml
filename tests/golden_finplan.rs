//! Golden test #1: Warren & Shelton FINPLAN.
//! Expected values from finmodel-lang-research/08-finplan-golden.md
//! (independent Python reference implementation, tolerance 0.01 USD).

use std::collections::HashMap;

fn series(result: &openfml::EvalResult) -> HashMap<String, Vec<f64>> {
    result.series.iter().cloned().collect()
}

#[test]
fn finplan_matches_reference_implementation() {
    let path = format!("{}/tests/fixtures/finplan.fml", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read finplan.fml");
    let result = openfml::run(&src).expect("compile + evaluate finplan");

    assert_eq!(result.period_labels, vec!["2026", "2027", "2028", "2029"]);
    let s = series(&result);

    // (measure, [2026, 2027, 2028, 2029], absolute tolerance)
    let golden: Vec<(&str, [f64; 4], f64)> = vec![
        ("sales", [575_000.00, 644_000.00, 708_400.00, 765_072.00], 0.01),
        ("ebit", [69_000.00, 77_280.00, 85_008.00, 91_808.64], 0.01),
        ("assets", [402_500.00, 450_800.00, 495_880.00, 535_550.40], 0.01),
        ("new_debt", [25_293.11, 20_420.00, 19_592.00, 18_200.96], 0.50),
        ("new_stock", [8_845.27, -3_113.62, -8_051.84, -13_876.33], 0.50),
        ("debt", [97_293.11, 109_713.11, 121_305.11, 131_506.06], 0.50),
        ("common_stock", [128_845.27, 125_731.65, 117_679.81, 103_803.48], 0.50),
        ("retained", [87_361.63, 118_075.25, 151_887.08, 188_432.22], 0.50),
        ("interest", [6_090.52, 7_019.12, 7_878.74, 8_633.21], 0.10),
        ("ni", [46_802.72, 52_389.36, 57_553.06, 62_108.56], 0.50),
        ("eafcd", [45_602.72, 51_189.36, 56_353.06, 60_908.56], 0.50),
        ("cmdiv", [18_241.09, 20_475.74, 22_541.22, 24_363.42], 0.50),
        ("numcs", [10_173.09, 10_119.10, 9_993.84, 9_798.03], 0.10),
        ("eps", [4.48, 5.06, 5.64, 6.22], 0.02),
        ("price", [53.79, 60.70, 67.67, 74.60], 0.20),
    ];

    for (name, expected, tol) in &golden {
        let got = s.get(*name).unwrap_or_else(|| panic!("missing series '{name}'"));
        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert!(
                (g - e).abs() <= *tol,
                "{name}[{}] = {g:.4}, expected {e:.4} (tolerance {tol})",
                2026 + i
            );
        }
    }

    // Leverage lands exactly on the 45% target every year.
    let debt = &s["debt"];
    let cs = &s["common_stock"];
    let re = &s["retained"];
    for t in 0..4 {
        let leverage = debt[t] / (cs[t] + re[t]);
        assert!(
            (leverage - 0.45).abs() < 1e-3,
            "leverage[{}] = {leverage:.5}, expected 0.45000",
            2026 + t
        );
    }

    // Both model-level asserts pass.
    for a in &result.asserts {
        assert!(
            a.passed,
            "model assert '{}' failed (max deviation {}, first failure {:?})",
            a.name, a.max_deviation, a.first_failure
        );
    }
}

#[test]
fn unit_errors_are_caught() {
    // Adding USD to shares must be rejected at check time.
    let src = r#"
model bad.units
calendar plan = yearly 2026 .. 2027
currency USD
unit share
input a : USD over plan = 100
input b : share over plan = 10
c = a + b
"#;
    let err = openfml::compile(src).expect_err("expected a unit error");
    assert!(err.contains("cannot add"), "unexpected error: {err}");
}

#[test]
fn circularity_outside_solve_is_rejected() {
    let src = r#"
model bad.cycle
calendar plan = yearly 2026 .. 2027
currency USD
a : USD over plan = b + 1
b : USD over plan = a * 2
"#;
    let err = openfml::compile(src).expect_err("expected a cycle error");
    assert!(err.contains("solve"), "unexpected error: {err}");
}

#[test]
fn missing_init_for_prev_is_rejected() {
    let src = r#"
model bad.init
calendar plan = yearly 2026 .. 2027
currency USD
a : USD over plan = prev(a) * 2
"#;
    let err = openfml::compile(src).expect_err("expected an init error");
    assert!(err.contains("init"), "unexpected error: {err}");
}
