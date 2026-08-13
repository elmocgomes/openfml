//! The ACME industrial budget — the complete multi-department example:
//! product-line revenue, direct costs, allocated overhead, personnel,
//! capex with a depreciation roll-forward, EBITDA, and covenants.

use std::collections::HashMap;

fn session() -> fml::Session {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models/acme");
    let raw = std::fs::read_to_string(base.join("industrial_budget.fml")).unwrap();
    let exp = fml::expand_includes_with_map("industrial_budget.fml", &raw, &mut |p| {
        std::fs::read_to_string(base.join(p)).map_err(|e| format!("{p}: {e}"))
    })
    .unwrap();
    let mut s = fml::Session::new_expanded_resolve(exp, &mut |f: &str| {
        std::fs::read_to_string(base.join(f)).map_err(|e| e.to_string())
    })
    .unwrap();
    s.run_full().unwrap();
    s
}

#[test]
fn the_industrial_budget_holds_together() {
    let mut s = session();
    // Q1 revenue: 11500·1.85 + 3300·4.2 + 4800·2.4 = 46_655.
    assert!((s.get("total_revenue", None, Some(0)).unwrap() - 46_655.0).abs() < 1e-6);
    // The depreciation roll-forward: Q1 = 2.5% of the opening asset base.
    assert_eq!(s.get("depreciation", None, Some(0)).unwrap(), 300.0);
    assert_eq!(s.get("asset_base", None, Some(0)).unwrap(), 12_000.0 + 1_100.0 - 300.0);
    // Overhead allocation conserves to the cent across lines.
    let alloc: f64 = ["Pipes", "Valves", "Fittings"]
        .iter()
        .map(|l| s.get("overhead_alloc", Some(l), Some(0)).unwrap())
        .sum();
    assert!((alloc - 3_300.0).abs() < 0.005 + 1e-9);
    // All four covenants pass.
    let asserts = s.run_asserts().unwrap();
    assert_eq!(asserts.len(), 4);
    assert!(asserts.iter().all(|a| a.passed), "{asserts:?}");
    // Departments own their inputs: a sales patch reprices one line…
    s.patch_input("price", Some("Valves"), None, 4.5).unwrap();
    s.recalc().unwrap();
    let files: HashMap<String, String> =
        s.files().iter().map(|f| (f.name.clone(), f.text.clone())).collect();
    assert!(files["sales_plan.fml"].contains("Valves   -> 4.5"), "landed in the sales file");
    assert!(!files["production_plan.fml"].contains("4.5"), "production file untouched");
    // …and the P&L follows through.
    assert!((s.get("total_revenue", None, Some(0)).unwrap() - (46_655.0 + 3_300.0 * 0.3)).abs() < 1e-6);
    // The Downturn scenario cuts prices and raises energy costs.
    let (vals, _) = s.eval_scenario("Downturn").unwrap();
    let ebitda_m = s.checked.index["ebitda"];
    assert!(vals[ebitda_m][0][0] < s.get("ebitda", None, Some(0)).unwrap());
}
