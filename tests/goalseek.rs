//! Goal-seek — the IFPS classic: which input value makes an output hit a
//! target? Safeguarded secant over runtime values, fully restored.

use fml::Session;

const ROLLING: &str = include_str!("../models/rolling.fml");

#[test]
fn nonlinear_goal_through_compounding_growth() {
    // fy_profit(growth) compounds 6 forecast months — properly nonlinear.
    let mut s = Session::new(ROLLING).unwrap();
    s.run_full().unwrap();
    let base = s.get("fy_profit", None, None).unwrap();
    let r = s
        .goal_seek("growth", None, None, "fy_profit", None, None, 350.0)
        .unwrap();
    assert!((r.achieved - 350.0).abs() < 1e-6, "achieved {}", r.achieved);
    assert!((0.07..0.10).contains(&r.value), "growth {} not ~8.4%", r.value);
    // The model is fully restored — median world untouched.
    assert_eq!(s.get("fy_profit", None, None).unwrap(), base);
    // Independent verification: apply the solution and re-read.
    s.set_input("growth", None, None, r.value).unwrap();
    s.recalc().unwrap();
    assert!((s.get("fy_profit", None, None).unwrap() - 350.0).abs() < 1e-6);
}

#[test]
fn linear_goal_is_exact_and_fast() {
    // headroom@2029 = budget_cap - Σ spends: linear in any one spend.
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
    // Base headroom 2029 = 2050 - 1955 = 95. Target 200 → marketing must
    // drop by 105: 540 - 105 = 435.
    let r = s
        .goal_seek("marketing_spend", None, Some(3), "headroom", None, Some(3), 200.0)
        .unwrap();
    assert!((r.value - 435.0).abs() < 1e-6, "lever {}", r.value);
    assert!((r.achieved - 200.0).abs() < 1e-9);
    assert!(r.iterations <= 4, "linear should converge immediately, took {}", r.iterations);
    // Other periods' spends are untouched levers — 2026 headroom stays 65.
    assert_eq!(s.get("headroom", None, Some(0)).unwrap(), 65.0);
}

#[test]
fn unresponsive_levers_are_rejected() {
    // 2029 marketing spend cannot move 2026 headroom (no cross-period dep).
    let mut s = Session::new(include_str!("../models/budget.fml")).unwrap();
    s.run_full().unwrap();
    let err = s
        .goal_seek(
            "expenses",
            Some("Marketing"),
            Some(3),
            "headroom",
            None,
            Some(0),
            100.0,
        )
        .expect_err("must not respond");
    assert!(err.contains("does not respond"), "err: {err}");
}

#[test]
fn goal_seek_through_a_solve_block() {
    // FINPLAN: the financing fixpoint sits between the lever and the
    // target — every secant evaluation re-runs the Gauss–Seidel solve.
    let mut s = Session::new(include_str!("../models/finplan.fml")).unwrap();
    s.run_full().unwrap();
    // `price` is computed inside the financing fixpoint; `ebit_margin` is
    // an upstream input. Every secant evaluation re-solves the SCC.
    let solved = s.checked.index["price"];
    assert!(s.checked.measures[solved].solve.is_some(), "price is solve-defined");
    let t = s.checked.measures[solved].range.1;
    let base = s.get("price", None, Some(t)).unwrap();
    let target = base * 1.10;
    let r = s
        .goal_seek("ebit_margin", None, None, "price", None, Some(t), target)
        .unwrap();
    assert!((r.achieved - target).abs() < target.abs() * 1e-6, "achieved {}", r.achieved);
    assert!(r.value > 0.12, "a higher share price needs a higher margin: {}", r.value);
    assert_eq!(s.get("price", None, Some(t)).unwrap(), base, "restored");
}

#[test]
fn misuse_is_reported() {
    let mut s = Session::new(ROLLING).unwrap();
    s.run_full().unwrap();
    let e1 = s.goal_seek("ghost", None, None, "fy_profit", None, None, 1.0).unwrap_err();
    assert!(e1.contains("unknown measure"));
    let e2 = s.goal_seek("profit", None, Some(0), "fy_profit", None, None, 1.0).unwrap_err();
    assert!(e2.contains("not an input"), "err: {e2}");
}
