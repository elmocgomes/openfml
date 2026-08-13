//! "Explain this number" — the provenance layer: which arm fired, which
//! cells fed a value, where the definition lives (routed to the owning
//! file through the include source map).

use openfml::Session;
use std::collections::HashMap;

const ROLLING: &str = include_str!("fixtures/rolling.fml");

fn rolling() -> Session {
    let mut s = Session::new(ROLLING).unwrap();
    s.run_full().unwrap();
    s
}

#[test]
fn forecast_month_explains_via_the_forecast_arm() {
    let mut s = rolling();
    // July = first forecast month: prev(sales)@June × (1 + growth).
    let ex = s.explain("sales", None, Some(6)).unwrap();
    assert!(ex.arm.contains("\\ closed"), "arm: {}", ex.arm);
    let june = ex.deps.iter().find(|d| d.name == "sales" && d.via == "prev").unwrap();
    assert_eq!(june.period, Some(5));
    assert_eq!(june.value, 118.0);
    let growth = ex.deps.iter().find(|d| d.name == "growth").unwrap();
    assert_eq!(growth.value, 0.03);
    // The actuals arm did NOT fire: sales_act is not a dependency.
    assert!(ex.deps.iter().all(|d| d.name != "sales_act"));
    assert!((ex.value - 118.0 * 1.03).abs() < 1e-9);
}

#[test]
fn actual_month_explains_via_the_actuals_arm() {
    let mut s = rolling();
    let ex = s.explain("sales", None, Some(1)).unwrap();
    assert!(ex.arm.contains("in closed"), "arm: {}", ex.arm);
    let act = ex.deps.iter().find(|d| d.name == "sales_act").unwrap();
    assert_eq!(act.value, 104.0);
    assert!(ex.deps.iter().all(|d| d.name != "growth"), "no growth in a booked month");
}

#[test]
fn aggregates_list_their_constituents() {
    let mut s = rolling();
    let ex = s.explain("fy_profit", None, None).unwrap();
    let profs: Vec<_> = ex.deps.iter().filter(|d| d.name == "profit" && d.via == "sum").collect();
    assert_eq!(profs.len(), 12, "sum[m](profit) has 12 constituent cells");
    let total: f64 = profs.iter().map(|d| d.value).sum();
    assert!((total - ex.value).abs() < 1e-9, "constituents add up to the cell");
}

#[test]
fn inputs_are_terminal_and_describe_themselves() {
    let mut s = rolling();
    let ex = s.explain("growth", None, None).unwrap();
    assert!(ex.is_input);
    assert!(ex.deps.is_empty());
    assert!(ex.note.contains("metalog"), "note: {}", ex.note);
    assert!(ex.note.contains("correlated 0.7 with 'margin'"), "note: {}", ex.note);
}

#[test]
fn prev_at_range_start_explains_the_init() {
    let mut s = rolling();
    // January is an actual, so test init via a measure-level init model.
    let src = "model demo.init\ncalendar plan = yearly 2026 .. 2028\ncurrency EUR\nbal : EUR stock over plan init: 500 = prev(bal) + 10\n";
    let mut s2 = Session::new(src).unwrap();
    s2.run_full().unwrap();
    let ex = s2.explain("bal", None, Some(0)).unwrap();
    let init = ex.deps.iter().find(|d| d.via == "prev" && d.label == "init").unwrap();
    assert_eq!(init.value, 500.0);
    // And a mid-range period points at the previous cell instead.
    let ex1 = s2.explain("bal", None, Some(1)).unwrap();
    let prev = ex1.deps.iter().find(|d| d.via == "prev").unwrap();
    assert_eq!(prev.period, Some(0));
    assert_eq!(prev.value, 510.0);
    drop(s);
}

#[test]
fn multi_file_definitions_locate_their_owning_file() {
    let files: HashMap<&str, String> = HashMap::from([
        ("team_marketing.fml", include_str!("fixtures/team_marketing.fml").to_string()),
        ("team_engineering.fml", include_str!("fixtures/team_engineering.fml").to_string()),
        ("team_operations.fml", include_str!("fixtures/team_operations.fml").to_string()),
    ]);
    let exp = openfml::expand_includes_with_map(
        "team_budget.fml",
        include_str!("fixtures/team_budget.fml"),
        &mut |p| files.get(p).cloned().ok_or_else(|| format!("missing {p}")),
    )
    .unwrap();
    let mut s = Session::new_expanded(exp).unwrap();
    s.run_full().unwrap();
    // total_expenses lives in the master file; its deps span all 3 teams.
    let ex = s.explain("total_expenses", None, Some(0)).unwrap();
    assert_eq!(ex.file, "team_budget.fml");
    let names: Vec<&str> = ex.deps.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"marketing_spend") && names.contains(&"engineering_spend") && names.contains(&"operations_spend"));
    assert!((ex.value - 1635.0).abs() < 1e-9);
    // A team input locates into ITS file at the right local line.
    let exm = s.explain("marketing_spend", None, Some(0)).unwrap();
    assert_eq!(exm.file, "team_marketing.fml");
    assert_eq!(exm.line, 2, "the input declaration is line 2 of the team file");
    assert!(exm.note.contains("editable"), "note: {}", exm.note);
}

#[test]
fn group_rollups_expand_to_leaf_cells() {
    let mut s = Session::new(include_str!("fixtures/budget.fml")).unwrap();
    s.run_full().unwrap();
    // total_expenses = expenses[Total] — a tree rollup: 3 leaf deps.
    let ex = s.explain("total_expenses", None, Some(0)).unwrap();
    let leaves: Vec<_> = ex.deps.iter().filter(|d| d.via == "rollup").collect();
    assert_eq!(leaves.len(), 3);
    let by_member: HashMap<&str, f64> =
        leaves.iter().map(|d| (d.member.as_str(), d.value)).collect();
    assert_eq!(by_member["Marketing"], 420.0);
    assert_eq!(by_member["Engineering"], 900.0);
    assert_eq!(by_member["Operations"], 315.0);
}

#[test]
fn solve_measures_say_so() {
    let mut s = Session::new(include_str!("fixtures/finplan.fml")).unwrap();
    s.run_full().unwrap();
    let solved = s
        .checked
        .measures
        .iter()
        .find(|m| m.solve.is_some())
        .map(|m| (m.name.clone(), m.range.0))
        .unwrap();
    let ex = s.explain(&solved.0, None, Some(solved.1)).unwrap();
    assert!(ex.note.contains("solve"), "note: {}", ex.note);
    assert!(!ex.deps.is_empty(), "solve members still show their dependencies");
}
