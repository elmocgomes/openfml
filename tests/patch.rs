//! Grid → text write-back: byte-exact span patching of input literals.
//! The round-trip theorem: patching the source and recalculating
//! incrementally must equal compiling the patched source from scratch.

use fml::Session;

const FINPLAN: &str = include_str!("fixtures/finplan.fml");

#[test]
fn patch_roundtrip_equals_fresh_compile() {
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    s.patch_input("g", None, Some(1), 0.20).unwrap(); // 2027: 12% -> 20%
    s.recalc().unwrap();

    // The source now says 20% where it said 12%.
    assert!(s.source().contains("2027: 20%"), "source: {}", &s.source()[..400]);
    assert!(!s.source().contains("2027: 12%"));

    // Fresh compile of the patched source agrees exactly.
    let mut fresh = Session::new(s.source()).unwrap();
    fresh.run_full().unwrap();
    for (m, mi) in s.checked.measures.iter().enumerate() {
        for mb in 0..s.checked.tuple_count(m) {
            for (slot, (a, b)) in s.values[m][mb].iter().zip(fresh.values[m][mb].iter()).enumerate() {
                let same = (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-9;
                assert!(same, "{}[{mb}][{slot}]: patched {a} vs fresh {b}", mi.name);
            }
        }
    }
}

#[test]
fn patch_is_byte_exact_outside_the_span() {
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    let before = s.source().to_string();
    s.patch_input("g", None, Some(2), 0.03).unwrap(); // 2028: 10% -> 3%
    let after = s.source().to_string();
    // Exactly one contiguous difference.
    let p = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let sfx = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    assert_eq!(&before[..p], &after[..p]);
    assert!(before.len() - sfx >= p, "difference is a single contiguous span");
    assert_eq!(&before[before.len() - sfx..], &after[after.len() - sfx..]);
    // The changed region is the minimal digit difference ("10" → "3"; the
    // shared "%" is absorbed into the common suffix).
    assert!(after[p..after.len() - sfx].contains('3'), "changed region: {}", &after[p..after.len() - sfx]);
    assert!(after.contains("2028: 3%"));
}

#[test]
fn broadcast_literal_patch_changes_all_periods() {
    let mut s = Session::new(FINPLAN).unwrap();
    s.run_full().unwrap();
    // pfd_div is a broadcast literal (1_200): editing any period patches the
    // one literal, which by the text's own semantics changes every period.
    s.patch_input("pfd_div", None, Some(2), 1_500.0).unwrap();
    s.recalc().unwrap();
    assert!(s.source().contains("= 1500"), "source: {}", s.source());
    for t in 0..4 {
        assert_eq!(s.get("pfd_div", None, Some(t)).unwrap(), 1_500.0);
    }
    // And two consecutive patches keep spans valid (shifted correctly).
    s.patch_input("g", None, Some(0), 0.18).unwrap();
    s.recalc().unwrap();
    assert!(s.source().contains("2026: 18%"));
    let fresh = fml::run(s.source()).expect("patched source still compiles");
    assert!(fresh.asserts.iter().all(|a| a.passed));
}

#[test]
fn formula_inputs_are_not_literal_editable() {
    let solar = include_str!("fixtures/solar_pf.fml");
    let mut s = Session::new(solar).unwrap();
    s.run_full().unwrap();
    // production is formula-defined: 32_000 * (1 - 0.5%)^(...)
    let err = s.patch_input("production", None, Some(10), 99.0).expect_err("must refuse");
    assert!(err.contains("literal-editable"), "unexpected: {err}");
    // capex map entries ARE editable.
    s.patch_input("capex", None, Some(1), 22_000.0).unwrap();
    assert!(s.source().contains("2026-Q2: 22000"));
}
