//! CST slice 3: edit-site spans are DERIVED from the tree via token
//! paths — nothing is stored positionally, nothing shifts. Replacements
//! re-lex to the same token count, so paths stay valid across any edit
//! sequence.

use fml::Session;

#[test]
fn many_length_changing_patches_in_one_declaration() {
    // All four sites live in ONE input declaration (a map literal); the
    // old code had to shift the later spans after every edit. Now the
    // paths never move — hammer it with growing and shrinking values.
    let src = "model demo.paths\ncalendar plan = yearly 2026 .. 2029\ncurrency EUR\n\
        input cap : EUR flow over plan = { 2026: 100, 2027: 200, 2028: 300, 2029: 400 }\n\
        tot : EUR = sum[plan](cap)\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    let seq = [(0usize, 123456.0), (2, 1.0), (1, 99.5), (3, 7777777.0), (0, 5.0), (2, 42.0)];
    for (t, v) in seq {
        s.patch_input("cap", None, Some(t), v).unwrap();
        s.recalc().unwrap();
    }
    assert!(s.source().contains("{ 2026: 5, 2027: 99.5, 2028: 42, 2029: 7777777 }"), "{}", s.source());
    assert_eq!(s.get("tot", None, None).unwrap(), 5.0 + 99.5 + 42.0 + 7777777.0);
    // The round-trip theorem, after the whole sequence.
    let fresh = fml::run(s.source()).unwrap();
    let tot = fresh.scalars.iter().find(|(n, _)| n == "tot").unwrap().1;
    assert_eq!(tot, s.get("tot", None, None).unwrap());
}

#[test]
fn qty_literals_replace_three_tokens_for_three() {
    // `0.01 USD` is Num·Ws·Ident — the replacement must be too, keeping
    // every other path in the declaration untouched.
    let src = "model demo.qty\ncalendar plan = yearly 2026 .. 2027\ncurrency USD\n\
        input fee : USD flow over plan = 0.01 USD\n\
        input n : 1 flow over plan = 100\n\
        payout : USD flow over plan = fee * n\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    s.patch_input("fee", None, None, 0.25).unwrap();
    s.recalc().unwrap();
    assert!(s.source().contains("= 0.25 USD"), "{}", s.source());
    assert_eq!(s.get("payout", None, Some(0)).unwrap(), 25.0);
    // And the neighbouring input still patches at its (unshifted) path.
    s.patch_input("n", None, None, 400.0).unwrap();
    s.recalc().unwrap();
    assert_eq!(s.get("payout", None, Some(0)).unwrap(), 100.0);
    let fresh = fml::run(s.source()).unwrap();
    let series: std::collections::HashMap<String, Vec<f64>> = fresh.series.iter().cloned().collect();
    assert_eq!(series["payout"][0], 100.0);
}

#[test]
fn the_session_source_is_always_the_cst_reprint() {
    // Internal coherence: after edits, src == cst reprint == a fresh
    // CST's reprint of src (losslessness is stable under editing).
    let mut s = Session::new(include_str!("fixtures/budget.fml")).unwrap();
    s.run_full().unwrap();
    s.patch_input("expenses", Some("Marketing"), Some(0), 431.5).unwrap();
    s.patch_input("headcount", Some("Engineering"), None, 51.0).unwrap();
    s.recalc().unwrap();
    let reparsed = fml::cst::parse_cst(s.source()).unwrap();
    assert_eq!(reparsed.text(), s.source());
    assert!(s.source().contains("2026: 431.5"));
    assert!(s.source().contains("Engineering -> 51"));
}
