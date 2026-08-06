//! Per-file span provenance: grid → text write-back routed into the file
//! that owns the edited literal. The multi-file round-trip theorem:
//! patch + incremental recalc ≡ re-expanding the PATCHED FILES and
//! compiling fresh — with every untouched file byte-identical.

use std::collections::HashMap;

const MAIN: &str = "model demo.multi\n\
calendar plan = yearly 2026 .. 2027\n\
currency kEUR\n\
include \"mkt.fml\"\n\
include \"ops.fml\"\n\
input adj : kEUR flow over plan = 10\n\
total : kEUR flow over plan = mkt + ops + adj\n";
const MKT: &str = "// marketing file\ninput mkt : kEUR flow over plan = { 2026: 420, 2027: 460 }\n";
const OPS: &str = "input ops : kEUR flow over plan = 315\n";

fn resolver(p: &str) -> Result<String, String> {
    let files: HashMap<&str, &str> = HashMap::from([("mkt.fml", MKT), ("ops.fml", OPS)]);
    files.get(p).map(|s| s.to_string()).ok_or_else(|| format!("missing {p}"))
}

fn build() -> fml::Session {
    let exp = fml::expand_includes_with_map("main.fml", MAIN, &mut resolver).unwrap();
    let mut s = fml::Session::new_expanded(exp).unwrap();
    s.run_full().unwrap();
    s
}

/// Re-expand the session's CURRENT files and assert flat-source equality,
/// then compile fresh and assert value equality — the round-trip theorem.
fn assert_round_trip(s: &mut fml::Session) {
    let files: Vec<fml::SourceFile> = s.files().to_vec();
    let re = fml::expand_includes(&files[0].text, &mut |p| {
        files
            .iter()
            .find(|f| f.name == p)
            .map(|f| f.text.clone())
            .ok_or_else(|| format!("missing {p}"))
    })
    .unwrap();
    assert_eq!(re, s.source(), "flat source must equal re-expansion of the patched files");
    let fresh = fml::run(&re).unwrap();
    let series: HashMap<String, Vec<f64>> = fresh.series.iter().cloned().collect();
    for t in 0..2 {
        assert_eq!(
            series["total"][t],
            s.get("total", None, Some(t)).unwrap(),
            "fresh compile must equal incremental state at t={t}"
        );
    }
}

#[test]
fn patch_lands_in_owning_file_only() {
    let mut s = build();
    assert_eq!(
        s.files().iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["main.fml", "mkt.fml", "ops.fml"]
    );
    s.patch_input("mkt", None, Some(0), 425.0).unwrap();
    s.recalc().unwrap();
    // Byte-surgical in the owning file; every other file untouched.
    assert_eq!(s.files()[1].text, MKT.replace("420", "425"));
    assert_eq!(s.files()[0].text, MAIN);
    assert_eq!(s.files()[2].text, OPS);
    assert_eq!(s.get("total", None, Some(0)).unwrap(), 425.0 + 315.0 + 10.0);
    assert_round_trip(&mut s);
}

#[test]
fn patches_across_files_and_main_shift_all_maps() {
    let mut s = build();
    // Length-changing patch in the FIRST included file (420 → 1000) shifts
    // the flat spans and local offsets of everything after it…
    s.patch_input("mkt", None, Some(0), 1000.0).unwrap();
    // …then a broadcast patch in the SECOND file must still land right…
    s.patch_input("ops", None, None, 999.5).unwrap();
    // …and a patch in MAIN-file content that sits AFTER both includes.
    s.patch_input("adj", None, None, 12.25).unwrap();
    s.recalc().unwrap();
    assert_eq!(s.files()[1].text, MKT.replace("420", "1000"));
    assert_eq!(s.files()[2].text, OPS.replace("315", "999.5"));
    assert_eq!(s.files()[0].text, MAIN.replace("= 10", "= 12.25"));
    assert_eq!(s.get("total", None, Some(0)).unwrap(), 1000.0 + 999.5 + 12.25);
    assert_eq!(s.get("total", None, Some(1)).unwrap(), 460.0 + 999.5 + 12.25);
    assert_round_trip(&mut s);
}

#[test]
fn second_patch_in_shifted_file_stays_exact() {
    let mut s = build();
    // Grow the 2026 literal, then patch 2027 in the SAME file: its edit
    // site and the file-local offsets both moved.
    s.patch_input("mkt", None, Some(0), 123456.0).unwrap();
    s.patch_input("mkt", None, Some(1), 7.0).unwrap();
    s.recalc().unwrap();
    assert_eq!(s.files()[1].text, MKT.replace("420", "123456").replace("460", "7"));
    assert_eq!(s.get("total", None, Some(1)).unwrap(), 7.0 + 315.0 + 10.0);
    assert_round_trip(&mut s);
}

#[test]
fn single_file_sessions_still_patch() {
    // Session::new is now a one-file, one-segment special case of the map.
    let src = "model demo.single\ncalendar plan = yearly 2026 .. 2027\ncurrency kEUR\ninput x : kEUR flow over plan = 5\ny : kEUR flow over plan = x * 2\n";
    let mut s = fml::Session::new(src).unwrap();
    s.run_full().unwrap();
    s.patch_input("x", None, None, 8.0).unwrap();
    s.recalc().unwrap();
    assert_eq!(s.files().len(), 1);
    assert_eq!(s.files()[0].text, src.replace("= 5", "= 8"));
    assert_eq!(s.source(), s.files()[0].text);
    assert_eq!(s.get("y", None, Some(0)).unwrap(), 16.0);
}
