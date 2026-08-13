//! Typed rounding (`round dp policy`) and exact allocation remainders
//! (`allocate … round dp`): stored values are exact multiples of the
//! minor unit, and rounded allocations conserve the pot EXACTLY in
//! minor-unit space — the cent-level promise, tested in integer cents.

use openfml::Session;

const HEAD: &str = "model demo.round\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n";

fn cents(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

#[test]
fn round_snaps_at_store_time_and_downstream_sees_posted_amounts() {
    let src = format!(
        "{HEAD}input base : EUR flow over plan = 100.10\n\
         vat : EUR flow over plan round 2 = base * 21%\n\
         gross : EUR flow over plan = base + vat\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    let vat = s.get("vat", None, Some(0)).unwrap();
    // raw 21.021 → posted 21.02; downstream adds the POSTED amount.
    assert_eq!(cents(vat), 2102);
    assert_eq!(cents(s.get("gross", None, Some(0)).unwrap()), 10010 + 2102);
}

#[test]
fn all_four_policies() {
    let src = format!(
        "{HEAD}input x : EUR flow over plan = 0.125\n\
         a : EUR flow over plan round 2 half_up = x\n\
         b : EUR flow over plan round 2 half_even = x\n\
         c : EUR flow over plan round 2 floor = x\n\
         d : EUR flow over plan round 2 ceil = x\n\
         n : EUR flow over plan round 2 = 0 - x\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    assert_eq!(cents(s.get("a", None, Some(0)).unwrap()), 13); // ties away
    assert_eq!(cents(s.get("b", None, Some(0)).unwrap()), 12); // ties to even
    assert_eq!(cents(s.get("c", None, Some(0)).unwrap()), 12);
    assert_eq!(cents(s.get("d", None, Some(0)).unwrap()), 13);
    assert_eq!(cents(s.get("n", None, Some(0)).unwrap()), -13); // symmetric half_up
}

#[test]
fn rounded_inputs_snap_grid_edits() {
    let src = format!("{HEAD}input p : EUR flow over plan round 2 = 10\nq : EUR flow over plan = p * 2\n");
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    s.set_input("p", None, None, 10.006).unwrap();
    s.recalc().unwrap();
    assert_eq!(cents(s.get("p", None, Some(0)).unwrap()), 1001);
    assert_eq!(cents(s.get("q", None, Some(0)).unwrap()), 2002);
}

const DHEAD: &str = "model demo.ralloc\ncalendar plan = yearly 2026 .. 2026\n\
dimension CC = tree { Total -> { A, B, C } }\ncurrency EUR\nunit hc\n";

#[test]
fn equal_thirds_conserve_to_the_cent() {
    // 100.00 / 3 = 33.333… — floats can never re-add this; cents can.
    let src = format!(
        "{DHEAD}input pot : EUR flow over plan = 100\n\
         allocate share : EUR flow over CC, plan round 2 = pot by 1\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    let (a, b, c) = (
        s.get("share", Some("A"), Some(0)).unwrap(),
        s.get("share", Some("B"), Some(0)).unwrap(),
        s.get("share", Some("C"), Some(0)).unwrap(),
    );
    // Largest remainder, tie → member order: A gets the extra cent.
    assert_eq!(cents(a), 3334);
    assert_eq!(cents(b), 3333);
    assert_eq!(cents(c), 3333);
    assert_eq!(cents(a) + cents(b) + cents(c), 10000, "EXACT in cents");
    let asserts = s.run_asserts().unwrap();
    assert!(asserts.iter().find(|x| x.name == "allocate_share").unwrap().passed);
}

#[test]
fn sevenths_give_the_extra_unit_to_the_largest_remainder() {
    // 100 by {1,2,4}: raw 14.2857/28.5714/57.1428 at dp=0 → floors 14/28/57
    // sum 99; the missing unit goes to B (remainder .5714 is largest).
    let src = format!(
        "{DHEAD}input pot : EUR flow over plan = 100\n\
         input drv : hc flow over CC, plan = match CC {{ A -> 1  B -> 2  C -> 4 }}\n\
         allocate share : EUR flow over CC, plan round 0 = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("share", Some("A"), Some(0)).unwrap(), 14.0);
    assert_eq!(s.get("share", Some("B"), Some(0)).unwrap(), 29.0);
    assert_eq!(s.get("share", Some("C"), Some(0)).unwrap(), 57.0);
}

#[test]
fn awkward_pots_still_conserve_exactly() {
    // A pot that is itself sub-cent-dirty: shares sum to round(pot, 2).
    let src = format!(
        "{DHEAD}input pot : EUR flow over plan = 997.774\n\
         input drv : hc flow over CC, plan = match CC {{ A -> 7  B -> 11  C -> 13 }}\n\
         allocate share : EUR flow over CC, plan round 2 = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    let total_cents: i64 = ["A", "B", "C"]
        .iter()
        .map(|m| cents(s.get("share", Some(m), Some(0)).unwrap()))
        .sum();
    assert_eq!(total_cents, 99777, "sum == round(997.774, 2) in cents, exactly");
    let asserts = s.run_asserts().unwrap();
    assert!(asserts.iter().find(|x| x.name == "allocate_share").unwrap().passed);
}

#[test]
fn incremental_edits_preserve_cent_conservation() {
    let src = format!(
        "{DHEAD}input pot : EUR flow over plan = 250.55\n\
         input drv : hc flow over CC, plan = match CC {{ A -> 3  B -> 3  C -> 3 }}\n\
         allocate share : EUR flow over CC, plan round 2 = pot by drv\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    for (pot_cents, drv_a) in [(25055, 5.0), (25055, 1.0), (99999, 7.0)] {
        s.set_input("pot", None, None, pot_cents as f64 / 100.0).unwrap();
        s.set_input("drv", Some("A"), None, drv_a).unwrap();
        s.recalc().unwrap();
        let sum: i64 = ["A", "B", "C"]
            .iter()
            .map(|m| cents(s.get("share", Some(m), Some(0)).unwrap()))
            .sum();
        assert_eq!(sum, pot_cents, "conservation survives incremental edits");
    }
}

#[test]
fn rounding_inside_a_solve_is_a_compile_error() {
    let src = "model demo.badsolve\ncalendar plan = yearly 2026 .. 2027\ncurrency EUR\n\
        input g : rate = 5%\n\
        solve fix {\n  x : EUR flow over plan round 2 = y * 0.5 + 10\n  y : EUR flow over plan = x * g\n}\n";
    let err = openfml::compile(src).unwrap_err();
    assert!(err.contains("solve") && err.contains("round"), "err: {err}");
}

#[test]
fn round_trip_theorem_holds_for_rounded_models() {
    // patch + incremental recalc ≡ fresh compile of the patched source.
    let src = format!(
        "{DHEAD}input pot : EUR flow over plan = 300\n\
         allocate share : EUR flow over CC, plan round 2 = pot by 1\n"
    );
    let mut s = Session::new(&src).unwrap();
    s.run_full().unwrap();
    s.patch_input("pot", None, None, 100.01).unwrap();
    s.recalc().unwrap();
    let fresh = openfml::run(s.source()).unwrap();
    for m in ["A", "B", "C"] {
        let inc = s.get("share", Some(m), Some(0)).unwrap();
        let f = fresh
            .series
            .iter()
            .find(|(n, _)| n == &format!("share[{m}]"))
            .unwrap()
            .1[0];
        assert_eq!(inc, f, "member {m}");
    }
}
