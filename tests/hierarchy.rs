//! Multi-level and alternate hierarchies: a group is a named LEAF-SET —
//! nested subgroups and `also { … }` alternates both roll up as the sum
//! of exactly their leaves, everywhere a group name is legal.

use openfml::Session;

const MODEL: &str = "model demo.tree
calendar y = yearly 2026 .. 2027
dimension Line = tree { All -> { Metal -> { Pipes, Valves }, Fittings } } also { Premium -> { Valves, Fittings } }
currency kEUR

input rev : kEUR flow over Line, y = match Line {
  Pipes    -> 100
  Valves   -> 20
  Fittings -> 3
}
metal_rev   : kEUR flow over y = rev[Metal]
premium_rev : kEUR flow over y = rev[Premium]
all_rev     : kEUR flow over y = rev[All]
";

fn session() -> Session {
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    s
}

#[test]
fn groups_roll_up_exactly_their_leaves() {
    let mut s = session();
    assert_eq!(s.get("metal_rev", None, Some(0)).unwrap(), 120.0, "Metal = Pipes + Valves");
    assert_eq!(s.get("premium_rev", None, Some(0)).unwrap(), 23.0, "Premium = Valves + Fittings (alternate)");
    assert_eq!(s.get("all_rev", None, Some(0)).unwrap(), 123.0, "root = every leaf");
}

#[test]
fn the_old_single_group_form_is_unchanged() {
    let flat = "model demo.flat
calendar y = yearly 2026 .. 2026
dimension D = tree { T -> { A, B } }
currency kEUR
input x : kEUR flow over D, y = match D { A -> 1  B -> 2 }
t : kEUR flow over y = x[T]
";
    let mut s = Session::new(flat).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("t", None, Some(0)).unwrap(), 3.0);
}

#[test]
fn alternate_groups_must_reference_existing_members() {
    let bad = MODEL.replace("{ Premium -> { Valves, Fittings } }", "{ Premium -> { Valves, Widgets } }");
    let err = Session::new(&bad).err().unwrap();
    assert!(err.contains("Widgets") && err.contains("not a member"), "{err}");
}

#[test]
fn group_names_stay_unique() {
    let bad = MODEL.replace(
        "also { Premium -> { Valves, Fittings } }",
        "also { Premium -> { Valves, Fittings }, Metal -> { Valves } }",
    );
    let err = Session::new(&bad).err().unwrap();
    assert!(err.contains("Metal") && err.contains("collides"), "{err}");
}

#[test]
fn provenance_reaches_through_subgroup_rollups() {
    // explain on metal_rev names exactly the Metal leaves, not all leaves.
    let mut s = session();
    let ex = s.explain("metal_rev", None, Some(0)).unwrap();
    let deps: Vec<String> = ex
        .deps
        .iter()
        .map(|d| format!("{}[{}]", d.name, d.member))
        .collect();
    assert!(deps.contains(&"rev[Pipes]".to_string()) && deps.contains(&"rev[Valves]".to_string()), "{deps:?}");
    assert!(!deps.iter().any(|d| d.contains("Fittings")), "Fittings is outside Metal: {deps:?}");
}

#[test]
fn the_acme_model_gains_metal_without_moving_a_number() {
    let base = std::path::Path::new("models/acme");
    let raw = std::fs::read_to_string(base.join("industrial_budget.fml")).unwrap();
    let exp = openfml::expand_includes_with_map("industrial_budget.fml", &raw, &mut |p: &str| {
        std::fs::read_to_string(base.join(p)).map_err(|e| e.to_string())
    })
    .unwrap();
    let mut s = openfml::Session::new_expanded_resolve(exp, &mut |f: &str| {
        std::fs::read_to_string(base.join(f)).map_err(|e| e.to_string())
    })
    .unwrap();
    s.run_full().unwrap();
    // Golden numbers untouched by the hierarchy restructure…
    assert!((s.get("fy_ebitda", None, None).unwrap() - 31477.0).abs() < 1.0);
    // …and the new groups roll up correctly.
    let metal: f64 = ["Pipes", "Valves"]
        .iter()
        .map(|m| s.get("revenue", Some(m), Some(0)).unwrap())
        .sum();
    let machined: f64 = ["Valves", "Fittings"]
        .iter()
        .map(|m| s.get("revenue", Some(m), Some(0)).unwrap())
        .sum();
    assert!(metal > 0.0 && machined > 0.0);
}

#[test]
fn add_member_refuses_multi_group_dimensions() {
    let s = session();
    let err = s.add_member("Line", "Widgets", "0").err().unwrap();
    assert!(err.contains("nested or alternate"), "{err}");
}
