//! `model_info_json` — the model's structural self-description that feeds
//! the workbench's management view: include hierarchy, the measure
//! reference graph (refs + dependents, symmetric), dims with roll-up
//! groups, asserts with their referenced measures.

use fml::json::{parse, J};
use fml::Session;

fn arr<'a>(j: &'a J, key: &str) -> &'a Vec<J> {
    match j.get(key) {
        Some(J::A(v)) => v,
        other => panic!("expected array at {key}, got {other:?}"),
    }
}
fn s(j: &J) -> &str {
    match j {
        J::S(s) => s,
        other => panic!("expected string, got {other:?}"),
    }
}
fn names(v: &Vec<J>) -> Vec<&str> {
    v.iter().map(s).collect()
}

fn measure<'a>(info: &'a J, name: &str) -> &'a J {
    arr(info, "measures")
        .iter()
        .find(|m| s(m.get("name").unwrap()) == name)
        .unwrap_or_else(|| panic!("measure {name} missing"))
}

#[test]
fn the_reference_graph_is_exact_and_symmetric() {
    let mut session = Session::new(include_str!("../models/budget.fml")).unwrap();
    session.run_full().unwrap();
    let info = parse(&session.model_info_json()).unwrap();
    assert_eq!(s(info.get("model").unwrap()), "demo.budget");

    // total = sum[CostCenter](expenses): refs exactly {expenses}.
    let total = measure(&info, "total_expenses");
    assert_eq!(names(arr(total, "refs")), vec!["expenses"]);
    // …and expenses lists total among its dependents (symmetry).
    let expenses = measure(&info, "expenses");
    assert!(names(arr(expenses, "dependents")).contains(&"total_expenses"));
    assert_eq!(s(expenses.get("unit").unwrap()), "kEUR");
    assert_eq!(names(arr(expenses, "dims")), vec!["CostCenter"]);
    assert_eq!(expenses.get("input"), Some(&J::B(true)));
    assert_eq!(expenses.get("editable"), Some(&J::B(true)));
    // A computed measure is not literal-editable.
    assert_eq!(total.get("editable"), Some(&J::B(false)));

    // Dims carry their roll-up group and members.
    let dims = arr(&info, "dims");
    let cc = dims.iter().find(|d| s(d.get("name").unwrap()) == "CostCenter").unwrap();
    assert_eq!(s(cc.get("group").unwrap()), "Total");
    assert!(names(arr(cc, "members")).contains(&"Marketing"));

    // Asserts name the measures they constrain.
    let asserts = arr(&info, "asserts");
    let cap = asserts.iter().find(|a| s(a.get("name").unwrap()) == "within_envelope").unwrap();
    let cap_refs = names(arr(cap, "refs"));
    assert!(cap_refs.contains(&"total_expenses") && cap_refs.contains(&"budget_cap"), "{cap_refs:?}");
}

#[test]
fn the_include_hierarchy_names_children_and_owns_decls() {
    let master = include_str!("../models/team_budget.fml");
    let exp = fml::expand_includes_with_map("team_budget.fml", master, &mut |p: &str| {
        std::fs::read_to_string(format!("models/{p}")).map_err(|e| e.to_string())
    })
    .unwrap();
    let mut session = Session::new_expanded(exp).unwrap();
    session.run_full().unwrap();
    let info = parse(&session.model_info_json()).unwrap();

    let files = arr(&info, "files");
    assert_eq!(s(files[0].get("name").unwrap()), "team_budget.fml");
    let incl = names(arr(&files[0], "includes"));
    assert!(
        incl.contains(&"team_marketing.fml") && incl.contains(&"team_engineering.fml"),
        "{incl:?}"
    );
    // A fragment owns its declaration; the master owns the totals.
    let mkt = files.iter().find(|f| s(f.get("name").unwrap()) == "team_marketing.fml").unwrap();
    let mkt_decls: Vec<&str> =
        arr(mkt, "decls").iter().map(|d| s(d.get("name").unwrap())).collect();
    assert!(mkt_decls.contains(&"marketing_spend"), "{mkt_decls:?}");
    let master_decls: Vec<&str> =
        arr(&files[0], "decls").iter().map(|d| s(d.get("name").unwrap())).collect();
    assert!(master_decls.contains(&"total_expenses"), "{master_decls:?}");
    // Measures are located in their OWNING file, not the flat expansion.
    let spend = measure(&info, "marketing_spend");
    assert_eq!(s(spend.get("file").unwrap()), "team_marketing.fml");
}

#[test]
fn solve_and_distribution_flags_survive() {
    let mut session = Session::new(include_str!("../models/rolling.fml")).unwrap();
    session.run_full().unwrap();
    let info = parse(&session.model_info_json()).unwrap();
    let margin = measure(&info, "margin");
    assert_eq!(margin.get("dist"), Some(&J::B(true)));
    let correlations = arr(&info, "correlations");
    assert_eq!(correlations.len(), 1, "rolling declares one correlate pair");
}
