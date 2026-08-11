//! End-to-end budget-server test: spawn the real fml-server on a temp
//! config directory and drive the whole round over HTTP — access
//! restrictions, the gate matrix, minting, checkpoint, and the
//! restart-on-baseline cycle.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};

fn http(port: u16, method: &str, path: &str, body: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let req = if method == "GET" {
        format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

fn wait_up(port: u16) {
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("server did not come up");
}

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[test]
fn the_full_budget_round_over_http() {
    let port: u16 = 42000 + (std::process::id() % 1000) as u16;
    let dir = std::env::temp_dir().join(format!("fml_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("users.cfg"),
        "alice: marketing editor\ncarol: finance admin\ndave: marketing viewer\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("access.cfg"),
        "model plan.fml\n  departments marketing finance\n  write marketing: spend\n  write finance: *\n\
         model secret.fml\n  departments finance\n  write finance: *\n",
    )
    .unwrap();
    let plan = "model demo.plan\ncalendar y = yearly 2026 .. 2027\ncurrency EUR\n\
input spend : EUR flow over y = { 2026: 100, 2027: 110 }\ntotal : EUR flow over y = spend * 2\n";
    std::fs::write(dir.join("models/plan.fml"), plan).unwrap();
    std::fs::write(
        dir.join("models/secret.fml"),
        "model demo.secret\ncalendar y = yearly 2026 .. 2026\ncurrency EUR\ninput x : EUR flow over y = 1\n",
    )
    .unwrap();

    let mint = |user: &str| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_fml-server"))
            .args(["token", user, dir.join("server.secret").to_str().unwrap()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // Secret is created by the first token call; the server reuses it.
    let alice = mint("alice");
    let carol = mint("carol");
    let dave = mint("dave");

    let child = Command::new(env!("CARGO_BIN_EXE_fml-server"))
        .args([dir.to_str().unwrap(), &port.to_string()])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _guard = Guard(child);
    wait_up(port);

    // Department read restriction.
    let r = http(port, "GET", &format!("/state?token={alice}&model=secret.fml"), "");
    assert!(r.contains("may not access"), "{r}");
    let r = http(port, "GET", &format!("/models?token={alice}"), "");
    assert!(r.contains("plan.fml") && !r.contains("secret.fml"), "{r}");

    // Directory & minting: admin-only.
    assert!(http(port, "GET", &format!("/users?token={alice}"), "").contains("admin-only"));
    let r = http(port, "GET", &format!("/users?token={carol}"), "");
    assert!(r.contains("\"user\":\"dave\"") && r.contains("\"role\":\"viewer\""), "{r}");
    let r = http(port, "POST", "/mint", &format!("{{\"token\":\"{carol}\",\"user\":\"dave\"}}"));
    assert!(r.contains("\"token\":\"dave."), "{r}");
    assert!(http(port, "POST", "/mint", &format!("{{\"token\":\"{alice}\",\"user\":\"dave\"}}")).contains("admin-only"));

    // The gate matrix over HTTP.
    let patch = |tok: &str, name: &str, period: &str, value: &str| {
        http(
            port,
            "POST",
            "/patch",
            &format!("{{\"token\":\"{tok}\",\"model\":\"plan.fml\",\"name\":\"{name}\",\"period\":\"{period}\",\"value\":\"{value}\"}}"),
        )
    };
    assert!(patch(&dave, "spend", "0", "1").contains("read-only"));
    assert!(patch(&alice, "spend", "0", "120").contains("\"ok\":true"));
    // The grant check fires first (alice holds no grant on `total`); the
    // structural input-only guarantee is covered in tests/process.rs.
    assert!(patch(&alice, "total", "0", "1").contains("may not write total"));
    // Even the ADMIN (wildcard grant) cannot patch a computed measure —
    // /patch reaches only literal input sites, structurally.
    assert!(patch(&carol, "total", "0", "1").contains("not literal-editable"));
    assert!(http(port, "POST", "/submit", &format!("{{\"token\":\"{alice}\",\"model\":\"plan.fml\"}}")).contains("\"ok\":true"));
    assert!(patch(&alice, "spend", "1", "999").contains("submitted"));
    assert!(patch(&carol, "spend", "1", "115").contains("\"ok\":true"), "admins adjust through submissions");
    assert!(http(
        port,
        "POST",
        "/formula",
        &format!("{{\"token\":\"{carol}\",\"model\":\"plan.fml\",\"name\":\"total\",\"body\":\"spend * 3\"}}")
    )
    .contains("\"ok\":true"));
    assert!(http(port, "POST", "/lock", &format!("{{\"token\":\"{carol}\",\"model\":\"plan.fml\"}}")).contains("\"ok\":true"));
    assert!(patch(&carol, "spend", "0", "1").contains("locked"));

    // Checkpoint: numbers land in the model FILES, the log is archived,
    // the next round starts fresh on the new baseline.
    let r = http(port, "POST", "/checkpoint", &format!("{{\"token\":\"{carol}\",\"model\":\"plan.fml\"}}"));
    assert!(r.contains("\"ok\":true") && r.contains("archived"), "{r}");
    let on_disk = std::fs::read_to_string(dir.join("models/plan.fml")).unwrap();
    assert!(on_disk.contains("2026: 120") && on_disk.contains("2027: 115"), "{on_disk}");
    assert!(on_disk.contains("= spend * 3"), "the formula change persisted: {on_disk}");
    let archives: Vec<_> = std::fs::read_dir(dir.join("logs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("archived"))
        .collect();
    assert_eq!(archives.len(), 1, "one archived round");
    // The new round is open again (checkpoint resets process state).
    let r = http(port, "GET", &format!("/process?token={alice}&model=plan.fml"), "");
    assert!(r.contains("\"locked\":false"), "{r}");
    assert!(patch(&alice, "spend", "0", "125").contains("\"ok\":true"), "round 2 begins on the new baseline");

    let _ = std::fs::remove_dir_all(&dir);
}
