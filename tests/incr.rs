//! The salsa-style incremental reload: semantic fingerprints over
//! non-trivia tokens give early cutoff — trivia edits reuse the whole
//! analysis and runtime state (site paths relocated by token ordinal);
//! semantic edits rebuild and name the declarations that forced it.
//! The governing theorem: reload ≡ fresh compile, always.

use openfml::{Expanded, Segment, Session, SourceFile};

fn single(src: &str) -> Expanded {
    Expanded {
        flat: src.to_string(),
        files: vec![SourceFile { name: "model".into(), text: src.to_string() }],
        segments: vec![Segment { flat_start: 0, flat_end: src.len(), file: 0, local_start: 0 }],
    }
}

const BUDGET: &str = include_str!("fixtures/budget.fml");

#[test]
fn trivia_edits_reuse_the_analysis_and_runtime() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let before = s.get("headroom", None, Some(0)).unwrap();
    // Insert a comment block and extra blank lines mid-file.
    let src2 = BUDGET.replace(
        "total_expenses :",
        "// reviewed by the CFO on 2026-08-08\n// (no numbers were harmed)\n\ntotal_expenses :",
    );
    let rs = s.reload(single(&src2)).unwrap();
    assert!(rs.reused, "comment edits must not recompile");
    assert_eq!(rs.steps_run, 0);
    assert!(rs.changed.is_empty());
    assert_eq!(s.source(), src2);
    assert_eq!(s.get("headroom", None, Some(0)).unwrap(), before, "runtime state kept");
    // Site paths were RELOCATED: a grid patch after the shift lands right.
    s.patch_input("expenses", Some("Marketing"), Some(0), 425.0).unwrap();
    s.recalc().unwrap();
    assert!(s.source().contains("2026: 425"), "{}", s.source());
    assert!(s.source().contains("// reviewed by the CFO"), "comment survives the patch");
    let fresh = openfml::run(s.source()).unwrap();
    let series: std::collections::HashMap<String, Vec<f64>> = fresh.series.iter().cloned().collect();
    assert_eq!(series["headroom"][0], s.get("headroom", None, Some(0)).unwrap());
    // Explain sees the SHIFTED line numbers.
    let ex = s.explain("total_expenses", None, Some(0)).unwrap();
    let expect_line = 1 + s.source()[..s.source().find("total_expenses :").unwrap()]
        .matches('\n')
        .count();
    assert_eq!(ex.line, expect_line, "declaration lines refreshed on reuse");
}

#[test]
fn semantic_edits_rebuild_and_name_the_culprit() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let src2 = BUDGET.replace("Operations  -> 315", "Operations  -> 320");
    let rs = s.reload(single(&src2)).unwrap();
    assert!(!rs.reused);
    assert_eq!(rs.changed, vec!["expenses".to_string()]);
    assert!(rs.steps_run > 0);
    // Equivalence: reload result == fresh session on the same source.
    let mut fresh = Session::new(&src2).unwrap();
    fresh.run_full().unwrap();
    for name in ["total_expenses", "headroom", "overhead_share"] {
        let member = if name == "overhead_share" { Some("Marketing") } else { None };
        assert_eq!(
            s.get(name, member, Some(0)).unwrap(),
            fresh.get(name, member, Some(0)).unwrap(),
            "{name}"
        );
    }
    assert_eq!(s.get("total_expenses", None, Some(0)).unwrap(), 1640.0);
}

#[test]
fn reordered_declarations_are_not_reused() {
    // Same content, different order: order matters (evaluation, solve
    // blocks) — the conservative cutoff must refuse reuse.
    let src = "model demo.ord\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n\
        input a : EUR flow over plan = 1\ninput b : EUR flow over plan = 2\nt : EUR flow over plan = a + b\n";
    let swapped = "model demo.ord\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n\
        input b : EUR flow over plan = 2\ninput a : EUR flow over plan = 1\nt : EUR flow over plan = a + b\n";
    let mut s = Session::new(src).unwrap();
    s.run_full().unwrap();
    let rs = s.reload(single(swapped)).unwrap();
    assert!(!rs.reused);
    assert_eq!(s.get("t", None, Some(0)).unwrap(), 3.0);
}

#[test]
fn multi_file_trivia_reuse_and_cross_file_move_guard() {
    let mk = include_str!("fixtures/team_marketing.fml");
    let eng = include_str!("fixtures/team_engineering.fml");
    let ops = include_str!("fixtures/team_operations.fml");
    let build = |mk: &str, eng: &str, ops: &str| -> Expanded {
        let files = [("team_marketing.fml", mk), ("team_engineering.fml", eng), ("team_operations.fml", ops)];
        openfml::expand_includes_with_map(
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
        .unwrap()
    };
    let mut s = Session::new_expanded(build(mk, eng, ops)).unwrap();
    s.run_full().unwrap();
    // Trivia edit inside ONE team file → full reuse.
    let mk2 = mk.replace("// Marketing — owned by the Marketing lead.", "// Marketing — owned by Rita.");
    let rs = s.reload(build(&mk2, eng, ops)).unwrap();
    assert!(rs.reused, "team-file comment edit reuses everything");
    // A patch into the edited file still lands at the right tokens.
    s.patch_input("marketing_spend", None, Some(1), 465.0).unwrap();
    s.recalc().unwrap();
    assert!(s.files()[1].text.contains("2027: 465"), "{}", s.files()[1].text);
    assert!(s.files()[1].text.contains("owned by Rita"));
    // Moving a declaration BETWEEN files (flat order intact) must NOT
    // reuse: file ownership changed even though the flat text agrees.
    // (build from the on-disk texts: the moved declaration carries the
    // original 460 — a semantic change vs the patched 465 regardless.)
    let mk3 = mk2.replace("input marketing_spend : kEUR flow over plan = { 2026: 420, 2027: 460, 2028: 500, 2029: 540 }\n", "");
    assert!(!mk3.contains("marketing_spend"), "declaration removed from the marketing file");
    let eng3 = format!("input marketing_spend : kEUR flow over plan = {{ 2026: 420, 2027: 460, 2028: 500, 2029: 540 }}\n{eng}");
    let rs3 = s.reload(build(&mk3, eng3.as_str(), ops)).unwrap();
    assert!(!rs3.reused, "cross-file moves are a rebuild");
    assert_eq!(s.get("total_expenses", None, Some(1)).unwrap(), 460.0 + 990.0 + 315.0);
}

#[test]
fn alternating_edit_sequence_stays_equivalent_to_fresh() {
    let mut s = Session::new(BUDGET).unwrap();
    s.run_full().unwrap();
    let steps: Vec<String> = vec![
        BUDGET.replace("model demo.budget", "model demo.budget\n// pass 1"),
        BUDGET.replace("model demo.budget", "model demo.budget\n// pass 1")
            .replace("2027: 1_850", "2027: 1_900"),
        BUDGET.replace("model demo.budget", "model demo.budget\n// pass 2 comment only")
            .replace("2027: 1_850", "2027: 1_900"),
    ];
    let mut expected_reused = [true, false, true].iter();
    for src in &steps {
        let rs = s.reload(single(src)).unwrap();
        assert_eq!(rs.reused, *expected_reused.next().unwrap());
        let mut fresh = Session::new(src).unwrap();
        fresh.run_full().unwrap();
        assert_eq!(
            s.get("headroom", None, Some(1)).unwrap(),
            fresh.get("headroom", None, Some(1)).unwrap(),
            "reload ≡ fresh at every step"
        );
    }
}
