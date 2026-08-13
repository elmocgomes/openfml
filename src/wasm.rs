//! WASM interface — zero-dependency C-ABI for the browser demo.
//!
//! Protocol (all strings via the shared input buffer):
//!   fml_input_ptr(len)  → pointer to write UTF-8 into
//!   fml_load()          → build a Session from the input buffer + full run;
//!                         result JSON in the result buffer; returns 0/1
//!   fml_set(period, v)  → override input named by the input buffer
//!                         (period -1 = all periods); returns 0/1
//!   fml_recalc()        → incremental recalc; stats+values JSON; returns 0/1
//!   fml_result_ptr() / fml_result_len() → read the response

#![allow(static_mut_refs)]

use crate::live::Session;

static mut INPUT_BUF: Vec<u8> = Vec::new();
static mut RESULT: Vec<u8> = Vec::new();
static mut SESSION: Option<Session> = None;
static mut ACTIVE_SCENARIO: Option<String> = None;
/// Include files provided by the host (name → text), used by fml_load's
/// resolver. The host fetches missing includes on demand and retries.
static mut FILES: Vec<(String, String)> = Vec::new();

fn set_result(s: String) {
    unsafe {
        RESULT = s.into_bytes();
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn json_num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".to_string()
    }
}

pub fn dump_state(session: &mut Session, stats_json: &str, include_src: bool) -> String {
    use crate::check::MUnit;
    let c = &session.checked;
    let unit_str = |mi: &crate::check::MeasureInfo| -> String {
        match &mi.munit {
            MUnit::Uniform(u) => format!("{u}"),
            MUnit::Local => "local".to_string(),
        }
    };
    let mut out = String::from("{\"ok\":true,");
    out.push_str(stats_json);
    let active = unsafe { ACTIVE_SCENARIO.clone() }.unwrap_or_else(|| "Base".to_string());
    out.push_str(&format!("\"active\":\"{}\",", json_escape(&active)));
    out.push_str("\"scenarios\":[");
    for (k, name) in session.scenario_names().iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", json_escape(name)));
    }
    out.push_str("],");
    out.push_str("\"dims\":[");
    for (k, d) in c.dims.iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str(&format!("{{\"name\":\"{}\",\"members\":[", json_escape(&d.name)));
        for (j, m) in d.members.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{}\"", json_escape(m)));
        }
        out.push_str("]}");
    }
    out.push_str("],\"periods\":[");
    for t in 0..c.calendar.len {
        if t > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", c.calendar.label(t)));
    }
    out.push_str("],\"series\":[");
    let mut first = true;
    for (i, mi) in c.measures.iter().enumerate() {
        for mb in 0..c.tuple_count(i) {
            if !mi.is_series {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let label = c.tuple_label(i, mb);
            let display = if label.is_empty() {
                mi.name.clone()
            } else {
                format!("{}[{}]", mi.name, label)
            };
            // Editability = literal edit sites exist (grid edits write back
            // into the source text). "all" = one broadcast literal; a list =
            // per-period map entries; "none" = formula-defined.
            let sites: Vec<Option<usize>> = c
                .edit_sites
                .iter()
                .filter(|(sm, smb, _, _, _)| *sm == i && *smb == mb)
                .map(|(_, _, st, _, _)| *st)
                .collect();
            let edit = if sites.is_empty() {
                "\"none\"".to_string()
            } else if sites.iter().any(|t| t.is_none()) {
                "\"all\"".to_string()
            } else {
                let ts: Vec<String> = sites.iter().flatten().map(|t| t.to_string()).collect();
                format!("[{}]", ts.join(","))
            };
            let member_field = if mi.dims.len() == 1 {
                c.tuple_label(i, mb)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"key\":\"{}\",\"member\":\"{}\",\"input\":{},\"edit\":{},\"unit\":\"{}\",\"range\":[{},{}],\"vals\":[",
                json_escape(&display),
                json_escape(&mi.name),
                json_escape(&member_field),
                mi.is_input,
                edit,
                json_escape(&unit_str(mi)),
                mi.range.0,
                mi.range.1
            ));
            for (t, v) in session.values[i][mb].iter().enumerate() {
                if t > 0 {
                    out.push(',');
                }
                out.push_str(&json_num(*v));
            }
            out.push_str("]}");
        }
    }
    out.push_str("],\"scalars\":[");
    let mut first = true;
    for (i, mi) in c.measures.iter().enumerate() {
        for mb in 0..c.tuple_count(i) {
            if mi.is_series {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"input\":{},\"unit\":\"{}\",\"val\":{}}}",
                json_escape(&mi.name),
                mi.is_input && mi.dims.is_empty(),
                json_escape(&unit_str(mi)),
                json_num(session.values[i][mb][0])
            ));
        }
    }
    out.push_str("],\"asserts\":[");
    match session.run_asserts() {
        Ok(asserts) => {
            for (k, a) in asserts.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "[\"{}\",{},{}]",
                    json_escape(&a.name),
                    a.passed,
                    json_num(a.max_deviation)
                ));
            }
        }
        Err(_) => {}
    }
    out.push(']');
    if include_src {
        out.push_str(&format!(",\"src\":\"{}\"", json_escape(session.source())));
        out.push_str(",\"files\":[");
        for (k, f) in session.files().iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"src\":\"{}\"}}",
                json_escape(&f.name),
                json_escape(&f.text)
            ));
        }
        out.push(']');
    }
    out.push('}');
    out
}

#[no_mangle]
pub extern "C" fn fml_input_ptr(len: usize) -> *mut u8 {
    unsafe {
        INPUT_BUF.clear();
        INPUT_BUF.resize(len, 0);
        INPUT_BUF.as_mut_ptr()
    }
}

#[no_mangle]
pub extern "C" fn fml_result_ptr() -> *const u8 {
    unsafe { RESULT.as_ptr() }
}

#[no_mangle]
pub extern "C" fn fml_result_len() -> usize {
    unsafe { RESULT.len() }
}

/// Provide (or replace) an include file: input buffer = "name\ncontent".
/// fml_load resolves `include "name"` against the provided set.
#[no_mangle]
pub extern "C" fn fml_file() -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    match raw.split_once('\n') {
        Some((name, content)) => {
            unsafe {
                if let Some(slot) = FILES.iter_mut().find(|(n, _)| n == name) {
                    slot.1 = content.to_string();
                } else {
                    FILES.push((name.to_string(), content.to_string()));
                }
            }
            0
        }
        None => {
            set_result("{\"ok\":false,\"error\":\"fml_file expects name\\ncontent\"}".into());
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn fml_files_clear() {
    unsafe {
        FILES.clear();
    }
}

#[no_mangle]
pub extern "C" fn fml_load() -> i32 {
    let src = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let expanded = crate::expand_includes_with_map("main", &src, &mut |p: &str| {
        unsafe { FILES.iter().find(|(n, _)| n == p).map(|(_, t)| t.clone()) }
            .ok_or_else(|| format!("include \"{p}\" is not loaded"))
    });
    let expanded = match expanded {
        Ok(e) => e,
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            return 1;
        }
    };
    // Incremental path: an existing session tries the salsa-style reload
    // first — a trivia-only edit reuses the whole analysis and runtime
    // state; a semantic edit rebuilds, naming the changed declarations.
    if let Some(sess) = unsafe { SESSION.as_mut() } {
        if let Ok(rs) = sess.reload(expanded.clone()) {
            let changed: Vec<String> =
                rs.changed.iter().map(|c| format!("\"{}\"", json_escape(c))).collect();
            let stats_json = format!(
                "\"steps_run\":{},\"steps_total\":{},\"nodes_changed\":0,\"reload\":{{\"reused\":{},\"changed\":[{}],\"affected\":{},\"total\":{},\"hits\":{},\"misses\":{}}},",
                rs.steps_run,
                rs.steps_run,
                rs.reused,
                changed.join(","),
                rs.affected.len(),
                rs.total_decls,
                rs.query_hits,
                rs.query_misses
            );
            unsafe {
                ACTIVE_SCENARIO = None;
            }
            let json = {
                let s = unsafe { SESSION.as_mut().expect("present") };
                dump_state(s, &stats_json, false)
            };
            set_result(json);
            return 0;
        }
        // reload failed to compile → fall through to the salvage path.
    }
    match Session::new_expanded(expanded.clone()) {
        Ok(mut s) => match s.run_full() {
            Ok(stats) => {
                let stats_json = format!(
                    "\"steps_run\":{},\"steps_total\":{},\"nodes_changed\":{},",
                    stats.steps_run, stats.steps_total, stats.nodes_changed
                );
                unsafe {
                    ACTIVE_SCENARIO = None;
                }
                let json = dump_state(&mut s, &stats_json, false);
                unsafe {
                    SESSION = Some(s);
                }
                set_result(json);
                0
            }
            Err(e) => {
                set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
                1
            }
        },
        Err(first_err) => {
            // Salvage: drop broken declarations and their dependents; if
            // the remainder checks and runs, serve THAT with a warning —
            // the grid stays live while the file is mid-edit.
            let attempt = (|| -> Result<(Session, String, String), String> {
                let sal = crate::parse_salvage(&expanded.flat)?;
                if sal.errors.is_empty() && sal.dropped.is_empty() {
                    return Err(String::new()); // nothing to salvage around
                }
                let mut s = Session::from_model_parts(
                    &sal.model,
                    expanded.flat.clone(),
                    expanded.files.clone(),
                    expanded.segments.clone(),
                )?;
                s.run_full()?;
                let mut warn = sal
                    .errors
                    .iter()
                    .map(|e| e.msg.clone())
                    .collect::<Vec<_>>()
                    .join(" · ");
                if !sal.dropped.is_empty() {
                    let names: Vec<String> =
                        sal.dropped.iter().map(|(w, _)| w.clone()).collect();
                    warn.push_str(&format!("   (also omitted: {})", names.join(", ")));
                }
                // Structured error locations, routed to the owning file.
                let mut errs = String::from("[");
                for (i, e) in sal.errors.iter().enumerate() {
                    if i > 0 {
                        errs.push(',');
                    }
                    let (file, line) = s.locate_line(e.line);
                    errs.push_str(&format!(
                        "{{\"file\":\"{}\",\"line\":{line},\"msg\":\"{}\"}}",
                        json_escape(&file),
                        json_escape(&e.msg)
                    ));
                }
                errs.push(']');
                Ok((s, warn, errs))
            })();
            match attempt {
                Ok((mut s, warn, errs)) => {
                    unsafe {
                        ACTIVE_SCENARIO = None;
                    }
                    let mut json = dump_state(
                        &mut s,
                        "\"steps_run\":0,\"steps_total\":0,\"nodes_changed\":0,",
                        false,
                    );
                    json.pop();
                    json.push_str(&format!(
                        ",\"warning\":\"{}\",\"errors\":{errs}}}",
                        json_escape(&warn)
                    ));
                    unsafe {
                        SESSION = Some(s);
                    }
                    set_result(json);
                    0
                }
                Err(_) => {
                    // Hard failure: surface the line from the message.
                    let line: usize = first_err
                        .strip_prefix("line ")
                        .and_then(|r| r.split(':').next())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    set_result(format!(
                        "{{\"ok\":false,\"error\":\"{}\",\"errors\":[{{\"file\":\"\",\"line\":{line},\"msg\":\"{}\"}}]}}",
                        json_escape(&first_err),
                        json_escape(&first_err)
                    ));
                    1
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn fml_set(period: i32, value: f64) -> i32 {
    let name = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    let p = if period < 0 { None } else { Some(period as usize) };
    match session.set_input(&name, None, p, value) {
        Ok(()) => 0,
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Grid → text write-back: patch the literal defining the named input in
/// the SOURCE, apply the change incrementally, and return state + the new
/// source text (so the editor pane stays in sync).
#[no_mangle]
pub extern "C" fn fml_patch(period: i32, value: f64) -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let (name, member) = match raw.split_once('|') {
        Some((n, m)) if !m.is_empty() => (n.to_string(), Some(m.to_string())),
        _ => (raw, None),
    };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    let p = if period < 0 { None } else { Some(period as usize) };
    if let Err(e) = session.patch_input(&name, member.as_deref(), p, value) {
        set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
        return 1;
    }
    match session.recalc() {
        Ok(stats) => {
            let stats_json = format!(
                "\"steps_run\":{},\"steps_total\":{},\"nodes_changed\":{},",
                stats.steps_run, stats.steps_total, stats.nodes_changed
            );
            let json = dump_state(session, &stats_json, true);
            set_result(json);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Evaluate a scenario (name via the input buffer; "Base" = the model as
/// written) and return its full state. Base values stay untouched.
#[no_mangle]
pub extern "C" fn fml_scenario() -> i32 {
    let name = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    if name == "Base" {
        unsafe {
            ACTIVE_SCENARIO = None;
        }
        let json = dump_state(session, "\"steps_run\":0,\"steps_total\":0,\"nodes_changed\":0,", false);
        set_result(json);
        return 0;
    }
    match session.eval_scenario(&name) {
        Ok((mut vals, stats)) => {
            unsafe {
                ACTIVE_SCENARIO = Some(name);
            }
            std::mem::swap(&mut session.values, &mut vals);
            let stats_json = format!(
                "\"steps_run\":{},\"steps_total\":{},\"nodes_changed\":{},",
                stats.steps_run, stats.steps_total, stats.nodes_changed
            );
            let json = dump_state(session, &stats_json, false);
            std::mem::swap(&mut session.values, &mut vals);
            set_result(json);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Monte Carlo simulation over the model's distribution inputs; returns
/// per-cell [p10, p50, p90] bands.
#[no_mangle]
pub extern "C" fn fml_simulate(trials: i32) -> i32 {
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.simulate(trials.max(10) as usize) {
        Ok(sim) => {
            let mut out = format!("{{\"ok\":true,\"trials\":{},\"cells\":[", sim.trials);
            for (k, (name, is_series, bands)) in sim.cells.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{{\"name\":\"{}\",\"series\":{},\"bands\":[", json_escape(name), is_series));
                for (j, b) in bands.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("[{},{},{}]", json_num(b[0]), json_num(b[1]), json_num(b[2])));
                }
                out.push_str("]}");
            }
            out.push_str("]}");
            set_result(out);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Tornado sensitivity for one output cell (name via input buffer, with
/// optional "|member"); returns ranked {label, down, up} bars.
#[no_mangle]
pub extern "C" fn fml_tornado(period: i32, rel: f64) -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let (name, member) = match raw.split_once('|') {
        Some((n, m)) if !m.is_empty() => (n.to_string(), Some(m.to_string())),
        _ => (raw, None),
    };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    let p = if period < 0 { None } else { Some(period as usize) };
    match session.tornado(&name, member.as_deref(), p, rel) {
        Ok(bars) => {
            let mut out = String::from("{\"ok\":true,\"bars\":[");
            for (k, (label, down, up)) in bars.iter().take(12).enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "[\"{}\",{},{}]",
                    json_escape(label),
                    json_num(*down),
                    json_num(*up)
                ));
            }
            out.push_str("]}");
            set_result(out);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Goal-seek: find the input value that makes an output hit a target.
/// Input buffer: "in_name|in_member|out_name|out_member" (members may be
/// empty); periods -1 = broadcast/scalar. Runtime-only — the caller
/// commits the solution via fml_patch if wanted.
#[no_mangle]
pub extern "C" fn fml_goalseek(in_period: i32, out_period: i32, target: f64) -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let parts: Vec<&str> = raw.split('|').collect();
    if parts.len() != 4 {
        set_result("{\"ok\":false,\"error\":\"fml_goalseek expects in|inMember|out|outMember\"}".into());
        return 1;
    }
    let opt = |s: &str| if s.is_empty() { None } else { Some(s.to_string()) };
    let (iname, imem, oname, omem) = (parts[0].to_string(), opt(parts[1]), parts[2].to_string(), opt(parts[3]));
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    let ip = if in_period < 0 { None } else { Some(in_period as usize) };
    let op = if out_period < 0 { None } else { Some(out_period as usize) };
    match session.goal_seek(&iname, imem.as_deref(), ip, &oname, omem.as_deref(), op, target) {
        Ok(r) => {
            set_result(format!(
                "{{\"ok\":true,\"value\":{},\"achieved\":{},\"iterations\":{}}}",
                json_num(r.value),
                json_num(r.achieved),
                r.iterations
            ));
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// "Explain this number" for one cell (input buffer: "name|member";
/// period -1 = scalar): definition site routed to the owning file, the
/// match/actuals arm that fired, and direct dependency cells with values.
#[no_mangle]
pub extern "C" fn fml_explain(period: i32) -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let (name, member) = match raw.split_once('|') {
        Some((n, m)) if !m.is_empty() => (n.to_string(), Some(m.to_string())),
        _ => (raw, None),
    };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    let p = if period < 0 { None } else { Some(period as usize) };
    match session.explain(&name, member.as_deref(), p) {
        Ok(ex) => {
            let body = session
                .body_text(&ex.name)
                .map(|b| format!("\"body\":\"{}\",", json_escape(&b)))
                .unwrap_or_default();
            let mut out = format!(
                "{{\"ok\":true,{body}\"name\":\"{}\",\"member\":\"{}\",\"period\":{},\"label\":\"{}\",\"value\":{},\"unit\":\"{}\",\"input\":{},\"file\":\"{}\",\"line\":{},\"arm\":\"{}\",\"note\":\"{}\",\"deps\":[",
                json_escape(&ex.name),
                json_escape(&ex.member),
                ex.period.map(|t| t.to_string()).unwrap_or_else(|| "null".into()),
                ex.period.map(|t| session.checked.calendar.label(t)).unwrap_or_default(),
                json_num(ex.value),
                json_escape(&ex.unit),
                ex.is_input,
                json_escape(&ex.file),
                ex.line,
                json_escape(&ex.arm),
                json_escape(&ex.note),
            );
            for (k, d) in ex.deps.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"name\":\"{}\",\"member\":\"{}\",\"t\":{},\"label\":\"{}\",\"value\":{},\"input\":{},\"via\":\"{}\"}}",
                    json_escape(&d.name),
                    json_escape(&d.member),
                    d.period.map(|t| t.to_string()).unwrap_or_else(|| "null".into()),
                    json_escape(&d.label),
                    json_num(d.value),
                    d.is_input,
                    json_escape(&d.via),
                ));
            }
            out.push_str("],\"terms\":[");
            for (k, tm) in ex.terms.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                let (cn, cm, ct) = match &tm.cell {
                    Some((n, mm, p)) => (
                        format!("\"{}\"", json_escape(n)),
                        format!("\"{}\"", json_escape(mm)),
                        p.map(|t| t.to_string()).unwrap_or_else(|| "null".into()),
                    ),
                    None => ("null".into(), "null".into(), "null".into()),
                };
                out.push_str(&format!(
                    "{{\"label\":\"{}\",\"value\":{},\"name\":{cn},\"member\":{cm},\"t\":{ct}}}",
                    json_escape(&tm.label),
                    json_num(tm.value),
                ));
            }
            out.push_str("]}");
            set_result(out);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

fn files_json(files: &[(String, String)]) -> String {
    let mut out = String::from("\"files\":[");
    for (k, (name, text)) in files.iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":\"{}\",\"src\":\"{}\"}}",
            json_escape(name),
            json_escape(text)
        ));
    }
    out.push(']');
    out
}

/// Structural edit: add one period at the end of the calendar (extending
/// full-range maps). Returns new file texts — the host reloads.
#[no_mangle]
pub extern "C" fn fml_add_period() -> i32 {
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.add_period() {
        Ok((files, label)) => {
            set_result(format!(
                "{{\"ok\":true,\"label\":\"{}\",{}}}",
                json_escape(&label),
                files_json(&files)
            ));
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Structural edit: add a member to a dimension (input buffer
/// "dim|member|default"). Returns new file texts — the host reloads.
#[no_mangle]
pub extern "C" fn fml_add_member() -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let parts: Vec<&str> = raw.splitn(3, '|').collect();
    if parts.len() != 3 {
        set_result("{\"ok\":false,\"error\":\"fml_add_member expects dim|member|default\"}".into());
        return 1;
    }
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.add_member(parts[0], parts[1], parts[2]) {
        Ok(files) => {
            set_result(format!("{{\"ok\":true,{}}}", files_json(&files)));
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Structural edit: replace a declaration's formula (input buffer
/// "name|new body text"). Returns new file texts — the host reloads.
#[no_mangle]
pub extern "C" fn fml_replace_formula() -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let Some((name, body)) = raw.split_once('|') else {
        set_result("{\"ok\":false,\"error\":\"fml_replace_formula expects name|body\"}".into());
        return 1;
    };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.replace_formula(name, body) {
        Ok(files) => {
            set_result(format!("{{\"ok\":true,{}}}", files_json(&files)));
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Structural edit: rename a measure everywhere (input buffer "old|new").
/// Returns new file texts — the host reloads.
#[no_mangle]
pub extern "C" fn fml_rename() -> i32 {
    let raw = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let Some((old, new)) = raw.split_once('|') else {
        set_result("{\"ok\":false,\"error\":\"fml_rename expects old|new\"}".into());
        return 1;
    };
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.rename_measure(old, new) {
        Ok(files) => {
            set_result(format!("{{\"ok\":true,{}}}", files_json(&files)));
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}

/// Tokenize the input buffer for editor highlighting: JSON [[kind, start,
/// end], …] with SEMANTIC kinds when a session is loaded (measure/member/
/// unit/keyword vs plain ident). Returns 1 on unlexable text (host falls
/// back to plain rendering).
#[no_mangle]
pub extern "C" fn fml_tokens() -> i32 {
    use crate::lexer::Tok;
    let src = unsafe { String::from_utf8_lossy(&INPUT_BUF).to_string() };
    let toks = match crate::lexer::lex_full(&src) {
        Ok(t) => t,
        Err(_) => {
            set_result("{\"ok\":false}".into());
            return 1;
        }
    };
    let session = unsafe { SESSION.as_ref() };
    let mut out = String::from("{\"ok\":true,\"toks\":[");
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let kind = match &t.tok {
            Tok::Ws => "ws",
            Tok::Comment => "cm",
            Tok::Directive => "dir",
            Tok::Num(_) => "num",
            Tok::Pct(_) => "pct",
            Tok::Sym(_) => "sym",
            Tok::Ident(name) => {
                if crate::parser::is_keyword(name) {
                    "kw"
                } else if let Some(s) = session {
                    if s.checked.index.contains_key(name) {
                        "ms"
                    } else if s.checked.member_lookup.contains_key(name)
                        || s.checked.group_lookup.contains_key(name)
                        || s.checked.dims.iter().any(|d| &d.name == name)
                    {
                        "mb"
                    } else if s.checked.unit_reg.contains_key(name) {
                        "un"
                    } else {
                        "id"
                    }
                } else {
                    "id"
                }
            }
        };
        out.push_str(&format!("[\"{kind}\",{},{}]", t.start, t.end));
    }
    out.push_str("]}");
    set_result(out);
    0
}

/// The model's structural self-description (files/includes, measures with
/// the reference graph, dims, asserts, scenarios) for management views.
#[no_mangle]
pub extern "C" fn fml_model_info() -> i32 {
    let session = unsafe {
        match SESSION.as_ref() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    set_result(session.model_info_json());
    0
}

/// Completion candidates from the live session: JSON [[name, kind,
/// detail], …] (measures with units, members, units, ranges, keywords).
#[no_mangle]
pub extern "C" fn fml_complete() -> i32 {
    let session = unsafe {
        match SESSION.as_ref() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":true,\"items\":[]}".into());
                return 0;
            }
        }
    };
    let mut out = String::from("{\"ok\":true,\"items\":[");
    for (i, (name, kind, detail)) in session.completions().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "[\"{}\",\"{kind}\",\"{}\"]",
            json_escape(name),
            json_escape(detail)
        ));
    }
    out.push_str("]}");
    set_result(out);
    0
}

#[no_mangle]
pub extern "C" fn fml_recalc() -> i32 {
    let session = unsafe {
        match SESSION.as_mut() {
            Some(s) => s,
            None => {
                set_result("{\"ok\":false,\"error\":\"no model loaded\"}".into());
                return 1;
            }
        }
    };
    match session.recalc() {
        Ok(stats) => {
            let stats_json = format!(
                "\"steps_run\":{},\"steps_total\":{},\"nodes_changed\":{},",
                stats.steps_run, stats.steps_total, stats.nodes_changed
            );
            let json = {
                let s: &mut Session = session;
                dump_state(s, &stats_json, false)
            };
            set_result(json);
            0
        }
        Err(e) => {
            set_result(format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e)));
            1
        }
    }
}
