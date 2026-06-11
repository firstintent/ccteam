//! Seam ① (PRD §七) — the **pure** spawn-spec builder for the Claude
//! stream-json adapter: argv / env / cwd only, zero IO. Kept a free
//! function with no `self` so a future `ccteam satellite` runner (v0.9
//! host axis) can build byte-identical argv on a remote box without
//! dragging in the adapter's live state.
//!
//! ## Zero-injection red line (CLAUDE.md §三)
//!
//! [`build_argv`] **never** emits `--append-system-prompt`, and the
//! adapter never sends an `initialize.systemPrompt` / `appendSystemPrompt`
//! field. Role persona is bound *only* via vendor-native `--agent <role>`
//! (the agent self-reads `.claude/agents/<role>.md`); an empty role omits
//! `--agent` entirely (roleless = bare claude reads the project's own
//! `CLAUDE.md`) — the same legitimate shape as the tmux path.

use std::path::Path;

use crate::PermissionMode;

/// Inputs to [`build_argv`] — everything that varies per spawn. Borrowed
/// so the builder stays allocation-light and the caller owns the strings.
#[derive(Debug, Clone, Copy)]
pub struct StreamJsonSpawnInput<'a> {
    /// Role persona (`--agent <role>`); empty = roleless (omit `--agent`).
    pub role: &'a str,
    /// Minted vendor session UUID. Bound via `--session-id` on a fresh
    /// spawn or `--resume` on a wake-up (the two are **mutually
    /// exclusive** — passing both makes claude silently swallow stdin).
    pub session_uuid: &'a str,
    /// `true` → `--resume <uuid>` (reload prior context); `false` →
    /// `--session-id <uuid>` (mint a fresh session bound to our id).
    pub resume: bool,
    /// Concrete model id (`--model`); `None`/empty = vendor default.
    pub model_id: Option<&'a str>,
    /// Per-session permission posture.
    pub permission_mode: PermissionMode,
}

/// Build the `claude` argv for a long-running stream-json session.
///
/// Flags verified against `claude --help` 2.1.170 + the live VS Code
/// extension process capture (see `docs/research/cc-stream-json-protocol.md`
/// §1). **No `-p`** (PRD §四 Q1): this is a long-lived interactive process
/// with stdin held open across the whole multi-turn session, not a
/// one-shot. `--debug --debug-to-stderr` guarantees any diagnostic claude
/// emits lands on stderr (which the transport drains) and never pollutes
/// the stdout NDJSON stream.
pub fn build_argv(bin: &str, input: &StreamJsonSpawnInput<'_>) -> Vec<String> {
    let mut argv = vec![
        bin.to_string(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--include-partial-messages".into(),
        "--verbose".into(),
        "--replay-user-messages".into(),
        "--debug".into(),
        "--debug-to-stderr".into(),
    ];

    // Role persona — vendor-native self-read, never injected. Empty =
    // roleless (omit `--agent`).
    if !input.role.is_empty() {
        argv.push("--agent".into());
        argv.push(input.role.to_string());
    }

    if let Some(model) = input.model_id.map(str::trim).filter(|m| !m.is_empty()) {
        argv.push("--model".into());
        argv.push(model.to_string());
    }

    // Permission posture. Skip = no prompts (today's default); Hitl =
    // route every non-allowlist tool through the `can_use_tool` reverse
    // RPC to the IM user (`--permission-prompt-tool stdio`). The native
    // ask-path needs `--permission-mode default` alongside it so a
    // user-global auto mode can't mask prompts.
    match input.permission_mode {
        PermissionMode::Skip => argv.push("--dangerously-skip-permissions".into()),
        PermissionMode::Hitl => {
            argv.push("--permission-prompt-tool".into());
            argv.push("stdio".into());
            argv.push("--permission-mode".into());
            argv.push("default".into());
        }
    }

    // Identity — mutually exclusive with the prior arg.
    if input.resume {
        argv.push("--resume".into());
    } else {
        argv.push("--session-id".into());
    }
    argv.push(input.session_uuid.to_string());

    argv
}

/// Env pairs forwarded into the stream-json child. Mirrors the tmux
/// path's `chat_spawn_env_owned` so the in-process MCP forwarder (cto
/// scheduling gate) can authenticate `session_*` calls against the
/// daemon's `sid -> {role, secret}` map. Empty `secret` / `sid` omit the
/// var (tests / legacy), preserving a minimal env exactly.
///
/// NOTE: unlike the tmux path there is **no** progress hook here — the
/// stream-json adapter reads the child's stdout directly, so
/// `CCTEAM_CHAT_ROLE` / `CCTEAM_CHAT_SLUG` are forwarded only for the MCP
/// forwarder's benefit, not for any hook subprocess.
pub fn build_env(role: &str, slug: &str, secret: &str, sid: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("CCTEAM_CHAT_ROLE".to_string(), role.to_string()),
        ("CCTEAM_CHAT_SLUG".to_string(), slug.to_string()),
    ];
    if !secret.is_empty() {
        env.push(("CCTEAM_CHAT_SECRET".to_string(), secret.to_string()));
    }
    if !sid.is_empty() {
        env.push(("CCTEAM_CHAT_SID".to_string(), sid.to_string()));
    }
    env
}

/// Mint a fresh RFC-4122 v4 UUID for `--session-id`, dependency-free
/// (claude requires a valid UUID string). Reads 16 bytes from
/// `/dev/urandom`; on the (vanishingly rare) read failure it falls back
/// to a time-seeded value so a spawn never hard-fails on entropy. Kept
/// here (not the transport) because the id is part of session identity,
/// which the spawn spec owns.
pub fn mint_session_uuid() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    // Read EXACTLY 16 bytes — `/dev/urandom` is an infinite stream, so a
    // whole-file read never returns. `read_exact` stops at 16.
    let got = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes).map(|_| 16usize))
        .unwrap_or(0);
    if got < 16 {
        // Entropy fallback: blend the wall clock + a process-unique seed.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let seed = nanos ^ (pid << 64);
        bytes.copy_from_slice(&seed.to_le_bytes());
    }
    // Set the version (4) and variant (RFC 4122) bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&bytes[0..4]),
        h(&bytes[4..6]),
        h(&bytes[6..8]),
        h(&bytes[8..10]),
        h(&bytes[10..16]),
    )
}

/// Derive a **stable** RFC-4122-v4-shaped UUID from `(slug, sid)`, so the
/// same session always maps to the same `--session-id` / `--resume`
/// target across daemon restarts and idle wake-ups — the stateless key to
/// resume-by-sid (PRD E1). sids are monotonic + never reused, so the
/// derived uuid is unique per session. Dependency-free FNV-1a over two
/// orderings of the input for the 16 bytes.
///
/// This is the §七 ⑤ identity-mapping primitive: `sid → vendor_uuid` is a
/// pure function here; the adapter stores `{sid, vendor_uuid, host}` so
/// v0.9 can hang a `Sandbox CR` off the same record without a re-key.
pub fn deterministic_session_uuid(slug: &str, sid: &str) -> String {
    let lo = fnv1a64(format!("{slug}\u{0}{sid}").as_bytes());
    let hi = fnv1a64(format!("{sid}\u{0}{slug}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&lo.to_be_bytes());
    bytes[8..16].copy_from_slice(&hi.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&bytes[0..4]),
        h(&bytes[4..6]),
        h(&bytes[6..8]),
        h(&bytes[8..10]),
        h(&bytes[10..16]),
    )
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Resolve the `claude` binary, honoring `CCTEAM_CLAUDE_BIN` (tests point
/// it at a fake NDJSON-emitting script). Free fn so [`build_argv`] callers
/// and the transport agree on the program path.
pub fn claude_bin() -> String {
    std::env::var(crate::CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

/// True only for a well-formed lowercase RFC-4122 v4 UUID string — used to
/// reject a malformed `--resume` target before a spawn. Kept tiny + pure.
pub fn looks_like_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, &c)| {
        if matches!(i, 8 | 13 | 18 | 23) {
            c == b'-'
        } else {
            c.is_ascii_hexdigit()
        }
    })
}

/// Bind the child's working directory. Trivial today (just the cwd) but a
/// named seam so the satellite runner can later remap a remote path.
pub fn working_dir(cwd: &Path) -> std::path::PathBuf {
    cwd.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn input<'a>(role: &'a str, uuid: &'a str, resume: bool) -> StreamJsonSpawnInput<'a> {
        StreamJsonSpawnInput {
            role,
            session_uuid: uuid,
            resume,
            model_id: None,
            permission_mode: PermissionMode::Skip,
        }
    }

    #[test]
    fn argv_has_no_dash_p_and_core_stream_flags() {
        let argv = build_argv("claude", &input("alice", "u-1", false));
        assert!(
            !argv.iter().any(|a| a == "-p"),
            "must NOT carry -p: {argv:?}"
        );
        for flag in [
            "--input-format",
            "stream-json",
            "--output-format",
            "--include-partial-messages",
            "--replay-user-messages",
        ] {
            assert!(argv.iter().any(|a| a == flag), "missing {flag}: {argv:?}");
        }
    }

    #[test]
    fn argv_never_injects_system_prompt() {
        // Zero-injection red line: --append-system-prompt must never appear.
        let argv = build_argv("claude", &input("alice", "u-1", false));
        assert!(!argv.iter().any(|a| a == "--append-system-prompt"));
        assert!(!argv.iter().any(|a| a.contains("system-prompt")));
    }

    #[test]
    fn fresh_spawn_uses_session_id_resume_uses_resume() {
        let fresh = build_argv("claude", &input("alice", "u-abc", false));
        let i = fresh.iter().position(|a| a == "--session-id").unwrap();
        assert_eq!(fresh[i + 1], "u-abc");
        assert!(!fresh.iter().any(|a| a == "--resume"));

        let woken = build_argv("claude", &input("alice", "u-abc", true));
        let j = woken.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(woken[j + 1], "u-abc");
        assert!(!woken.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn roleless_omits_agent() {
        let with_role = build_argv("claude", &input("alice", "u-1", false));
        assert!(with_role.iter().any(|a| a == "--agent"));
        let roleless = build_argv("claude", &input("", "u-1", false));
        assert!(!roleless.iter().any(|a| a == "--agent"));
    }

    #[test]
    fn skip_vs_hitl_permission_flags() {
        let skip = build_argv("claude", &input("alice", "u-1", false));
        assert!(skip.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!skip.iter().any(|a| a == "--permission-prompt-tool"));

        let hitl = build_argv(
            "claude",
            &StreamJsonSpawnInput {
                permission_mode: PermissionMode::Hitl,
                ..input("alice", "u-1", false)
            },
        );
        assert!(!hitl.iter().any(|a| a == "--dangerously-skip-permissions"));
        let k = hitl
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(hitl[k + 1], "stdio");
        assert!(hitl
            .windows(2)
            .any(|w| w == ["--permission-mode", "default"]));
    }

    #[test]
    fn env_omits_empty_secret_and_sid() {
        let env = build_env("alice", "demo", "", "");
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"CCTEAM_CHAT_ROLE"));
        assert!(keys.contains(&"CCTEAM_CHAT_SLUG"));
        assert!(!keys.contains(&"CCTEAM_CHAT_SECRET"));
        assert!(!keys.contains(&"CCTEAM_CHAT_SID"));

        let env2 = build_env("alice", "demo", "sek", "s3");
        let map: std::collections::HashMap<_, _> = env2.into_iter().collect();
        assert_eq!(
            map.get("CCTEAM_CHAT_SECRET").map(String::as_str),
            Some("sek")
        );
        assert_eq!(map.get("CCTEAM_CHAT_SID").map(String::as_str), Some("s3"));
    }

    #[test]
    fn minted_uuid_is_well_formed_v4() {
        let u = mint_session_uuid();
        assert!(looks_like_uuid(&u), "not a uuid: {u}");
        // Version nibble is 4.
        assert_eq!(u.as_bytes()[14], b'4');
        // Two mints differ (overwhelmingly).
        assert_ne!(mint_session_uuid(), mint_session_uuid());
    }

    #[test]
    fn looks_like_uuid_rejects_garbage() {
        assert!(!looks_like_uuid("not-a-uuid"));
        assert!(!looks_like_uuid(""));
        assert!(!looks_like_uuid(&"f".repeat(36)));
        assert!(looks_like_uuid("12345678-1234-4234-8234-1234567890ab"));
    }

    #[test]
    fn working_dir_is_cwd() {
        let p = PathBuf::from("/tmp/x");
        assert_eq!(working_dir(&p), p);
    }

    #[test]
    fn deterministic_uuid_is_stable_unique_and_well_formed() {
        let a1 = deterministic_session_uuid("demo", "s1");
        let a2 = deterministic_session_uuid("demo", "s1");
        assert_eq!(a1, a2, "same (slug, sid) → same uuid (resume key)");
        assert!(looks_like_uuid(&a1), "not a uuid: {a1}");
        assert_eq!(a1.as_bytes()[14], b'4', "version nibble must be 4");
        // Different sid / slug → different uuid.
        assert_ne!(a1, deterministic_session_uuid("demo", "s2"));
        assert_ne!(a1, deterministic_session_uuid("other", "s1"));
    }
}
