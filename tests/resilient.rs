//! Error-resilient parsing (CST slice 2): a broken declaration becomes an
//! ErrorDecl — the rest of the file stays parsed, the CST still reprints
//! byte-exactly, and the SALVAGED model (broken declarations plus their
//! transitive dependents dropped) still checks and runs.

use fml::cst::{decl_name, parse_cst, Red, SyntaxKind};
use fml::{parse_salvage, Parser, Session};

const BROKEN: &str = "model demo.res\n\
calendar plan = yearly 2026 .. 2027\n\
currency EUR\n\
input g : rate = 5%\n\
ok1 : EUR flow over plan = 100\n\
bad : EUR flow over plan = 100 +*/ 3\n\
dep : EUR flow over plan = bad * 2\n\
dep2 : EUR flow over plan = dep + ok1\n\
assert tie over plan : dep2 == 0 ± 1\n\
ok2 : EUR flow over plan = ok1 * (1 + g)\n\
scenario Squeeze from Base { dep = 5 }\n";

#[test]
fn broken_declarations_are_skipped_not_fatal() {
    let (model, spans, errors) = Parser::parse_resilient(BROKEN).unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].line, 6, "the broken declaration's first line");
    assert_eq!(spans.iter().filter(|(_, _, t)| *t == "error").count(), 1);
    // Everything AFTER the broken declaration parsed normally.
    let names: Vec<&str> = model
        .items
        .iter()
        .filter_map(|it| match it {
            fml::ast::Item::Measure(m) => Some(m.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"dep") && names.contains(&"ok2"), "{names:?}");
    assert!(!names.contains(&"bad"), "the broken one is absent");
}

#[test]
fn the_cst_exists_and_reprints_even_for_broken_files() {
    let cst = parse_cst(BROKEN).unwrap();
    assert_eq!(cst.text(), BROKEN, "losslessness survives the error");
    let root = Red::root(&cst);
    let kinds: Vec<SyntaxKind> = root.decls().iter().map(|d| d.green.kind).collect();
    assert!(kinds.contains(&SyntaxKind::ErrorDecl));
    // The error node covers exactly the broken declaration's text.
    let err = root
        .decls()
        .into_iter()
        .find(|d| d.green.kind == SyntaxKind::ErrorDecl)
        .unwrap();
    assert!(err.green.text().contains("bad : EUR"), "{}", err.green.text());
    assert!(!err.green.text().contains("dep :"), "the next declaration is NOT swallowed");
}

#[test]
fn fragment_files_without_a_header_get_a_cst() {
    // A team-owned include fragment: no `model` line, units declared in
    // the master file — slice 1 couldn't parse this at all.
    let frag = include_str!("../models/team_marketing.fml");
    let cst = parse_cst(frag).unwrap();
    assert_eq!(cst.text(), frag);
    let (_, _, errors) = Parser::parse_resilient(frag).unwrap();
    assert!(errors.iter().any(|e| e.msg.contains("model")), "missing header is recorded");
}

#[test]
fn salvage_drops_the_transitive_dependents_and_runs() {
    let sal = parse_salvage(BROKEN).unwrap();
    assert_eq!(sal.errors.len(), 1);
    let dropped: Vec<&str> = sal.dropped.iter().map(|(w, _)| w.as_str()).collect();
    assert!(dropped.contains(&"dep"), "{dropped:?}");
    assert!(dropped.contains(&"dep2"), "cascade: {dropped:?}");
    assert!(dropped.contains(&"assert tie"), "asserts cascade too: {dropped:?}");
    assert!(dropped.contains(&"scenario Squeeze"), "scenarios cascade too: {dropped:?}");
    // The salvaged model checks and evaluates — the intact measures live.
    let mut s = Session::from_model_parts(
        &sal.model,
        BROKEN.to_string(),
        vec![fml::SourceFile { name: "model".into(), text: BROKEN.to_string() }],
        vec![fml::Segment { flat_start: 0, flat_end: BROKEN.len(), file: 0, local_start: 0 }],
    )
    .unwrap();
    s.run_full().unwrap();
    assert_eq!(s.get("ok1", None, Some(0)).unwrap(), 100.0);
    assert_eq!(s.get("ok2", None, Some(0)).unwrap(), 105.0);
}

#[test]
fn recovery_works_at_the_first_and_last_declaration() {
    let first_broken = "model demo.f\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n\
        junk junk : :\nok : EUR flow over plan = 7\n";
    let (model, _, errors) = Parser::parse_resilient(first_broken).unwrap();
    assert_eq!(errors.len(), 1);
    assert!(model.items.iter().any(|it| matches!(it, fml::ast::Item::Measure(m) if m.name == "ok")));

    let last_broken = "model demo.l\ncalendar plan = yearly 2026 .. 2026\ncurrency EUR\n\
        ok : EUR flow over plan = 7\nbad : EUR flow over plan = ((\n";
    let (model2, _, errors2) = Parser::parse_resilient(last_broken).unwrap();
    assert_eq!(errors2.len(), 1);
    assert!(model2.items.iter().any(|it| matches!(it, fml::ast::Item::Measure(m) if m.name == "ok")));
}

#[test]
fn intact_files_are_untouched_by_resilient_mode() {
    for src in [
        include_str!("../models/budget.fml"),
        include_str!("../models/rolling.fml"),
        include_str!("../models/finplan.fml"),
    ] {
        let (_, _, errors) = Parser::parse_resilient(src).unwrap();
        assert!(errors.is_empty());
        let sal = parse_salvage(src).unwrap();
        assert!(sal.dropped.is_empty(), "{:?}", sal.dropped);
    }
}
