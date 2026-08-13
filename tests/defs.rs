//! User defs: `def name(params) -> ret = expr`. The extension mechanism
//! is growth-by-desugaring — calls EXPAND into the core before checking,
//! so the theorem to hold is: a def call ≡ the hand-written expansion,
//! bit for bit. Definition-site unit soundness is checked with skolem
//! units; recursion is rejected; unknown calls name what IS in scope.

use openfml::Session;

const BASE: &str = "model demo.defs
calendar y = yearly 2026 .. 2029
currency kEUR

def dcf(cash : $C flow, rate : rate, horizon : range) -> $C =
  npv(rate, cash over horizon)
def gordon_tv(final_cash : $C, rate : rate, g : rate) -> $C =
  final_cash * (1 + g) / (rate - g)
def ev(cash : $C flow, final_cash : $C, rate : rate, g : rate, horizon : range) -> $C =
  dcf(cash, rate, horizon) + gordon_tv(final_cash, rate, g)

input fcf : kEUR flow over y = { 2026: 100, 2027: 110, 2028: 120, 2029: 130 }
input r : rate = 9%
input g : rate = 2%
value : kEUR = ev(fcf, fcf[y.end], r, g, y)
";

#[test]
fn a_def_call_equals_its_hand_written_expansion() {
    let mut s = Session::new(BASE).unwrap();
    s.run_full().unwrap();
    let via_def = s.get("value", None, None).unwrap();

    let manual = BASE.replace(
        "value : kEUR = ev(fcf, fcf[y.end], r, g, y)",
        "value : kEUR = npv(r, fcf over y) + fcf[y.end] * (1 + g) / (r - g)",
    );
    let mut m = Session::new(&manual).unwrap();
    m.run_full().unwrap();
    assert_eq!(via_def, m.get("value", None, None).unwrap(), "expansion ≡ hand-written, exactly");
    assert!(via_def > 0.0);
}

#[test]
fn definition_site_skolem_check_rejects_unit_unsound_defs() {
    // x + y with DISTINCT unit variables cannot be sound for all
    // instantiations — refused at the definition, before any call.
    let bad = "model demo.bad
calendar y = yearly 2026 .. 2027
currency kEUR
def mix(x : $C, y2 : $D) -> $C = x + y2
input a : kEUR = 1
out : kEUR = a
";
    let err = Session::new(bad).err().unwrap();
    assert!(err.contains("mix") && err.contains("unit-sound"), "{err}");
}

#[test]
fn recursion_is_rejected() {
    let rec = "model demo.rec
calendar y = yearly 2026 .. 2027
currency kEUR
def f(x : $C) -> $C = f(x)
input a : kEUR = 1
out : kEUR = a
";
    let err = Session::new(rec).err().unwrap();
    assert!(err.contains("recursive") && err.contains("DAG"), "{err}");
}

#[test]
fn unknown_calls_name_the_defs_in_scope() {
    let m = "model demo.unknown
calendar y = yearly 2026 .. 2027
currency kEUR
def dcf(cash : $C flow, rate : rate, horizon : range) -> $C = npv(rate, cash over horizon)
input a : kEUR flow over y = 1
out : kEUR flow over y = dfc(a)
";
    let err = Session::new(m).err().unwrap();
    assert!(err.contains("unknown function 'dfc'") && err.contains("dcf"), "{err}");
}

#[test]
fn arity_is_checked_at_the_call() {
    let m = BASE.replace("ev(fcf, fcf[y.end], r, g, y)", "ev(fcf, r)");
    let err = Session::new(&m).err().unwrap();
    assert!(err.contains("takes 5 arguments, got 2"), "{err}");
}

#[test]
fn name_positions_require_bare_names() {
    // `x` sits under prev() inside the def — a NAME position; passing an
    // arithmetic expression for it cannot expand.
    let m = "model demo.namepos
calendar y = yearly 2026 .. 2029
currency kEUR
def chg(x : $C flow) -> $C flow = x - prev(x) init 0
input fcf : kEUR flow over y = { 2026: 100, 2027: 110, 2028: 120, 2029: 130 }
d : kEUR flow over y = chg(fcf * 2)
";
    let err = Session::new(m).err().unwrap();
    assert!(err.contains("bare name"), "{err}");
    // …and with a bare name it expands and runs.
    let ok = m.replace("chg(fcf * 2)", "chg(fcf) * 2");
    let mut s = Session::new(&ok).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("d", None, Some(1)).unwrap(), 20.0);
}

#[test]
fn provenance_reaches_through_defs() {
    // The expanded graph is the real graph: `value` depends on the call
    // arguments, not on an opaque function node.
    let mut s = Session::new(BASE).unwrap();
    s.run_full().unwrap();
    let info = openfml::json::parse(&s.model_info_json()).unwrap();
    let value = match info.get("measures").unwrap() {
        openfml::json::J::A(ms) => ms
            .iter()
            .find(|m| matches!(m.get("name"), Some(openfml::json::J::S(n)) if n == "value"))
            .unwrap()
            .clone(),
        _ => panic!(),
    };
    let refs: Vec<String> = match value.get("refs").unwrap() {
        openfml::json::J::A(v) => v
            .iter()
            .map(|x| match x {
                openfml::json::J::S(s) => s.clone(),
                _ => panic!(),
            })
            .collect(),
        _ => panic!(),
    };
    assert!(refs.contains(&"fcf".to_string()) && refs.contains(&"r".to_string()), "{refs:?}");
}

#[test]
fn what_if_and_inverse_work_through_defs() {
    // set_input flows through the expanded graph incrementally; goal-seek
    // inverts THROUGH the def.
    let mut s = Session::new(BASE).unwrap();
    s.run_full().unwrap();
    let v0 = s.get("value", None, None).unwrap();
    s.set_input("r", None, None, 0.12).unwrap();
    s.recalc().unwrap();
    let v1 = s.get("value", None, None).unwrap();
    assert!(v1 < v0, "a higher discount rate lowers the valuation");
    let gs = s.goal_seek("r", None, None, "value", None, None, v0).unwrap();
    assert!((gs.achieved - v0).abs() < 1e-6 * v0.abs());
    assert!((gs.value - 0.09).abs() < 1e-6, "goal-seek recovers the original rate: {}", gs.value);
}
