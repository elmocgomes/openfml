//! User-management e2e: spawn the real server on fresh config dirs and
//! drive the module over HTTP — Super Admin seeding, password login,
//! user & role CRUD with its guards, and users.cfg migration.

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

fn scaffold(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fml_users_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("access.cfg"),
        "model plan.fml\n  departments finance\n  write finance: *\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("models/plan.fml"),
        "model demo.plan\ncalendar y = yearly 2026 .. 2026\ncurrency EUR\ninput spend : EUR flow over y = 1\n",
    )
    .unwrap();
    dir
}

fn spawn(dir: &std::path::Path, port: u16) -> Guard {
    let child = Command::new(env!("CARGO_BIN_EXE_openfml-server"))
        .args([dir.to_str().unwrap(), &port.to_string()])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_up(port);
    Guard(child)
}

fn json_field(body: &str, key: &str) -> String {
    let pat = format!("\"{key}\":\"");
    let start = body.find(&pat).map(|i| i + pat.len()).unwrap_or_else(|| panic!("no {key} in {body}"));
    body[start..].split('"').next().unwrap().to_string()
}

fn seeded_password(dir: &std::path::Path) -> (String, String) {
    let note = std::fs::read_to_string(dir.join("admin-initial-password.txt")).expect("password note");
    let user = note.lines().next().unwrap().trim_start_matches("user: ").to_string();
    let pw = note.lines().nth(1).unwrap().trim_start_matches("password: ").to_string();
    (user, pw)
}

#[test]
fn seeding_login_and_user_crud() {
    let port: u16 = 43100 + (std::process::id() % 500) as u16;
    let dir = scaffold("crud");
    let _guard = spawn(&dir, port);

    // A fresh directory seeds a Super Admin: users.json plus a one-time
    // password note next to the config.
    assert!(dir.join("users.json").exists());
    let (admin, pw) = seeded_password(&dir);
    assert_eq!(admin, "admin");

    // Wrong password refused; right password issues a working token and
    // flags the forced change.
    let r = http(port, "POST", "/login", &format!("{{\"user\":\"{admin}\",\"password\":\"nope\"}}"));
    assert!(r.contains("wrong user or password"), "{r}");
    let r = http(port, "POST", "/login", &format!("{{\"user\":\"{admin}\",\"password\":\"{pw}\"}}"));
    assert!(r.contains("\"mustChange\":true") && r.contains("\"canManageUsers\":true"), "{r}");
    let tok = json_field(&r, "token");

    // Own-password change clears must_change and old password stops working.
    let r = http(port, "POST", "/password", &format!("{{\"token\":\"{tok}\",\"old\":\"bad\",\"new\":\"hunter2hunter2\"}}"));
    assert!(r.contains("current password is wrong"), "{r}");
    let r = http(port, "POST", "/password", &format!("{{\"token\":\"{tok}\",\"old\":\"{pw}\",\"new\":\"hunter2hunter2\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");
    assert!(http(port, "POST", "/login", &format!("{{\"user\":\"{admin}\",\"password\":\"{pw}\"}}")).contains("wrong"));
    let r = http(port, "POST", "/login", &format!("{{\"user\":\"{admin}\",\"password\":\"hunter2hunter2\"}}"));
    assert!(r.contains("\"mustChange\":false"), "{r}");

    // Create a user with a password; they can log in but cannot manage.
    let r = http(
        port,
        "POST",
        "/user_create",
        &format!("{{\"token\":\"{tok}\",\"user\":\"carla\",\"dept\":\"finance\",\"role\":\"admin\",\"password\":\"carla-pass-1\"}}"),
    );
    assert!(r.contains("\"ok\":true"), "{r}");
    let r = http(port, "POST", "/login", "{\"user\":\"carla\",\"password\":\"carla-pass-1\"}");
    assert!(r.contains("\"canManageUsers\":false"), "{r}");
    let carla = json_field(&r, "token");
    let r = http(port, "POST", "/user_create", &format!("{{\"token\":\"{carla}\",\"user\":\"x\",\"dept\":\"d\",\"role\":\"viewer\"}}"));
    assert!(r.contains("Super Admin capability"), "{r}");

    // Role CRUD: custom role, assignment, guards on builtins and in-use roles.
    let r = http(port, "POST", "/role_create", &format!("{{\"token\":\"{tok}\",\"name\":\"planner\",\"base\":\"editor\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");
    let r = http(port, "POST", "/user_update", &format!("{{\"token\":\"{tok}\",\"user\":\"carla\",\"role\":\"planner\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");
    let r = http(port, "GET", &format!("/roles?token={tok}"), "");
    assert!(r.contains("\"name\":\"planner\"") && r.contains("\"assigned\":1"), "{r}");
    assert!(http(port, "POST", "/role_update", &format!("{{\"token\":\"{tok}\",\"name\":\"admin\",\"base\":\"viewer\"}}"))
        .contains("built-in"));
    assert!(http(port, "POST", "/role_delete", &format!("{{\"token\":\"{tok}\",\"name\":\"planner\"}}"))
        .contains("assigned to 'carla'"));

    // Deletion guards: not yourself, never the last Super Admin.
    assert!(http(port, "POST", "/user_delete", &format!("{{\"token\":\"{tok}\",\"user\":\"{admin}\"}}"))
        .contains("cannot delete yourself"));
    assert!(http(port, "POST", "/user_update", &format!("{{\"token\":\"{tok}\",\"user\":\"{admin}\",\"role\":\"viewer\"}}"))
        .contains("last Super Admin"));
    let r = http(port, "POST", "/user_delete", &format!("{{\"token\":\"{tok}\",\"user\":\"carla\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");
    let r = http(port, "POST", "/role_delete", &format!("{{\"token\":\"{tok}\",\"name\":\"planner\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn users_cfg_migrates_and_survives_restart() {
    let port: u16 = 43700 + (std::process::id() % 500) as u16;
    let dir = scaffold("migrate");
    std::fs::write(dir.join("users.cfg"), "carla: finance admin\nvictor: finance viewer\n").unwrap();
    let guard = spawn(&dir, port);

    // Migrated accounts exist (token auth still works) but have no
    // password, so login is refused until a Super Admin sets one.
    let (_, pw) = seeded_password(&dir);
    let r = http(port, "POST", "/login", &format!("{{\"user\":\"admin\",\"password\":\"{pw}\"}}"));
    let tok = json_field(&r, "token");
    let r = http(port, "GET", &format!("/users?token={tok}"), "");
    assert!(r.contains("\"user\":\"carla\"") && r.contains("\"user\":\"victor\""), "{r}");
    assert!(http(port, "POST", "/login", "{\"user\":\"carla\",\"password\":\"\"}").contains("wrong"));
    let r = http(port, "POST", "/user_update", &format!("{{\"token\":\"{tok}\",\"user\":\"carla\",\"password\":\"carla-pw-12\"}}"));
    assert!(r.contains("\"ok\":true"), "{r}");
    assert!(http(port, "POST", "/login", "{\"user\":\"carla\",\"password\":\"carla-pw-12\"}").contains("\"token\""));

    // Restart: users.json is now authoritative — no re-seeding, no
    // second password note, carla's password persists.
    drop(guard);
    std::fs::remove_file(dir.join("admin-initial-password.txt")).unwrap();
    let _guard = spawn(&dir, port);
    assert!(!dir.join("admin-initial-password.txt").exists(), "re-seeded on restart");
    assert!(http(port, "POST", "/login", "{\"user\":\"carla\",\"password\":\"carla-pw-12\"}").contains("\"token\""));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn password_hashing_roundtrip() {
    use openfml::crypto::{hash_password, verify_password};
    let stored = hash_password("s3cret pa55", b"0123456789abcdef");
    assert!(verify_password("s3cret pa55", &stored));
    assert!(!verify_password("s3cret pa56", &stored));
    assert!(!verify_password("s3cret pa55", "garbage"));
    // Same password, different salt → different digest.
    assert_ne!(stored, hash_password("s3cret pa55", b"fedcba9876543210"));
}
