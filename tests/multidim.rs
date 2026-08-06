//! Multi-dimension broadcasting: measures over Product × Region × time,
//! automatic broadcast of lower-dimensional operands, per-dimension
//! aggregation, group rollup, and unbound-dimension errors.

use std::collections::HashMap;

const MODEL: &str = r#"
model t.multidim
calendar plan = yearly 2026 .. 2027
dimension Product = list { Alpha, Beta }
dimension Region  = tree { World -> { EU, US } }
currency EUR

input price : EUR over Product = match Product { Alpha -> 10, Beta -> 20 }
input volume : ratio over Product, Region, plan = match Product {
  Alpha -> match Region { EU -> 5, US -> 7 }
  Beta  -> 11
}
input growth : rate over plan = { 2026: 0%, 2027: 10% }

// price (Product) broadcasts over Region and time; growth (time) over both dims
revenue : EUR flow over Product, Region, plan = price * volume * (1 + growth)

// aggregate one dimension at a time
rev_by_region : EUR flow over Region, plan = sum[Product](revenue)
rev_by_product : EUR flow over Product, plan = sum[Region](revenue)

// group rollup on the tree dimension
total : EUR flow over plan = rev_by_region[World]
total2 : EUR flow over plan = sum[Product](sum[Region](revenue))

assert agree : total == total2 ± 0.000001
assert alpha_eu : revenue[Alpha][EU] == price[Alpha] * 5 * (1 + growth) ± 0.000001
"#;

#[test]
fn broadcasting_and_aggregation() {
    let r = fml::run(MODEL).expect("multidim model runs");
    let series: HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();

    // 2026: Alpha: EU 50, US 70; Beta: EU 220, US 220 → total 560
    assert!((series["revenue[Alpha,EU]"][0] - 50.0).abs() < 1e-9);
    assert!((series["revenue[Alpha,US]"][0] - 70.0).abs() < 1e-9);
    assert!((series["revenue[Beta,EU]"][0] - 220.0).abs() < 1e-9);
    assert!((series["revenue[Beta,US]"][0] - 220.0).abs() < 1e-9);
    assert!((series["rev_by_region[EU]"][0] - 270.0).abs() < 1e-9);
    assert!((series["rev_by_region[US]"][0] - 290.0).abs() < 1e-9);
    assert!((series["rev_by_product[Alpha]"][0] - 120.0).abs() < 1e-9);
    assert!((series["rev_by_product[Beta]"][0] - 440.0).abs() < 1e-9);
    assert!((series["total"][0] - 560.0).abs() < 1e-9);
    // 2027: everything ×1.1
    assert!((series["total"][1] - 616.0).abs() < 1e-6);

    for a in &r.asserts {
        assert!(a.passed, "assert '{}' failed", a.name);
    }
}

#[test]
fn unbound_dimension_is_rejected() {
    let bad = r#"
model t.unbound
calendar plan = yearly 2026 .. 2026
dimension Product = list { A, B }
currency EUR
input x : EUR flow over Product, plan = 1
y : EUR flow over plan = x
"#;
    let err = fml::compile(bad).expect_err("expected unbound-dimension error");
    assert!(err.contains("sum[Product]") || err.contains("unbound"), "unexpected: {err}");
}

#[test]
fn local_units_reject_cross_currency_dim_sum() {
    let bad = r#"
model t.badsum
calendar m = monthly 2026-01 .. 2026-02
dimension Entity = tree { Group -> { A, B } }
currency kEUR
unit kUSD
functional Entity = { A: kEUR, B: kUSD }
x : local flow over Entity, m = match Entity { A -> 1, B -> 2 }
y : kEUR flow over m = sum[Entity](x)
"#;
    let err = fml::compile(bad).expect_err("expected cross-currency sum error");
    assert!(err.contains("translate"), "unexpected: {err}");
}
