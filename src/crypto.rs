//! Minimal zero-dependency crypto for the collaboration server:
//! SHA-256 (FIPS 180-4), HMAC-SHA256 (RFC 2104), constant-time
//! comparison, and server-secret management. Verified against the
//! standard test vectors in `tests/auth.rs`.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bitlen = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[i] = h[i].wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(msg);
    let ih = sha256(&inner);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&ih);
    sha256(&outer)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality — no early exit on the first differing byte.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Load the server secret, creating a fresh random one on first run
/// (0600, from the OS entropy pool with a hashed-clock fallback).
pub fn load_or_create_secret(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    if let Ok(s) = std::fs::read(path) {
        if s.len() >= 16 {
            return Ok(s);
        }
    }
    let urandom = || -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")?;
        let mut b = [0u8; 32];
        f.read_exact(&mut b)?;
        Ok(b.to_vec())
    };
    let secret = match urandom().ok() {
        Some(b) => b,
        None => {
            let mut seed = Vec::new();
            seed.extend_from_slice(&std::process::id().to_le_bytes());
            if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                seed.extend_from_slice(&d.as_nanos().to_le_bytes());
            }
            sha256(&seed).to_vec()
        }
    };
    std::fs::write(path, &secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(secret)
}

/// Decode a lowercase hex string; None on bad length/characters.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    for i in (0..b.len()).step_by(2) {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// Password hashing: salted SHA-256, iterated 60k times (a PBKDF in the
/// zero-dependency spirit — not memory-hard, but the tokens it guards are
/// short-lived HMACs and the store is a 0600 file on the server).
/// Format: "<salt hex>$<digest hex>".
pub fn hash_password(pass: &str, salt: &[u8]) -> String {
    let mut buf = Vec::with_capacity(salt.len() + pass.len());
    buf.extend_from_slice(salt);
    buf.extend_from_slice(pass.as_bytes());
    let mut h = sha256(&buf);
    for _ in 0..60_000 {
        let mut b = Vec::with_capacity(32 + salt.len());
        b.extend_from_slice(&h);
        b.extend_from_slice(salt);
        h = sha256(&b);
    }
    format!("{}${}", hex(salt), hex(&h))
}

/// Constant-time password verification against a stored "<salt>$<hash>".
pub fn verify_password(pass: &str, stored: &str) -> bool {
    let Some((salt_hex, _)) = stored.split_once('$') else { return false };
    let Some(salt) = from_hex(salt_hex) else { return false };
    ct_eq(hash_password(pass, &salt).as_bytes(), stored.as_bytes())
}

/// 16 random bytes from the OS.
pub fn random_bytes16() -> [u8; 16] {
    use std::io::Read;
    let mut b = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom").expect("urandom");
    f.read_exact(&mut b).expect("urandom read");
    b
}
