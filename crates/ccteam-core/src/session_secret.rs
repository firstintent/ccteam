//! Per-session secret for the cto scheduling gate (v0.8.7 review-fix R-M1).
//!
//! A 128-bit value drawn from the OS CSPRNG, hex-encoded to a 32-char ASCII
//! token. The gateway mints one per spawned session, injects it into the
//! pane env (`CCTEAM_CHAT_SECRET`), and stores `sid -> {role, secret}`. The
//! stdio MCP forwarder (which inherits the pane env) forwards it with each
//! `session_*` call, and the daemon gate authenticates the caller by looking
//! the secret up in its session map instead of trusting a plaintext role arg.
//!
//! HONEST SCOPE (read before reasoning about the threat model): under the
//! current SINGLE-OS-UID full-trust model there is NO hard boundary between
//! agents — every agent runs as the same OS user, so one can read another
//! process's `/proc/<pid>/environ`, its files, or ptrace it, and thereby
//! recover this secret. The secret therefore only RAISES THE BAR
//! (defense-in-depth): it stops the trivial "send `{_caller_role:"cto"}` over
//! the socket" forgery and gives the gate a value to verify, but it is NOT a
//! security boundary and does not close the hole. Real per-agent isolation
//! requires a per-agent OS user or sandbox — tracked as v0.8.8-deferred. This
//! is a primitives leaf: no team-name literals live here.

/// Mint a fresh per-session secret: 16 CSPRNG bytes, lowercase-hex encoded
/// (32 ASCII chars, no `:` so it is safe inside colon-delimited keys).
///
/// Falls back to a time-seeded value only if the OS RNG is somehow
/// unavailable (never expected on Linux/macOS); the fallback still avoids a
/// constant so a degraded environment doesn't mint identical secrets, but it
/// is best-effort — the security posture is documented at the module level.
pub fn mint() -> String {
    let mut buf = [0u8; 16];
    if getrandom::getrandom(&mut buf).is_err() {
        // Extremely unlikely OS-RNG failure: derive a non-constant fallback
        // from the high-resolution clock + pid so two near-simultaneous
        // mints still differ. NOT cryptographically strong (the module doc
        // is explicit the secret only raises the bar), but never a constant.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let mixed = nanos ^ (pid << 64) ^ (pid.rotate_left(32));
        buf.copy_from_slice(&mixed.to_le_bytes());
    }
    let mut out = String::with_capacity(32);
    for b in buf {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// Constant-time equality for two secret tokens (avoids leaking how many
/// leading bytes matched via early-return timing). Length mismatch is a
/// fast `false` — the lengths are not themselves secret.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_32_hex_chars() {
        let s = mint();
        assert_eq!(s.len(), 32, "secret must be 32 hex chars (128 bits): {s}");
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "secret must be lowercase hex only: {s}"
        );
        // No `:` so it stays safe inside colon-delimited keys / logs.
        assert!(!s.contains(':'));
    }

    #[test]
    fn mint_is_unique_across_calls() {
        // 128 bits of entropy → a collision in a handful of draws is
        // astronomically unlikely; this guards a constant-return regression.
        let a = mint();
        let b = mint();
        let c = mint();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn ct_eq_matches_only_identical_tokens() {
        let s = mint();
        assert!(ct_eq(&s, &s.clone()));
        assert!(!ct_eq(&s, &mint()));
        assert!(!ct_eq("abc", "abcd"), "length mismatch is false");
        assert!(!ct_eq("", "x"));
        // Empty vs empty is technically equal but the gate never treats an
        // empty secret as authenticated (handled by the caller, not here).
        assert!(ct_eq("", ""));
    }
}
