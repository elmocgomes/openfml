//! The fact plane: `= data "file.csv"` inputs. Facts bind from external
//! tables before checking, carry NO edit sites (structurally not
//! literal-editable — they change by re-import), pin by content hash,
//! and re-import equals a fresh compile (the equivalence theorem).

use fml::{Expanded, Segment, Session, SourceFile};

const MODEL: &str = "model demo.facts
calendar q = quarterly 2026-Q1 .. 2026-Q4
dimension Line = tree { All -> { A, B } }
currency kEUR
unit t

input volume : t flow over Line, q = data \"volumes.csv\"
input price : kEUR/t flow over q = data \"prices.csv\"
input fx : 1 = data \"fx.csv\"
revenue : kEUR flow over Line, q = volume * price * fx
total : kEUR flow over q = revenue[All]
";

const VOLUMES: &str = "Line,period,value
A,2026-Q1,100
A,2026-Q2,110
A,2026-Q3,120
A,2026-Q4,130
B,2026-Q1,50
B,2026-Q2,55
B,2026-Q3,60
B,2026-Q4,65
";

const PRICES: &str = "period,value
2026-Q1,2.0
2026-Q2,2.0
2026-Q3,2.1
2026-Q4,2.2
";

const FX: &str = "value
1.0
";

fn expanded(src: &str) -> Expanded {
    Expanded {
        flat: src.to_string(),
        files: vec![SourceFile { name: "model".into(), text: src.to_string() }],
        segments: vec![Segment { flat_start: 0, flat_end: src.len(), file: 0, local_start: 0 }],
    }
}

fn resolver(vols: &str, prices: &str, fx: &str) -> impl FnMut(&str) -> Result<String, String> {
    let (v, p, f) = (vols.to_string(), prices.to_string(), fx.to_string());
    move |name: &str| match name {
        "volumes.csv" => Ok(v.clone()),
        "prices.csv" => Ok(p.clone()),
        "fx.csv" => Ok(f.clone()),
        other => Err(format!("data file \"{other}\" is not loaded")),
    }
}

fn load(vols: &str, prices: &str, fx: &str) -> Session {
    let mut s =
        Session::new_expanded_resolve(expanded(MODEL), &mut resolver(vols, prices, fx)).unwrap();
    s.run_full().unwrap();
    s
}

#[test]
fn facts_bind_and_flow_like_any_input() {
    let mut s = load(VOLUMES, PRICES, FX);
    assert_eq!(s.get("volume", Some("A"), Some(0)).unwrap(), 100.0);
    assert_eq!(s.get("volume", Some("B"), Some(3)).unwrap(), 65.0);
    assert_eq!(s.get("revenue", Some("A"), Some(2)).unwrap(), 120.0 * 2.1);
    assert_eq!(s.get("total", None, Some(0)).unwrap(), 150.0 * 2.0);
}

#[test]
fn facts_are_structurally_not_literal_editable() {
    let mut s = load(VOLUMES, PRICES, FX);
    let err = s.patch_input("volume", Some("A"), Some(0), 999.0).unwrap_err();
    assert!(err.contains("not literal-editable"), "{err}");
    // …and the model view says why: the measure is data-bound.
    let info = fml::json::parse(&s.model_info_json()).unwrap();
    let vol = match info.get("measures").unwrap() {
        fml::json::J::A(ms) => ms
            .iter()
            .find(|m| matches!(m.get("name"), Some(fml::json::J::S(n)) if n == "volume"))
            .unwrap()
            .clone(),
        _ => panic!(),
    };
    assert_eq!(vol.get("data"), Some(&fml::json::J::S("volumes.csv".into())));
    assert_eq!(vol.get("editable"), Some(&fml::json::J::B(false)));
}

#[test]
fn a_missing_file_names_itself_for_the_host_retry_loop() {
    let err = Session::new(MODEL).err().unwrap();
    assert!(err.contains("data file \"volumes.csv\" is not loaded"), "{err}");
}

#[test]
fn the_sha256_pin_guards_reproducibility() {
    let good = fml::crypto::hex(&fml::crypto::sha256(PRICES.as_bytes()));
    let pinned = MODEL.replace(
        "data \"prices.csv\"",
        &format!("data \"prices.csv\" sha256 \"{good}\""),
    );
    let mut s =
        Session::new_expanded_resolve(expanded(&pinned), &mut resolver(VOLUMES, PRICES, FX))
            .unwrap();
    s.run_full().unwrap();
    // A changed file under the same pin refuses to load.
    let tampered = PRICES.replace("2.2", "9.9");
    let err = Session::new_expanded_resolve(expanded(&pinned), &mut resolver(VOLUMES, &tampered, FX))
        .err()
        .unwrap();
    assert!(err.contains("does not match the pin"), "{err}");
}

#[test]
fn incomplete_facts_are_a_compile_error() {
    let short = "Line,period,value\nA,2026-Q1,100\nB,2026-Q1,50\n";
    let err = Session::new_expanded_resolve(expanded(MODEL), &mut resolver(short, PRICES, FX))
        .err()
        .unwrap();
    assert!(err.contains("volume"), "the coverage error names the measure: {err}");
}

#[test]
fn reimport_equals_fresh_compile() {
    // The theorem, as ever: reload_resolve with changed facts ≡ building
    // from scratch on those facts.
    let mut s = load(VOLUMES, PRICES, FX);
    let new_prices = "period,value\n2026-Q1,2.5\n2026-Q2,2.5\n2026-Q3,2.6\n2026-Q4,2.7\n";
    let rs = s
        .reload_resolve(expanded(MODEL), &mut resolver(VOLUMES, new_prices, FX))
        .unwrap();
    assert!(!rs.reused, "changed facts must not reuse the analysis");
    assert_eq!(rs.changed, vec!["external data".to_string()]);
    let fresh = load(VOLUMES, new_prices, FX);
    for member in [Some("A"), Some("B")] {
        for t in 0..4 {
            assert_eq!(
                s.get("revenue", member, Some(t)).unwrap(),
                fresh.get("revenue", member, Some(t)).unwrap()
            );
        }
    }
    // …and IDENTICAL facts stay on the salsa fast path.
    let rs2 = s
        .reload_resolve(expanded(MODEL), &mut resolver(VOLUMES, new_prices, FX))
        .unwrap();
    assert!(rs2.reused, "byte-identical facts reuse the whole analysis");
}
