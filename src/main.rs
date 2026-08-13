//! fml CLI: `fml check <file>` and `fml eval <file>`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && (args[1] == "--version" || args[1] == "-V") {
        println!("openfml {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let (cmd, path) = match (args.get(1).map(|s| s.as_str()), args.get(2)) {
        (Some(c @ ("check" | "eval")), Some(p)) => (c, p.clone()),
        _ => {
            eprintln!("usage: openfml <check|eval> <model.fml>");
            return ExitCode::from(2);
        }
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    // Resolve `include "…"` relative to the model file's directory.
    let base = std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let src = match openfml::expand_includes(&src, &mut |rel| {
        std::fs::read_to_string(base.join(rel))
            .map_err(|e| format!("cannot read include \"{rel}\": {e}"))
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Parse, bind the fact plane (data files resolve beside the model),
    // then check — the CLI equivalent of compile() with a live resolver.
    let checked = match (|| -> Result<_, String> {
        let mut model = openfml::Parser::parse(&src)?;
        openfml::expand_defs(&mut model)?;
        openfml::bind_data(
            &mut model,
            &mut |f| {
                std::fs::read_to_string(base.join(f))
                    .map_err(|e| format!("data file \"{f}\": {e}"))
            },
            &mut Vec::new(),
        )?;
        openfml::check(&model)
    })() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if cmd == "check" {
        println!("model {} — OK", checked.model_name);
        println!(
            "calendar {}: {} .. {} ({} periods)",
            checked.calendar.name,
            checked.calendar.label(0),
            checked.calendar.label(checked.calendar.len - 1),
            checked.calendar.len
        );
        for r in checked.ranges.iter().skip(1) {
            println!(
                "period {} = {} .. {}",
                r.name,
                checked.calendar.label(r.start),
                checked.calendar.label(r.end)
            );
        }
        println!("{:<16} {:>12} {:>7} {:>18} {}", "measure", "unit", "kind", "range", "role");
        for m in &checked.measures {
            let range = if m.is_series {
                format!(
                    "{} .. {}",
                    checked.calendar.label(m.range.0),
                    checked.calendar.label(m.range.1)
                )
            } else {
                "scalar".to_string()
            };
            let unit_str = match &m.munit {
                openfml::check::MUnit::Uniform(u) => u.to_string(),
                openfml::check::MUnit::Local => "local".to_string(),
            };
            println!(
                "{:<16} {:>12} {:>7} {:>18} {}",
                m.name,
                unit_str,
                match m.kind {
                    Some(openfml::ast::Kind::Stock) => "stock",
                    Some(openfml::ast::Kind::Flow) => "flow",
                    None => "-",
                },
                range,
                if m.is_input { "input" } else { "computed" },
            );
        }
        return ExitCode::SUCCESS;
    }

    let result = match openfml::evaluate(&checked) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    print!("{:<16}", "measure");
    for p in &result.period_labels {
        print!(" {:>12}", p);
    }
    println!();
    for (name, vals) in &result.series {
        print!("{name:<16}");
        for v in vals {
            if v.is_nan() {
                print!(" {:>12}", "·");
            } else {
                print!(" {v:>12.2}");
            }
        }
        println!();
    }
    if !result.scalars.is_empty() {
        println!();
        for (name, v) in &result.scalars {
            println!("{name:<16} {v:>14.4}");
        }
    }
    println!();
    for (name, iters) in &result.solve_iterations {
        if iters.is_empty() {
            continue;
        }
        let shown: Vec<String> = iters.iter().take(12).map(|i| i.to_string()).collect();
        let suffix = if iters.len() > 12 { ", …" } else { "" };
        println!("solve '{}' converged; iterations: [{}{}]", name, shown.join(", "), suffix);
    }
    let mut ok = true;
    for a in &result.asserts {
        if a.passed {
            println!("assert {:<20} PASS  (max deviation {:.6})", a.name, a.max_deviation);
        } else {
            ok = false;
            println!(
                "assert {:<20} FAIL  (max deviation {:.6}, first failure at {})",
                a.name,
                a.max_deviation,
                a.first_failure.clone().unwrap()
            );
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
