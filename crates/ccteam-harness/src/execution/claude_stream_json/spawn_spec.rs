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

use crate::execution::claude_common;
use crate::PermissionMode;

// Binary resolution is shared with the tmux path (`claude_common`); re-exported
// here so existing `spawn_spec::claude_bin` callers keep resolving.
pub use claude_common::claude_bin;
// `strip_context_tag` moved to `claude_common` (used via `push_model_arg`); the
// re-export exists only so the module's own strip test keeps resolving it.
#[cfg(test)]
pub(crate) use claude_common::strip_context_tag;

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
    /// v0.8.24 A-U3 — reasoning effort (`--effort low|medium|high|xhigh|max`,
    /// verified against claude 2.1.207 `--help`); `None`/empty = vendor
    /// default (flag omitted).
    pub effort: Option<&'a str>,
    /// Per-session permission posture.
    pub permission_mode: PermissionMode,
    /// Path to curated per-session `--mcp-config` JSON (only ccteam MCP).
    /// `None` keeps historical strip-all behavior (`--strict-mcp-config` alone).
    pub mcp_config_path: Option<&'a Path>,
}

/// Build the `claude` argv for a long-running stream-json session.
///
/// Flags verified against `claude --help` 2.1.173 + the live VS Code
/// extension process capture (see `docs/research/cc-stream-json-protocol.md`
/// §1) and an on-machine A/B repro. **`--no-chrome` is the headless
/// trigger**: `--input-format`/`--output-format stream-json` only take
/// effect under `--print` OR `--no-chrome`; with neither, claude boots the
/// interactive TUI and never emits `system:init` (the "timed out waiting
/// for claude system:init" failure — every stream-json spawn dies at
/// init). We use `--no-chrome`, **not** `-p`/`--print`: this is a
/// long-lived resident process holding stdin open across the whole
/// multi-turn session, whereas `-p` is one-shot — `--no-chrome` is exactly
/// the shape the VS Code extension (a persistent client) launches.
/// `--debug --debug-to-stderr` keeps any diagnostic on stderr (drained by
/// the transport) so it never pollutes the stdout NDJSON stream.
pub fn build_argv(bin: &str, input: &StreamJsonSpawnInput<'_>) -> Vec<String> {
    let mut argv = vec![
        bin.to_string(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        // Headless trigger — WITHOUT this, stream-json I/O is ignored and
        // no `system:init` is ever emitted (see the doc comment above).
        "--no-chrome".into(),
        "--include-partial-messages".into(),
        "--verbose".into(),
        "--replay-user-messages".into(),
        "--debug".into(),
        "--debug-to-stderr".into(),
        // Do NOT inherit the user's ambient MCP servers. stream-json gates
        // `system:init` on every MCP server connecting, so ANY ambient server
        // that has to reach the daemon we are spawning from cannot connect: the
        // daemon is synchronously blocked in `wait_for_init` while holding the
        // gateway lock → that self-referential server never initializes → init
        // never arrives → timeout. The historical shape of that trap was the
        // global `ccteam` entry spawning an `internal mcp-serve` child that
        // dialled `mcp.sock` (the command is now deleted; today's entry is HTTP
        // against `POST /mcp`, whose credential check deliberately takes no
        // gateway lock). The chat-only adapter reads stdout directly
        // and needs no in-pane MCP, so strip them all (also drops unrelated
        // ambient servers like a VS Code extension that would only add init
        // latency). When `mcp_config_path` is set, `--mcp-config` + this flag
        // loads ONLY that curated file (ccteam MCP + per-session secret).
        "--strict-mcp-config".into(),
        // Include ALL three settings layers — crucially `local`
        // (`settings.local.json`), where a marketplace-enabled vendor plugin
        // writes its `extraKnownMarketplaces` + `enabledPlugins` (so the
        // plugin actually loads in this default protocol). `local` also
        // carries ccteam's tmux-path chat hooks, and historically the
        // `SessionStart` hook deadlocked init (it HTTP-POSTs back to the
        // daemon while the daemon is synchronously spawning this child) — that
        // is now defused at the source: the spawn marks the child
        // `CCTEAM_HOOKLESS=1` (see `build_env`) and `hook.sh` / `internal
        // hook` no-op for it, so no hook ever fires for a stream-json session
        // (it stays hookless — events come from stdout). MCP self-reference is
        // still handled by `--strict-mcp-config`; permissions by the flags
        // below.
        "--setting-sources=user,project,local".into(),
    ];

    // Curated per-session MCP (v0.8.24 C1): only load this config under
    // --strict-mcp-config (empty path = strip ambient, historical behavior).
    if let Some(path) = input.mcp_config_path {
        argv.push("--mcp-config".into());
        argv.push(path.display().to_string());
    }

    // Role persona (`--agent <role>`, omitted when roleless) + `--model`.
    // Shared with the tmux path via `claude_common`. `strip_1m = true`: the
    // `[1m]` suffix is ccteam's 1M-context DISPLAY tag, not part of any claude
    // model id — `--model …[1m]` is rejected and claude silently defaults to
    // sonnet (the model-loss-on-restart bug), so the base id goes here and the
    // 1M window is re-requested post-init via `set_model`.
    claude_common::push_agent_arg(&mut argv, input.role);
    claude_common::push_model_arg(&mut argv, input.model_id, true);

    // Explicit reasoning effort (v0.8.24 A-U3). Only when the caller picked
    // one — `None`/empty keeps claude's own default (no flag).
    if let Some(effort) = input.effort.map(str::trim).filter(|e| !e.is_empty()) {
        argv.push("--effort".into());
        argv.push(effort.to_string());
    }

    // Permission posture. The Skip flag / `--permission-mode default` core is
    // shared (`claude_common::permission_args`). Stream-json ADDITIONALLY routes
    // every non-allowlist tool through the `can_use_tool` reverse RPC
    // (`--permission-prompt-tool stdio`) for Hitl — a transport-specific extra
    // the tmux path has no equivalent to (it uses the `PermissionRequest` hook).
    if input.permission_mode.is_hitl() {
        argv.push("--permission-prompt-tool".into());
        argv.push("stdio".into());
    }
    argv.extend(claude_common::permission_args(input.permission_mode));

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
    // ROLE + SLUG base and the optional SECRET / SID are shared with the tmux
    // path (`claude_common`). The `CCTEAM_HOOKLESS` marker is stream-json-only
    // and sits BETWEEN the base and the secret/sid to preserve the exact env
    // ordering this path historically emitted: this protocol is hookless
    // (events come from the child's stdout), yet it includes `local` in
    // `--setting-sources` (so a marketplace-enabled plugin loads) which also
    // pulls in ccteam's tmux-path hooks — so mark the child hookless and
    // `hook.sh` / `internal hook` no-op for it (no SessionStart-POST init
    // deadlock, no double-emit). See spawn argv `--setting-sources`.
    let mut env = claude_common::chat_env_role_slug(role, slug);
    env.push(("CCTEAM_HOOKLESS".to_string(), "1".to_string()));
    claude_common::push_secret_sid(&mut env, secret, sid);
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
            effort: None,
            permission_mode: PermissionMode::Skip,
            mcp_config_path: None,
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
            // The headless trigger — its absence is the init-timeout bug.
            "--no-chrome",
            // Don't inherit ambient MCP (self-referential init deadlock).
            "--strict-mcp-config",
            // All three layers incl. `local` (plugin enablement lives there);
            // local hooks are defused via CCTEAM_HOOKLESS — see build_argv.
            "--setting-sources=user,project,local",
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
    fn strip_context_tag_removes_1m_suffix() {
        assert_eq!(strip_context_tag("claude-opus-4-8[1m]"), "claude-opus-4-8");
        assert_eq!(strip_context_tag("opus[1m]"), "opus");
        assert_eq!(
            strip_context_tag("claude-sonnet-4-6[1M]"),
            "claude-sonnet-4-6"
        );
        // No tag → unchanged.
        assert_eq!(strip_context_tag("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(strip_context_tag("sonnet"), "sonnet");
    }

    #[test]
    fn resume_model_arg_strips_1m_tag() {
        // The model-loss-on-restart fix: `--model` must carry the BASE id, never
        // the `[1m]`-tagged form claude rejects (→ silent default to sonnet).
        let mut inp = input("", "u-1", true);
        inp.model_id = Some("claude-opus-4-8[1m]");
        let argv = build_argv("claude", &inp);
        let i = argv
            .iter()
            .position(|a| a == "--model")
            .expect("--model present");
        assert_eq!(argv[i + 1], "claude-opus-4-8", "must strip [1m]: {argv:?}");
        assert!(
            !argv.iter().any(|a| a.contains("[1m]")),
            "no [1m] anywhere in argv: {argv:?}"
        );
    }

    #[test]
    fn effort_flag_only_when_picked() {
        // Default: no --effort (vendor default preserved).
        let argv = build_argv("claude", &input("alice", "u-1", false));
        assert!(!argv.iter().any(|a| a == "--effort"), "{argv:?}");
        // Explicit pick → `--effort <level>` verbatim.
        let mut inp = input("alice", "u-1", false);
        inp.effort = Some("max");
        let argv = build_argv("claude", &inp);
        let i = argv.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(argv[i + 1], "max");
        // Whitespace-only = not picked.
        let mut inp = input("alice", "u-1", false);
        inp.effort = Some("  ");
        assert!(!build_argv("claude", &inp).iter().any(|a| a == "--effort"));
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
        // Hookless marker is ALWAYS present (stream-json is hookless even
        // though it now reads the `local` settings layer for plugins).
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CCTEAM_HOOKLESS").map(String::as_str), Some("1"));

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

    #[test]
    fn argv_includes_mcp_config_when_path_set() {
        let path = PathBuf::from("/tmp/demo/.ccteam/chat/s1/mcp.json");
        let mut inp = input("cto", "u-1", false);
        inp.mcp_config_path = Some(path.as_path());
        let argv = build_argv("claude", &inp);
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
        let i = argv
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("--mcp-config present");
        assert_eq!(argv[i + 1], path.display().to_string());
    }

    #[test]
    fn argv_omits_mcp_config_when_none() {
        let argv = build_argv("claude", &input("cto", "u-1", false));
        assert!(!argv.iter().any(|a| a == "--mcp-config"));
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
    }
}
