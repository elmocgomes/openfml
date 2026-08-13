//! Per-file trees (CST slice 6): every file is its own lossless CST,
//! patches edit BOTH trees by path, and the segment map is derived on
//! demand — no positional state is maintained anywhere. The lockstep
//! invariant: expand(file texts) == flat source, always.

use openfml::Session;

fn team_session() -> Session {
    let files = [
        ("team_marketing.fml", include_str!("fixtures/team_marketing.fml")),
        ("team_engineering.fml", include_str!("fixtures/team_engineering.fml")),
        ("team_operations.fml", include_str!("fixtures/team_operations.fml")),
    ];
    let exp = openfml::expand_includes_with_map(
        "team_budget.fml",
        include_str!("fixtures/team_budget.fml"),
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
    s
}

fn assert_lockstep(s: &Session) {
    let files: Vec<_> = s.files().to_vec();
    let re = openfml::expand_includes(&files[0].text, &mut |p| {
        files
            .iter()
            .find(|f| f.name == p)
            .map(|f| f.text.clone())
            .ok_or_else(|| format!("missing {p}"))
    })
    .unwrap();
    assert_eq!(re, s.source(), "expand(file texts) must equal the flat source");
}

#[test]
fn lockstep_invariant_survives_patch_sequences() {
    let mut s = team_session();
    let seq: [(&str, Option<usize>, f64); 6] = [
        ("marketing_spend", Some(0), 123456.0),
        ("operations_spend", None, 999.5),
        ("engineering_spend", Some(3), 7.0),
        ("marketing_spend", Some(0), 1.0),
        ("budget_cap", Some(2), 2222.25),
        ("engineering_spend", Some(0), 42.0),
    ];
    for (name, t, v) in seq {
        s.patch_input(name, None, t, v).unwrap();
        s.recalc().unwrap();
        assert_lockstep(&s);
    }
    // And the round-trip theorem still closes the loop.
    let fresh = openfml::run(s.source()).unwrap();
    let series: std::collections::HashMap<String, Vec<f64>> = fresh.series.iter().cloned().collect();
    assert_eq!(series["total_expenses"][0], s.get("total_expenses", None, Some(0)).unwrap());
}

#[test]
fn structural_edits_after_patches_use_fresh_segments() {
    // The old code kept segments valid by shifting them on every patch;
    // now they are recomputed — prove a structural edit lands correctly
    // AFTER length-changing patches moved everything around.
    let mut s = team_session();
    s.patch_input("marketing_spend", None, Some(0), 987654.0).unwrap();
    s.recalc().unwrap();
    let (files, label) = s.add_period().unwrap();
    assert_eq!(label, "2030");
    let mkt = files.iter().find(|(n, _)| n == "team_marketing.fml").unwrap();
    assert!(
        mkt.1.contains("{ 2026: 987654, 2027: 460, 2028: 500, 2029: 540, 2030: 540 }"),
        "{}",
        mkt.1
    );
    // Rename after a patch also routes through fresh segments.
    let renamed = s.rename_measure("operations_spend", "ops").unwrap();
    let master = renamed.iter().find(|(n, _)| n == "team_budget.fml").unwrap();
    assert!(master.1.contains("engineering_spend + ops"));
}

#[test]
fn single_file_sessions_keep_source_and_file_identical() {
    let src = "model demo.one\ncalendar plan = yearly 2026 .. 2027\ncurrency EUR\n\
        input x : EUR flow over plan = { 2026: 5, 2027: 6 }\ny : EUR flow over plan = x * 2\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    s.patch_input("x", None, Some(0), 123.5).unwrap();
    s.recalc().unwrap();
    assert_eq!(s.source(), s.files()[0].text, "single-file: flat IS the file");
    assert!(s.source().contains("2026: 123.5"));
}

#[test]
fn explain_locations_stay_correct_after_earlier_edits_grow() {
    // locate_line derives from fresh segments: grow an early literal in
    // the marketing file, then locate a declaration in a LATER file.
    let mut s = team_session();
    s.patch_input("marketing_spend", None, Some(0), 111222333.0).unwrap();
    s.recalc().unwrap();
    let ex = s.explain("engineering_spend", None, Some(0)).unwrap();
    assert_eq!(ex.file, "team_engineering.fml");
    assert_eq!(ex.line, 2, "the declaration is still line 2 of its own file");
    let exm = s.explain("total_expenses", None, Some(0)).unwrap();
    assert_eq!(exm.file, "team_budget.fml");
}
