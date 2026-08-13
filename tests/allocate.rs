//! The `allocate` primitive: spread a total across a dimension's members
//! in proportion to a driver, with conservation proven by the
//! auto-generated tie-assert.

use openfml::Session;

const HEAD: &str = "model demo.alloc\n\
calendar plan = yearly 2026 .. 2027\n\
dimension CC = tree { Total -> { A, B, C } }\n\
currency kEUR\nunit hc\n";

#[test]
fn proportional_split_conserves_the_total() {
    let src = format!(
        "{HEAD}input pot : kEUR flow over plan = 1_000\n\
         input drv : hc flow over CC, plan = match CC {{ A -> 10  B -> 30  C -> 10 }}\n\
         allocate share : kEUR flow over CC, plan = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("share", Some("A"), Some(0)).unwrap(), 200.0);
    assert_eq!(s.get("share", Some("B"), Some(0)).unwrap(), 600.0);
    assert_eq!(s.get("share", Some("C"), Some(0)).unwrap(), 200.0);
    let asserts = s.run_asserts().unwrap();
    let cons = asserts.iter().find(|a| a.name == "allocate_share").unwrap();
    assert!(cons.passed, "conservation tie-assert must pass");
}

#[test]
fn time_varying_drivers_reshape_the_split() {
    let src = format!(
        "{HEAD}input pot : kEUR flow over plan = {{ 2026: 900, 2027: 1_200 }}\n\
         input drv : hc flow over CC, plan = match CC {{\n\
           A -> {{ 2026: 1, 2027: 2 }}\n\
           B -> {{ 2026: 2, 2027: 1 }}\n\
           C -> 0\n\
         }}\n\
         allocate share : kEUR flow over CC, plan = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("share", Some("A"), Some(0)).unwrap(), 300.0);
    assert_eq!(s.get("share", Some("B"), Some(0)).unwrap(), 600.0);
    assert_eq!(s.get("share", Some("A"), Some(1)).unwrap(), 800.0);
    assert_eq!(s.get("share", Some("B"), Some(1)).unwrap(), 400.0);
    assert_eq!(s.get("share", Some("C"), Some(1)).unwrap(), 0.0);
}

#[test]
fn dimensionless_driver_gives_equal_split() {
    let src = format!(
        "{HEAD}input pot : kEUR flow over plan = 900\n\
         allocate share : kEUR flow over CC, plan = pot by 1\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    for m in ["A", "B", "C"] {
        assert_eq!(s.get("share", Some(m), Some(0)).unwrap(), 300.0);
    }
}

#[test]
fn zero_driver_sum_is_a_runtime_error_naming_the_measure() {
    let src = format!(
        "{HEAD}input pot : kEUR flow over plan = 500\n\
         input drv : hc flow over CC, plan = 0\n\
         allocate share : kEUR flow over CC, plan = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    let err = s.run_full().expect_err("0/0 allocation must be surfaced, not silent");
    assert!(err.contains("share") && err.contains("division by zero"), "err: {err}");
}

#[test]
fn allocation_composes_with_the_budget_model() {
    // budget.fml: overhead 300 by headcount {12, 45, 18} (Σ=75).
    let mut s = Session::new(include_str!("fixtures/budget.fml")).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("overhead_share", Some("Marketing"), Some(0)).unwrap(), 300.0 * 12.0 / 75.0);
    assert_eq!(s.get("overhead_share", Some("Engineering"), Some(0)).unwrap(), 180.0);
    // loaded_cost = own expenses + allocated overhead.
    assert_eq!(s.get("loaded_cost", Some("Marketing"), Some(0)).unwrap(), 420.0 + 48.0);
    // Editing a headcount reallocates live (incremental).
    s.set_input("headcount", Some("Marketing"), None, 37.0).unwrap();
    let stats = s.recalc().unwrap();
    assert!(stats.steps_run < stats.steps_total, "incremental, not full");
    assert_eq!(s.get("overhead_share", Some("Marketing"), Some(0)).unwrap(), 300.0 * 37.0 / 100.0);
    let asserts = s.run_asserts().unwrap();
    assert!(asserts.iter().find(|a| a.name == "allocate_overhead_share").unwrap().passed);
}

#[test]
fn explain_shows_the_allocation_basis() {
    let mut s = Session::new(include_str!("fixtures/budget.fml")).unwrap();
    s.run_full().unwrap();
    let ex = s.explain("overhead_share", Some("Marketing"), Some(0)).unwrap();
    let names: Vec<(&str, &str)> = ex.deps.iter().map(|d| (d.name.as_str(), d.via.as_str())).collect();
    assert!(names.contains(&("overhead", "")), "the pot: {names:?}");
    // Marketing's own driver plus every member's driver via the sum basis.
    assert!(ex.deps.iter().any(|d| d.name == "headcount" && d.member == "Marketing" && d.via.is_empty()));
    let basis: Vec<_> = ex.deps.iter().filter(|d| d.name == "headcount" && d.via == "sum").collect();
    assert_eq!(basis.len(), 3, "sum basis lists all members: {names:?}");
}

#[test]
fn misuse_is_a_compile_error() {
    // No dimension in the over clause.
    let bad1 = format!("{HEAD}input pot : kEUR flow over plan = 1\nallocate s : kEUR flow over plan = pot by 1\n");
    let e1 = openfml::compile(&bad1).unwrap_err();
    assert!(e1.contains("exactly one dimension"), "err: {e1}");
    // Missing `by`.
    let bad2 = format!("{HEAD}input pot : kEUR flow over plan = 1\nallocate s : kEUR flow over CC, plan = pot\n");
    let e2 = openfml::compile(&bad2).unwrap_err();
    assert!(e2.contains("by"), "err: {e2}");
}
