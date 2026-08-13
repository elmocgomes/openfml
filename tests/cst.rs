//! The lossless CST, slice 1. The defining theorem, over every model in
//! the repo: `reprint(parse_cst(text)) == text`, byte for byte — comments,
//! whitespace, `1_700` spellings, include directives, everything.

use openfml::cst::{decl_name, parse_cst, GreenChild, Red, SyntaxKind};

const MODELS: &[(&str, &str)] = &[
    ("finplan", include_str!("fixtures/finplan.fml")),
    ("solar_pf", include_str!("fixtures/solar_pf.fml")),
    ("fx_consol", include_str!("fixtures/fx_consol.fml")),
    ("budget", include_str!("fixtures/budget.fml")),
    ("rolling", include_str!("fixtures/rolling.fml")),
    ("team_budget", include_str!("fixtures/team_budget.fml")),
];

#[test]
fn reprint_theorem_over_every_model() {
    for (name, src) in MODELS {
        let cst = parse_cst(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(cst.text(), *src, "{name}: reprint must be byte-identical");
    }
    // And over the include-EXPANDED multi-file model.
    let files = [
        ("team_marketing.fml", include_str!("fixtures/team_marketing.fml")),
        ("team_engineering.fml", include_str!("fixtures/team_engineering.fml")),
        ("team_operations.fml", include_str!("fixtures/team_operations.fml")),
    ];
    let flat = openfml::expand_includes(include_str!("fixtures/team_budget.fml"), &mut |p| {
        files
            .iter()
            .find(|(n, _)| *n == p)
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| format!("missing {p}"))
    })
    .unwrap();
    assert_eq!(parse_cst(&flat).unwrap().text(), flat, "expanded model round-trips");
}

#[test]
fn declarations_are_segmented_and_named() {
    let cst = parse_cst(include_str!("fixtures/budget.fml")).unwrap();
    let root = Red::root(&cst);
    let got: Vec<(SyntaxKind, Option<String>)> =
        root.decls().iter().map(|d| (d.green.kind, decl_name(d.green))).collect();
    use SyntaxKind::*;
    let want: Vec<(SyntaxKind, Option<&str>)> = vec![
        (ModelHeader, Some("demo")),
        (CalendarDecl, Some("plan")),
        (DimensionDecl, Some("CostCenter")),
        (CurrencyDecl, None), // `currency kEUR` has only one ident after the keyword
        (UnitsDecl, Some("hc")),
        (InputDecl, Some("budget_cap")),
        (InputDecl, Some("expenses")),
        (InputDecl, Some("overhead")),
        (InputDecl, Some("headcount")),
        (AllocateDecl, Some("overhead_share")),
        (MeasureDecl, Some("loaded_cost")),
        (MeasureDecl, Some("total_expenses")),
        (MeasureDecl, Some("headroom")),
        (AssertDecl, Some("within_envelope")),
        (ScenarioDecl, Some("Squeeze")),
    ];
    assert_eq!(got.len(), want.len(), "decl count: {got:?}");
    for ((gk, gn), (wk, wn)) in got.iter().zip(want.iter()) {
        assert_eq!(gk, wk, "kind mismatch: {got:?}");
        if let Some(w) = wn {
            assert_eq!(gn.as_deref(), Some(*w));
        }
    }
}

#[test]
fn leading_comments_travel_with_their_declaration() {
    let src = include_str!("fixtures/budget.fml");
    let cst = parse_cst(src).unwrap();
    let root = Red::root(&cst);
    // The overhead block's explanatory comment sits INSIDE the overhead
    // input's node, so moving/removing the declaration moves the comment.
    let overhead = root
        .decls()
        .into_iter()
        .find(|d| decl_name(d.green).as_deref() == Some("overhead"))
        .unwrap();
    assert!(
        overhead.green.text().contains("// Shared overhead, allocated by headcount"),
        "leading comment attaches to the declaration"
    );
}

#[test]
fn include_lines_become_directive_nodes() {
    let cst = parse_cst(include_str!("fixtures/team_budget.fml")).unwrap();
    let root = Red::root(&cst);
    let directives: Vec<String> = root
        .decls()
        .into_iter()
        .filter(|d| d.green.kind == SyntaxKind::IncludeDirective)
        .map(|d| d.green.text())
        .collect();
    assert_eq!(directives.len(), 3);
    assert!(directives[0].contains("include \"team_marketing.fml\""));
}

#[test]
fn red_offsets_locate_declarations_exactly() {
    let src = include_str!("fixtures/budget.fml");
    let cst = parse_cst(src).unwrap();
    let root = Red::root(&cst);
    let off = src.find("headroom :").unwrap();
    let d = root.decl_at(off).unwrap();
    assert_eq!(d.green.kind, SyntaxKind::MeasureDecl);
    assert_eq!(decl_name(d.green).as_deref(), Some("headroom"));
    // The node's range reprints to exactly the bytes it claims.
    let (s, e) = d.range();
    assert_eq!(&src[s..e], d.green.text());
}

#[test]
fn structural_edits_are_byte_predictable_and_share_structure() {
    let src = include_str!("fixtures/budget.fml");
    let cst = parse_cst(src).unwrap();
    let root = Red::root(&cst);
    // Find the Squeeze scenario's child index and byte range.
    let (idx, range) = root
        .children()
        .into_iter()
        .enumerate()
        .find_map(|(i, c)| match c {
            openfml::cst::RedChild::Node(n) if n.green.kind == SyntaxKind::ScenarioDecl => {
                Some((i, n.range()))
            }
            _ => None,
        })
        .unwrap();
    // Remove it: the reprint is the source minus EXACTLY those bytes.
    let removed = cst.with_child_removed(idx);
    let expect = format!("{}{}", &src[..range.0], &src[range.1..]);
    assert_eq!(removed.text(), expect);
    // Insert it back at the same index: byte-identical to the original.
    let again = removed.with_child_inserted(idx, cst.children[idx].clone());
    assert_eq!(again.text(), src);
    // Structural sharing: every untouched declaration is the SAME node.
    for (i, (a, b)) in cst.children.iter().zip(again.children.iter()).enumerate() {
        if let (GreenChild::Node(x), GreenChild::Node(y)) = (a, b) {
            assert!(std::rc::Rc::ptr_eq(x, y), "child {i} must be shared, not copied");
        }
    }
}

#[test]
fn exotic_spellings_survive() {
    // Underscored literals, ± tolerance, chained indexing, unicode ops.
    let src = "model demo.spell\ncalendar plan = yearly 2026 .. 2027\ncurrency EUR\n\
        input x : EUR flow over plan = 1_000_000   // spaced comment\n\
        y : EUR flow over plan = x * 2\n\
        assert tie over plan : y == x + x ± 0.5\n";
    let cst = parse_cst(src).unwrap();
    assert_eq!(cst.text(), src);
    let root = Red::root(&cst);
    let x = root.decls().into_iter().find(|d| decl_name(d.green).as_deref() == Some("x")).unwrap();
    assert!(x.green.text().contains("1_000_000"), "raw spelling preserved in the tree");
    assert!(x.green.text().contains("// spaced comment"), "same-line comment stays with its declaration");
}
