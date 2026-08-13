//! Multi-file models: `include "file"` textual expansion with cycle guards.

use std::collections::HashMap;

#[test]
fn includes_expand_and_compile() {
    let mut files: HashMap<&str, &str> = HashMap::new();
    files.insert("cc_marketing.fml", "input mkt : kEUR flow over plan = { 2026: 420, 2027: 460 }\n");
    files.insert("cc_ops.fml", "input ops : kEUR flow over plan = 315\n");
    let main = r#"model demo.multi
calendar plan = yearly 2026 .. 2027
currency kEUR
include "cc_marketing.fml"
include "cc_ops.fml"
total : kEUR flow over plan = mkt + ops
"#;
    let expanded = openfml::expand_includes(main, &mut |p| {
        files.get(p).map(|s| s.to_string()).ok_or_else(|| format!("missing {p}"))
    })
    .unwrap();
    let r = openfml::run(&expanded).expect("expanded model runs");
    let series: HashMap<String, Vec<f64>> = r.series.iter().cloned().collect();
    assert_eq!(series["total"][0], 735.0);
    assert_eq!(series["total"][1], 775.0);
}

#[test]
fn circular_includes_are_rejected() {
    let mut files: HashMap<&str, &str> = HashMap::new();
    files.insert("a.fml", "include \"b.fml\"\n");
    files.insert("b.fml", "include \"a.fml\"\n");
    let err = openfml::expand_includes("include \"a.fml\"\n", &mut |p| {
        files.get(p).map(|s| s.to_string()).ok_or_else(|| format!("missing {p}"))
    })
    .expect_err("cycle must fail");
    assert!(err.contains("circular"), "unexpected: {err}");
}
