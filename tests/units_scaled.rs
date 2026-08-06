//! Scaled units: `unit kEUR = 1000 EUR` — same dimension, different scale.
//! Adding them is a type error; `in` converts without a rate.

#[test]
fn scale_conversion_works_and_mixing_is_rejected() {
    let src = r#"
model t.scaled
calendar plan = yearly 2026 .. 2026
currency EUR
unit kEUR = 1000 EUR
input a : kEUR = 12
b : EUR over plan = a in EUR             // 12 kEUR -> 12_000 EUR
c : kEUR over plan = 500_000 EUR in kEUR // -> 500 kEUR
"#;
    let r = fml::run(src).expect("scaled model runs");
    let series: std::collections::HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();
    assert!((series["b"][0] - 12_000.0).abs() < 1e-9, "b = {}", series["b"][0]);
    assert!((series["c"][0] - 500.0).abs() < 1e-9, "c = {}", series["c"][0]);

    // Mixing scales without conversion is ill-typed.
    let bad = r#"
model t.badscale
calendar plan = yearly 2026 .. 2026
currency EUR
unit kEUR = 1000 EUR
input a : kEUR = 12
input b : EUR = 5
c = a + b
"#;
    let err = fml::compile(bad).expect_err("expected scale mismatch");
    assert!(err.contains("cannot add"), "unexpected error: {err}");
}

#[test]
fn eliminate_desugars_to_tie_assert() {
    let src = r#"
model t.elim
calendar plan = yearly 2026 .. 2027
currency EUR
input a : EUR flow over plan = 100
input b : EUR flow over plan = 100
eliminate pair over plan : a against b
"#;
    let r = fml::run(src).expect("eliminate model runs");
    assert_eq!(r.asserts.len(), 1);
    assert_eq!(r.asserts[0].name, "eliminate_pair");
    assert!(r.asserts[0].passed);
}
