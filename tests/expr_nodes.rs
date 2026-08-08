//! Expression-level CST granularity (slice 5): Body / MapEntry / MatchArm
//! nodes nested inside declarations, and the first formula-level
//! operation — replacing a measure's formula from the tree.

use fml::cst::{decl_name, parse_cst, Red, RedChild, SyntaxKind};
use fml::Session;

fn find_decl<'a>(root: &Red<'a>, name: &str) -> Red<'a> {
    root.decls()
        .into_iter()
        .find(|d| decl_name(d.green).as_deref() == Some(name))
        .unwrap()
}

fn node_children<'a>(n: &Red<'a>, kind: SyntaxKind) -> Vec<Red<'a>> {
    n.children()
        .into_iter()
        .filter_map(|c| match c {
            RedChild::Node(x) if x.green.kind == kind => Some(x),
            _ => None,
        })
        .collect()
}

#[test]
fn bodies_arms_and_entries_are_nodes() {
    let cst = parse_cst(include_str!("../models/budget.fml")).unwrap();
    let root = Red::root(&cst);
    // expenses: InputDecl → Body → 3 MatchArms; Marketing arm → 4 MapEntries.
    let expenses = find_decl(&root, "expenses");
    let body = node_children(&expenses, SyntaxKind::Body);
    assert_eq!(body.len(), 1);
    let arms = node_children(&body[0], SyntaxKind::MatchArm);
    assert_eq!(arms.len(), 3);
    let mkt = &arms[0];
    assert!(mkt.green.text().trim_start().starts_with("Marketing"));
    let entries = node_children(mkt, SyntaxKind::MapEntry);
    assert_eq!(entries.len(), 4);
    assert!(entries[0].green.text().contains("2026: 420"), "{}", entries[0].green.text());
    // A computed measure's body node covers exactly its formula.
    let headroom = find_decl(&root, "headroom");
    let hb = node_children(&headroom, SyntaxKind::Body);
    assert!(hb[0].green.text().contains("budget_cap - total_expenses"));
}

#[test]
fn nesting_never_breaks_losslessness() {
    for src in [
        include_str!("../models/finplan.fml"),
        include_str!("../models/solar_pf.fml"),
        include_str!("../models/fx_consol.fml"),
        include_str!("../models/budget.fml"),
        include_str!("../models/rolling.fml"),
        include_str!("../models/team_budget.fml"),
    ] {
        assert_eq!(parse_cst(src).unwrap().text(), src);
    }
}

#[test]
fn body_text_and_replace_formula_round_trip() {
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.body_text("profit").as_deref(), Some("sales * margin"));
    let base = s.get("profit", None, Some(0)).unwrap();
    let files = s.replace_formula("profit", "sales * margin * 0.9").unwrap();
    // Everything outside the formula is byte-identical.
    let old = include_str!("../models/rolling.fml");
    assert_eq!(files[0].1, old.replace("= sales * margin\n", "= sales * margin * 0.9\n"));
    let mut s2 = Session::new(&files[0].1).unwrap();
    s2.run_full().unwrap();
    assert!((s2.get("profit", None, Some(0)).unwrap() - base * 0.9).abs() < 1e-9);
}

#[test]
fn replace_formula_pre_checks_syntax_and_targets() {
    let mut s = Session::new(include_str!("../models/rolling.fml")).unwrap();
    s.run_full().unwrap();
    let err = s.replace_formula("profit", "sales *").unwrap_err();
    assert!(err.contains("not a valid formula"), "err: {err}");
    assert!(s.replace_formula("ghost", "1").is_err());
    // Editing an INPUT's body (a map literal) works too.
    let files = s
        .replace_formula("sales_act", "{ 2026-01: 100, 2026-02: 104, 2026-03: 101, 2026-04: 108, 2026-05: 112, 2026-06: 120 }")
        .unwrap();
    let mut s2 = Session::new(&files[0].1).unwrap();
    s2.run_full().unwrap();
    assert_eq!(s2.get("sales", None, Some(5)).unwrap(), 120.0);
}

#[test]
fn formula_edits_route_to_the_owning_file() {
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
    let out = s.replace_formula("operations_spend", "330").unwrap();
    let ops = out.iter().find(|(n, _)| n == "team_operations.fml").unwrap();
    assert!(ops.1.contains("= 330"), "{}", ops.1);
    let master = out.iter().find(|(n, _)| n == "team_budget.fml").unwrap();
    assert_eq!(master.1, include_str!("../models/team_budget.fml"), "master untouched");
}
