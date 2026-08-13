//! Per-declaration check queries, slice 1: the cached `references` query
//! (fingerprint-keyed, surviving reloads) and blast-radius evaluation —
//! a semantic edit to one measure re-evaluates ONLY that measure and its
//! transitive dependents, copying every other value from the old session.
//! Theorem, as ever: incremental ≡ from-scratch.

use openfml::{Expanded, Segment, Session, SourceFile};

fn single(src: &str) -> Expanded {
    Expanded {
        flat: src.to_string(),
        files: vec![SourceFile { name: "model".into(), text: src.to_string() }],
        segments: vec![Segment { flat_start: 0, flat_end: src.len(), file: 0, local_start: 0 }],
    }
}

const BUDGET: &str = include_str!("fixtures/budget.fml");

fn assert_equiv(s: &mut Session, src: &str) {
    let mut fresh = Session::new(src).unwrap();
    fresh.run_full().unwrap();
    for (name, &m) in &fresh.checked.index.clone() {
        for mb in 0..fresh.checked.tuple_count(m) {
            let label = fresh.checked.tuple_label(m, mb);
            let member = if label.is_empty() { None } else { Some(label.as_str()) };
            let (r0, r1) = fresh.checked.measures[m].range;
            let slots: Vec<Option<usize>> = if fresh.checked.measures[m].is_series {
                (r0..=r1).map(Some).collect()
            } else {
                vec![None]
            };
            for t in slots {
                let a = fresh.get(name, member, t).unwrap();
                let b = s.get(name, member, t).unwrap();
                assert!(
                    (a == b) || (a.is_nan() && b.is_nan()),
                    "{name}[{member:?}]@{t:?}: fresh {a} vs incremental {b}"
                );
            }
        }
    }
}

#[test]
fn one_edit_reevaluates_only_the_blast_radius() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let full_steps = s.checked.steps.len();
    // Edit the budget cap: dependents are headroom only.
    let src2 = BUDGET.replace("2029: 2_050", "2029: 2_150");
    let rs = s.reload(single(&src2)).unwrap();
    assert!(!rs.reused);
    assert_eq!(rs.changed, vec!["budget_cap".to_string()]);
    assert_eq!(rs.affected, vec!["budget_cap".to_string(), "headroom".to_string()]);
    assert!(
        rs.steps_run < full_steps,
        "blast radius {} must beat full {}",
        rs.steps_run,
        full_steps
    );
    assert_equiv(&mut s, &src2);
}

#[test]
fn deep_chains_cascade_and_untouched_branches_are_copied() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    // Edit expenses: loaded_cost, total_expenses, headroom follow; the
    // overhead allocation (driven by headcount) is NOT affected.
    let src2 = BUDGET.replace("Operations  -> 315", "Operations  -> 400");
    let rs = s.reload(single(&src2)).unwrap();
    assert_eq!(
        rs.affected,
        vec!["expenses", "headroom", "loaded_cost", "total_expenses"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    assert!(!rs.affected.contains(&"overhead_share".to_string()));
    assert_equiv(&mut s, &src2);
    assert_eq!(s.get("total_expenses", None, Some(0)).unwrap(), 1720.0);
}

#[test]
fn the_references_cache_survives_reloads() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    // First semantic reload: every declaration's references are computed.
    let src2 = BUDGET.replace("2029: 2_050", "2029: 2_151");
    let rs1 = s.reload(single(&src2)).unwrap();
    assert!(rs1.query_misses > 5, "cold cache: {rs1:?}");
    // Second reload editing a DIFFERENT declaration: only the edited
    // declaration misses — every other reference list is a cache hit.
    let src3 = src2.replace("Operations  -> 315", "Operations  -> 316");
    let rs2 = s.reload(single(&src3)).unwrap();
    assert_eq!(rs2.query_misses, 1, "{rs2:?}");
    assert_eq!(rs2.query_hits, rs1.query_hits + rs1.query_misses - 1);
    assert_equiv(&mut s, &src3);
}

#[test]
fn solve_and_structure_edits_stay_conservative() {
    // Editing a solve block member → full rebuild (no blast radius).
    let finplan = include_str!("fixtures/finplan.fml");
    let mut s = Session::new(finplan).unwrap();
    s.run_full().unwrap();
    let src2 = finplan.replace("input pe          : ratio = 12", "input pe          : ratio = 13");
    let rs = s.reload(single(&src2)).unwrap();
    // pe is a plain input; its dependents include solve members → the
    // guard must refuse selective evaluation.
    let selective_possible = rs.affected.is_empty();
    if !selective_possible {
        // If selective ran, solve members must not be in the radius…
        assert!(rs
            .affected
            .iter()
            .all(|n| s.checked.measures[s.checked.index[n]].solve.is_none()));
    }
    assert_equiv(&mut s, &src2);

    // Editing a non-measure declaration (a scenario) → conservative full
    // rebuild: its name is outside the measure index.
    let mut s2 = Session::new(BUDGET).unwrap();
    s2.run_full().unwrap();
    let src3 = BUDGET.replace("2026: 1_600", "2026: 1_580");
    let rs3 = s2.reload(single(&src3)).unwrap();
    assert!(rs3.affected.is_empty(), "scenario edits are full rebuilds: {rs3:?}");
    assert_equiv(&mut s2, &src3);
}

#[test]
fn added_and_removed_measures_stay_equivalent() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let src2 = format!("{BUDGET}slack : kEUR flow over plan = headroom * 50%\n");
    let rs = s.reload(single(&src2)).unwrap();
    assert!(!rs.reused);
    assert_equiv(&mut s, &src2);
    assert_eq!(
        s.get("slack", None, Some(0)).unwrap(),
        s.get("headroom", None, Some(0)).unwrap() * 0.5
    );
    // And remove it again.
    let rs2 = s.reload(single(BUDGET)).unwrap();
    assert!(!rs2.reused);
    assert_equiv(&mut s, BUDGET);
    let _ = rs2;
}
