//! Structural edits (CST slice: declaration-level operations): add a
//! period to the calendar (extending full-range maps, across files) and
//! rename a measure everywhere — both as source transformations that
//! recompile cleanly with formatting preserved.

use fml::Session;
use std::collections::HashMap;

fn team_session() -> Session {
    let files = [
        ("team_marketing.fml", include_str!("../models/team_marketing.fml")),
        ("team_engineering.fml", include_str!("../models/team_engineering.fml")),
        ("team_operations.fml", include_str!("../models/team_operations.fml")),
    ];
    let exp = fml::expand_includes_with_map(
        "team_budget.fml",
        include_str!("../models/team_budget.fml"),
        &mut |p| {
            files
                .iter()
                .find(|(n, _)| *n == p)
                .map(|(_, t)| t.to_string())
                .ok_or_else(|| format!("missing {p}"))
        },
    )
    .unwrap();
    let mut s = Session::new_expanded(exp).unwrap();
    s.run_full().unwrap();
    s
}

#[test]
fn add_period_extends_calendar_and_full_range_maps() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    let (files, label) = s.add_period().unwrap();
    assert_eq!(label, "2030");
    let text = &files[0].1;
    assert!(text.contains("yearly 2026 .. 2030"), "calendar bumped");
    // Full-range maps gain a copy of their last entry…
    assert!(text.contains("2029: 2_050, 2030: 2050 }"), "budget_cap extended: {text}");
    assert!(text.contains("2029: 540, 2030: 540 }"), "Marketing arm extended");
    // …broadcast arms (Operations -> 315) need nothing and get nothing.
    assert!(text.contains("Operations  -> 315\n"), "broadcast arm untouched");
    // The transformed model compiles and runs with the copied year live.
    let mut s2 = Session::new(text).unwrap();
    s2.run_full().unwrap();
    assert_eq!(s2.checked.calendar.len, 5);
    assert_eq!(s2.get("budget_cap", None, Some(4)).unwrap(), 2050.0);
    assert_eq!(s2.get("headroom", None, Some(4)).unwrap(), 2050.0 - 1955.0 - 0.0);
}

#[test]
fn add_period_reaches_into_included_files() {
    let s = team_session();
    let (files, label) = s.add_period().unwrap();
    assert_eq!(label, "2030");
    let by_name: HashMap<&str, &str> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    assert!(by_name["team_budget.fml"].contains("yearly 2026 .. 2030"));
    assert!(by_name["team_marketing.fml"].contains("2029: 540, 2030: 540 }"), "map in the TEAM file grew");
    assert!(by_name["team_engineering.fml"].contains("2029: 1_100, 2030: 1100 }"));
    assert!(
        by_name["team_operations.fml"].contains("= 315"),
        "broadcast file untouched"
    );
    // Recompile the whole multi-file model from the new texts.
    let exp = fml::expand_includes_with_map("team_budget.fml", by_name["team_budget.fml"], &mut |p| {
        by_name.get(p).map(|t| t.to_string()).ok_or_else(|| format!("missing {p}"))
    })
    .unwrap();
    let mut s2 = Session::new_expanded(exp).unwrap();
    s2.run_full().unwrap();
    assert_eq!(s2.get("total_expenses", None, Some(4)).unwrap(), 540.0 + 1100.0 + 315.0);
}

#[test]
fn add_period_skips_subrange_maps() {
    // rolling: sales_act ranges over `closed`, not the calendar — adding a
    // month must NOT touch it.
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    let (files, label) = s.add_period().unwrap();
    assert_eq!(label, "2027-01");
    let text = &files[0].1;
    assert!(text.contains("monthly 2026-01 .. 2027-01"));
    assert!(text.contains("2026-06: 118 }"), "closed actuals map untouched");
    let mut s2 = Session::new(text).unwrap();
    s2.run_full().unwrap();
    assert_eq!(s2.checked.calendar.len, 13);
    // The new month forecasts off December.
    let dec = s2.get("sales", None, Some(11)).unwrap();
    assert!((s2.get("sales", None, Some(12)).unwrap() - dec * 1.03).abs() < 1e-9);
}

#[test]
fn rename_measure_rewrites_every_reference() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    let files = s.rename_measure("expenses", "spend").unwrap();
    let text = &files[0].1;
    assert!(text.contains("input spend : kEUR flow over CostCenter"));
    assert!(text.contains("= spend + overhead_share"), "loaded_cost reference renamed");
    assert!(text.contains("= spend[Total]"), "indexed reference renamed");
    assert!(!text.contains("input expenses"), "old name gone from declarations");
    // Semantics preserved exactly.
    let mut s2 = Session::new(text).unwrap();
    s2.run_full().unwrap();
    assert_eq!(
        s2.get("total_expenses", None, Some(0)).unwrap(),
        s.get("total_expenses", None, Some(0)).unwrap()
    );
    assert_eq!(s2.get("spend", Some("Marketing"), Some(0)).unwrap(), 420.0);
}

#[test]
fn rename_reaches_into_included_files() {
    let s = team_session();
    let files = s.rename_measure("marketing_spend", "mkt").unwrap();
    let by_name: HashMap<&str, &str> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    assert!(by_name["team_marketing.fml"].contains("input mkt : kEUR"));
    assert!(by_name["team_budget.fml"].contains("= mkt + engineering_spend"));
    // The comment naming the team is deliberately untouched.
    assert!(by_name["team_marketing.fml"].contains("// Marketing — owned by"));
}

#[test]
fn rename_guards_the_namespaces() {
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    for (new, why) in [
        ("headroom", "existing measure"),
        ("Marketing", "dimension member"),
        ("CostCenter", "dimension"),
        ("kEUR", "unit"),
        ("plan", "calendar/range"),
        ("Squeeze", "scenario"),
        ("round", "keyword"),
        ("2x", "not an identifier"),
    ] {
        assert!(s.rename_measure("expenses", new).is_err(), "must refuse {why}");
    }
    assert!(s.rename_measure("ghost", "x").is_err(), "unknown source name");
}
