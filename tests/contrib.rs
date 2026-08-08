//! Quantified provenance: `explain` decomposes a cell's value into EXACT
//! additive terms — they sum to the value, signed. No sensitivity
//! approximations: what can't be decomposed additively stays one term.

use fml::Session;

fn term_sum(ex: &fml::live::Explanation) -> f64 {
    ex.terms.iter().map(|t| t.value).sum()
}

#[test]
fn rollup_terms_are_exact_shares() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    // total_expenses = expenses[Total]: 3 leaf terms, summing exactly.
    let ex = s.explain("total_expenses", None, Some(0)).unwrap();
    assert_eq!(ex.terms.len(), 3);
    assert!((term_sum(&ex) - ex.value).abs() < 1e-9);
    let mkt = ex.terms.iter().find(|t| t.label.contains("Marketing")).unwrap();
    assert_eq!(mkt.value, 420.0);
    assert_eq!(
        mkt.cell.as_ref().unwrap(),
        &("expenses".to_string(), "Marketing".to_string(), Some(0))
    );
}

#[test]
fn signed_terms_bridge_a_difference() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    // headroom = budget_cap − total_expenses: +1700 and −1635 → 65.
    let ex = s.explain("headroom", None, Some(0)).unwrap();
    assert_eq!(ex.terms.len(), 2);
    assert_eq!(ex.terms[0].value, 1700.0);
    assert_eq!(ex.terms[1].value, -1635.0);
    assert!(ex.terms[1].label.starts_with("−"), "negative terms are marked: {}", ex.terms[1].label);
    assert!((term_sum(&ex) - 65.0).abs() < 1e-9);
}

#[test]
fn calendar_sums_decompose_per_period() {
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    let ex = s.explain("fy_profit", None, None).unwrap();
    assert_eq!(ex.terms.len(), 12, "sum[m](profit) → 12 monthly terms");
    assert!((term_sum(&ex) - ex.value).abs() < 1e-9);
    // December's contribution outweighs January's (growth compounds).
    let jan = ex.terms.first().unwrap().value;
    let dec = ex.terms.last().unwrap().value;
    assert!(dec > jan, "dec {dec} > jan {jan}");
}

#[test]
fn non_additive_cells_are_one_honest_term() {
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    // profit = sales × margin: no additive split exists — one labeled term.
    let ex = s.explain("profit", None, Some(3)).unwrap();
    assert_eq!(ex.terms.len(), 1);
    assert_eq!(ex.terms[0].label, "sales × margin");
    assert!((ex.terms[0].value - ex.value).abs() < 1e-9);
    assert!(ex.terms[0].cell.is_none(), "opaque terms are not drillable");
}

#[test]
fn npv_terms_are_the_pv_bridge() {
    let src = "model demo.pv\ncalendar plan = yearly 2026 .. 2028\ncurrency EUR\n\
        input cf : EUR flow over plan = { 2026: 100, 2027: 100, 2028: 100 }\n\
        input r : rate = 10%\n\
        value : EUR = npv(r, cf over plan)\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    let ex = s.explain("value", None, None).unwrap();
    assert_eq!(ex.terms.len(), 3);
    assert!((ex.terms[0].value - 100.0 / 1.1).abs() < 1e-9);
    assert!((ex.terms[1].value - 100.0 / 1.1_f64.powi(2)).abs() < 1e-9);
    assert!((term_sum(&ex) - ex.value).abs() < 1e-9, "PV bridge sums to npv");
    assert!(ex.terms[0].label.contains("PV @ 2026"), "label: {}", ex.terms[0].label);
}

#[test]
fn mixed_sum_of_cell_and_product_terms() {
    let src = "model demo.mix\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n\
        input base : EUR flow over plan = 50\n\
        input units : 1 flow over plan = 4\n\
        input price : EUR flow over plan = 25\n\
        rev : EUR flow over plan = base + units * price - 10\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    let ex = s.explain("rev", None, Some(0)).unwrap();
    assert_eq!(ex.terms.len(), 3);
    assert_eq!(ex.terms[0].value, 50.0);           // cell term
    assert_eq!(ex.terms[1].value, 100.0);          // opaque product term
    assert_eq!(ex.terms[1].label, "units × price");
    assert_eq!(ex.terms[2].value, -10.0);          // signed literal
    assert!((term_sum(&ex) - 140.0).abs() < 1e-9);
    assert!(ex.terms[0].cell.is_some() && ex.terms[1].cell.is_none());
}

#[test]
fn allocation_terms_expose_the_loaded_cost_split() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    // loaded_cost = expenses + overhead_share: exactly two drillable terms.
    let ex = s.explain("loaded_cost", Some("Marketing"), Some(0)).unwrap();
    assert_eq!(ex.terms.len(), 2);
    assert_eq!(ex.terms[0].value, 420.0);
    assert_eq!(ex.terms[1].value, 48.0);
    assert!(ex.terms.iter().all(|t| t.cell.is_some()));
}

#[test]
fn inputs_have_no_terms() {
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    let ex = s.explain("growth", None, None).unwrap();
    assert!(ex.terms.is_empty());
}
