//! OS host identity helpers.
//!
//! v0.8.18 柱1 — the `GET /api/v1/hosts` report needs this machine's
//! hostname. Lifted here (from the CLI's private copy) so both the CLI's
//! web-URL line and `ccteam-web`'s host report read ONE implementation.
//! `ccteam-core` already carries `libc`, so the `gethostname(2)` path has
//! no new dependency.

/// Read the OS hostname via libc `gethostname`. Returns `None` on syscall
/// failure or a non-UTF8 result (the caller then falls back to `local` /
/// the bind address).
#[cfg(unix)]
pub fn read_hostname() -> Option<String> {
    use std::ffi::CStr;
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `len-1` bytes and NUL-terminates.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
    if rc != 0 {
        return None;
    }
    // Ensure NUL-termination before scanning the buffer.
    buf[buf.len() - 1] = 0;
    let cstr = CStr::from_bytes_until_nul(&buf).ok()?;
    let s = cstr.to_str().ok()?.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Non-unix fallback: no portable `gethostname` here, so callers degrade to
/// a synthetic id.
#[cfg(not(unix))]
pub fn read_hostname() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn read_hostname_returns_nonempty_on_unix() {
        // A real unix box always has a hostname; assert we get a non-empty
        // string rather than an exact value (which varies per machine/CI).
        let h = read_hostname();
        assert!(h.is_some(), "unix should resolve a hostname");
        assert!(!h.unwrap().is_empty());
    }
}
