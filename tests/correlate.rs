//! `correlate a, b = rho` — Gaussian-copula dependence between
//! distribution inputs (marginals stay exactly as assessed), and
//! `per period` — iid draws each period instead of one draw per trial.
//! The assertions are structural: N(0,1) sums/differences have known
//! percentile widths, so the copula is verified through the model.

fn sim(src: &str, trials: usize) -> openfml::live::SimResult {
    let mut s = openfml::Session::new(src).unwrap();
    s.run_full().unwrap();
    s.simulate(trials).unwrap()
}

fn band(r: &openfml::live::SimResult, name: &str, t: usize) -> [f64; 3] {
    r.cells.iter().find(|(n, _, _)| n == name).unwrap().2[t]
}

const HEAD: &str = "model demo.corr\ncalendar plan = yearly 2026 .. 2026\n";

// p90 - p10 of a normal = 2 * 1.2816 * sd.
fn width(b: [f64; 3]) -> f64 {
    b[2] - b[0]
}

#[test]
fn positive_correlation_widens_sums_and_narrows_differences() {
    // sd(x+y) = sqrt(2(1+rho)) = 1.897; sd(x-y) = sqrt(2(1-rho)) = 0.632.
    let src = format!(
        "{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 ~ normal(0, 1)\ncorrelate x, y = 0.8\ns : 1 = x + y\nd : 1 = x - y\n"
    );
    let r = sim(&src, 4000);
    let (ws, wd) = (width(band(&r, "s", 0)), width(band(&r, "d", 0)));
    assert!((4.4..=5.3).contains(&ws), "sum width {ws} not ~4.86");
    assert!((1.4..=1.9).contains(&wd), "diff width {wd} not ~1.62");
}

#[test]
fn independent_inputs_stay_independent() {
    let src = format!(
        "{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 ~ normal(0, 1)\ns : 1 = x + y\n"
    );
    let ws = width(band(&sim(&src, 4000), "s", 0));
    // sd = sqrt(2) → width ≈ 3.63; clearly below the rho=0.8 width.
    assert!((3.3..=4.0).contains(&ws), "independent sum width {ws} not ~3.63");
}

#[test]
fn negative_correlation_narrows_sums() {
    let src = format!(
        "{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 ~ normal(0, 1)\ncorrelate x, y = -0.8\ns : 1 = x + y\n"
    );
    let ws = width(band(&sim(&src, 4000), "s", 0));
    // sd = sqrt(2(1-0.8)) = 0.632 → width ≈ 1.62.
    assert!((1.4..=1.9).contains(&ws), "anti-correlated sum width {ws} not ~1.62");
}

#[test]
fn marginals_survive_the_copula() {
    // Correlation must not distort each input's own distribution: a
    // uniform(0,1) correlated with a normal still has p50 ≈ 0.5 and
    // p10/p90 ≈ 0.1/0.9.
    let src = format!(
        "{HEAD}input u : 1 ~ uniform(0, 1)\ninput y : 1 ~ normal(0, 1)\ncorrelate u, y = 0.7\nout : 1 = u + 0 * y\n"
    );
    let b = band(&sim(&src, 4000), "out", 0);
    assert!((b[1] - 0.5).abs() < 0.05, "p50 {} not ~0.5", b[1]);
    assert!((b[0] - 0.1).abs() < 0.05, "p10 {} not ~0.1", b[0]);
    assert!((b[2] - 0.9).abs() < 0.05, "p90 {} not ~0.9", b[2]);
}

const HEAD2: &str = "model demo.pp\ncalendar plan = yearly 2026 .. 2027\n";

#[test]
fn per_period_draws_are_independent_across_time() {
    // Cumulating 2 iid N(0,1) draws: sd = sqrt(2) → width ≈ 3.63…
    let pp = format!(
        "{HEAD2}input e : 1 flow over plan ~ normal(0, 1) per period\ntot : 1 = sum[plan](e)\n"
    );
    let w_pp = width(band(&sim(&pp, 4000), "tot", 0));
    assert!((3.3..=4.0).contains(&w_pp), "per-period cum width {w_pp} not ~3.63");
    // …while ONE draw broadcast over both periods doubles: sd = 2 → ≈5.13.
    let one = format!(
        "{HEAD2}input e : 1 flow over plan ~ normal(0, 1)\ntot : 1 = sum[plan](e)\n"
    );
    let w_one = width(band(&sim(&one, 4000), "tot", 0));
    assert!((4.7..=5.6).contains(&w_one), "single-draw cum width {w_one} not ~5.13");
}

#[test]
fn per_period_correlation_is_contemporaneous() {
    // rho=0.9 at every period: d = a - b has sd sqrt(0.2) → width ≈ 1.15.
    let src = format!(
        "{HEAD2}input a : 1 flow over plan ~ normal(0, 1) per period\ninput b : 1 flow over plan ~ normal(0, 1) per period\ncorrelate a, b = 0.9\nd : 1 flow over plan = a - b\n"
    );
    let r = sim(&src, 4000);
    for t in 0..2 {
        let w = width(band(&r, "d", t));
        assert!((0.95..=1.35).contains(&w), "t={t}: diff width {w} not ~1.15");
    }
}

#[test]
fn correlated_simulation_is_deterministic() {
    let src = format!(
        "{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 ~ normal(0, 1)\ncorrelate x, y = 0.6\ns : 1 = x + y\n"
    );
    let (a, b) = (sim(&src, 500), sim(&src, 500));
    assert_eq!(band(&a, "s", 0), band(&b, "s", 0), "same seeds → same bands");
}

#[test]
fn deterministic_base_is_unchanged_by_correlate() {
    let plain = format!("{HEAD}input x : 1 ~ normal(3, 1)\ninput y : 1 ~ normal(4, 1)\ns : 1 = x + y\n");
    let corr = format!("{HEAD}input x : 1 ~ normal(3, 1)\ninput y : 1 ~ normal(4, 1)\ncorrelate x, y = 0.8\ns : 1 = x + y\n");
    let (a, b) = (openfml::run(&plain).unwrap(), openfml::run(&corr).unwrap());
    let get = |r: &openfml::EvalResult| r.scalars.iter().find(|(n, _)| n == "s").unwrap().1;
    assert_eq!(get(&a), 7.0);
    assert_eq!(get(&a), get(&b));
}

#[test]
fn correlate_errors_are_compile_time() {
    let bad = [
        (format!("{HEAD}input x : 1 ~ normal(0, 1)\ncorrelate x, ghost = 0.5\n"), "unknown measure"),
        (format!("{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 = 4\ncorrelate x, y = 0.5\n"), "no '~' distribution"),
        (format!("{HEAD}input x : 1 ~ normal(0, 1)\ninput y : 1 ~ normal(0, 1)\ncorrelate x, y = 1.2\n"), "within (-1, 1)"),
        (format!("{HEAD}input x : 1 ~ normal(0, 1)\ncorrelate x, x = 0.5\n"), "two distinct"),
        (
            format!("{HEAD}input a : 1 ~ normal(0, 1)\ninput b : 1 ~ normal(0, 1)\ninput c : 1 ~ normal(0, 1)\ncorrelate a, b = 0.9\ncorrelate b, c = 0.9\ncorrelate a, c = -0.9\n"),
            "not positive definite",
        ),
        (format!("{HEAD}input x : 1 ~ normal(0, 1) per period\n"), "'per period' needs a series"),
        (
            format!("{HEAD2}input a : 1 flow over plan ~ normal(0, 1) per period\ninput b : 1 ~ normal(0, 1)\ncorrelate a, b = 0.5\n"),
            "different frequencies",
        ),
    ];
    for (src, want) in &bad {
        let err = openfml::compile(src).expect_err(&format!("must fail: {want}"));
        assert!(err.contains(want), "error {err:?} lacks {want:?}");
    }
}
