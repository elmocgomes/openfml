//! Authentication and the tamper-evident event log: SHA-256/HMAC against
//! the standard test vectors, bearer tokens, and the hash chain that
//! makes modified history fail replay.

use fml::crypto::{hex, hmac_sha256, sha256};
use fml::server::{make_token, replay_signed, sign_line, verify_token, Acl, Event, Process, GENESIS};
use fml::Session;

#[test]
fn sha256_matches_the_standard_vectors() {
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // Multi-block message (>55 bytes forces a second block).
    assert_eq!(
        hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn hmac_matches_rfc_4231() {
    // RFC 4231 test case 2.
    assert_eq!(
        hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // Test case 1: 20 bytes of 0x0b, "Hi There".
    assert_eq!(
        hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn tokens_verify_and_forgeries_do_not() {
    let secret = b"a-32-byte-demo-secret-for-tests!";
    let tok = make_token(secret, "alice");
    assert!(tok.starts_with("alice."));
    assert_eq!(verify_token(secret, &tok), Some("alice".to_string()));
    // Tampered MAC, renamed user, wrong secret, malformed — all rejected.
    let mut bad = tok.clone();
    bad.pop();
    bad.push('0');
    assert_eq!(verify_token(secret, &bad), None);
    let renamed = tok.replacen("alice", "cfo", 1);
    assert_eq!(verify_token(secret, &renamed), None);
    assert_eq!(verify_token(b"other-secret", &tok), None);
    assert_eq!(verify_token(secret, "alice"), None);
    assert_eq!(verify_token(secret, ".abc"), None);
}

const MODEL: &str = "model demo.auth\ncalendar plan = yearly 2026 .. 2027\ncurrency kEUR\n\
input a : kEUR flow over plan = 10\ninput b : kEUR flow over plan = 20\ntotal : kEUR flow over plan = a + b\n";

fn signed_log(secret: &[u8], events: &[Event]) -> String {
    let mut out = String::new();
    let mut prev = GENESIS.to_string();
    for ev in events {
        let line = ev.to_line();
        let sig = sign_line(secret, &prev, &line);
        out.push_str(&format!("{line}\t{sig}\n"));
        prev = sig;
    }
    out
}

fn events() -> Vec<Event> {
    vec![
        Event { seq: 1, user: "alice".into(), kind: "patch".into(), name: "a".into(), member: None, period: Some(0), value: 11.0, text: None },
        Event { seq: 2, user: "bob".into(), kind: "patch".into(), name: "b".into(), member: None, period: None, value: 25.0, text: None },
        Event { seq: 3, user: "alice".into(), kind: "patch".into(), name: "a".into(), member: None, period: Some(1), value: 12.0, text: None },
    ]
}

#[test]
fn intact_chains_replay_and_verify() {
    let secret = b"chain-secret";
    let log = signed_log(secret, &events());
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    let (last, tip) = replay_signed(&mut s, &mut Process::default(), &log, secret).unwrap();
    assert_eq!(last, 3);
    assert_eq!(tip.len(), 64, "tip is the last signature");
    // `a` is a broadcast literal: each patch rewrites the ONE literal, so
    // the last committed value (12) holds for every period.
    assert_eq!(s.get("total", None, Some(0)).unwrap(), 12.0 + 25.0);
    assert_eq!(s.get("total", None, Some(1)).unwrap(), 12.0 + 25.0);
}

#[test]
fn edited_history_breaks_the_chain() {
    let secret = b"chain-secret";
    let log = signed_log(secret, &events());
    // Retroactively "fix" bob's number from 25 to 20: signature mismatch.
    let tampered = log.replace("\t25\t", "\t20\t");
    assert_ne!(tampered, log);
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    let err = replay_signed(&mut s, &mut Process::default(), &tampered, secret).unwrap_err();
    assert!(err.contains("signature mismatch"), "err: {err}");
    assert!(err.contains("event 2"), "names the modified event: {err}");
}

#[test]
fn deleted_and_reordered_history_break_the_chain() {
    let secret = b"chain-secret";
    let lines: Vec<&str> = Box::leak(signed_log(secret, &events()).into_boxed_str()).lines().collect();
    // Deleting the middle event breaks the link into event 3.
    let deleted = format!("{}\n{}\n", lines[0], lines[2]);
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    assert!(replay_signed(&mut s, &mut Process::default(), &deleted, secret).unwrap_err().contains("signature mismatch"));
    // Reordering breaks immediately.
    let reordered = format!("{}\n{}\n{}\n", lines[1], lines[0], lines[2]);
    let mut s2 = Session::new(MODEL).unwrap();
    s2.run_full().unwrap();
    assert!(replay_signed(&mut s2, &mut Process::default(), &reordered, secret).unwrap_err().contains("signature mismatch"));
}

#[test]
fn wrong_secret_and_legacy_logs_are_refused() {
    let secret = b"chain-secret";
    let log = signed_log(secret, &events());
    let mut s = Session::new(MODEL).unwrap();
    s.run_full().unwrap();
    assert!(replay_signed(&mut s, &mut Process::default(), &log, b"other").unwrap_err().contains("signature mismatch"));
    // A pre-authentication (unsigned) log is refused with guidance.
    let legacy = events().iter().map(|e| e.to_line() + "\n").collect::<String>();
    let mut s2 = Session::new(MODEL).unwrap();
    s2.run_full().unwrap();
    assert!(replay_signed(&mut s2, &mut Process::default(), &legacy, secret).unwrap_err().contains("unsigned"));
}

#[test]
fn the_gate_composes_token_then_acl() {
    // End-to-end authorization decision as the server makes it:
    // verified identity → ACL → apply.
    let secret = b"gate-secret";
    let acl = Acl::parse("alice: a\nbob: b\ncfo: *\n").unwrap();
    let alice = make_token(secret, "alice");
    let user = verify_token(secret, &alice).unwrap();
    assert!(acl.authorize(&user, "a", None));
    assert!(!acl.authorize(&user, "b", None), "alice may not write b");
    // mallory forges a cfo token: rejected before the ACL is even asked.
    let forged = format!("cfo.{}", &make_token(secret, "mallory")[8..]);
    assert_eq!(verify_token(secret, &forged), None);
}
