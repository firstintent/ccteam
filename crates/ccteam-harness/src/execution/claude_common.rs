//! Shared Claude-adapter primitives — the small, drift-prone bits both Claude
//! spawn paths ([`super::claude_tui::ClaudeTuiAdapter`] over tmux/PTY and
//! [`super::claude_stream_json::ClaudeStreamJsonAdapter`] over bidirectional
//! NDJSON) construct identically. Extracted so an upstream `claude` CLI change
//! (a new flag, a renamed permission mode, a tweaked env var) is fixed **once**
//! rather than twice-and-drifting.
//!
//! Scope is deliberately narrow: binary resolution, the shared argv segments
//! (`--agent`, `--model`, permission core), the chat identity env, and the
//! ChoicePrompt token. Everything transport-specific stays in each adapter:
//! the stream-json-only flags (`--no-chrome`, `--input-format`, …), the
//! `--permission-prompt-tool stdio` HITL extra (reverse-RPC channel), the
//! `CCTEAM_HOOKLESS` marker, and the two DIFFERENT slash-command
//! classifications (tui popup categories vs the stream-json bridge gate) —
//! those are NOT unified because their behavior genuinely differs by protocol.
//!
//! ## Zero-injection red line (CLAUDE.md §三)
//!
//! Role persona is bound **only** via vendor-native `--agent <role>` (the agent
//! self-reads `.claude/agents/<role>.md`); an empty role omits `--agent`
//! entirely (roleless = bare claude reads the project's own `CLAUDE.md`). None
//! of these helpers ever emit `--append-system-prompt` or any system-prompt
//! field.

use crate::PermissionMode;

/// Resolve the `claude` binary path, honoring the `CCTEAM_CLAUDE_BIN` override
/// (tests point it at a fake NDJSON-emitting / tmux-runnable script). Single
/// source so every spawn path agrees on the program path.
pub fn claude_bin() -> String {
    std::env::var(crate::CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

/// Push the `--agent <role>` persona pair onto `argv` **iff** `role` is
/// non-empty. An empty role (roleless) omits `--agent` so bare claude reads the
/// project's own `CLAUDE.md` — the same legitimate zero-injection shape on both
/// spawn paths.
pub fn push_agent_arg(argv: &mut Vec<String>, role: &str) {
    if !role.is_empty() {
        argv.push("--agent".to_string());
        argv.push(role.to_string());
    }
}

/// Strip a trailing `[1m]` (case-insensitive) — ccteam's 1M-context **display**
/// tag — from a model id. claude's `--model` rejects the tagged form (`…[1m]`)
/// and silently defaults to sonnet; the bare base id (short alias or full id)
/// is accepted. The 1M window is re-requested separately (stream-json:
/// `set_model` post-init).
pub fn strip_context_tag(model: &str) -> &str {
    let m = model.trim();
    if m.len() >= 4 && m[m.len() - 4..].eq_ignore_ascii_case("[1m]") {
        m[..m.len() - 4].trim_end()
    } else {
        m
    }
}

/// Push the `--model <id>` pair onto `argv` when a non-blank model is set
/// (blank / whitespace-only omits it → vendor default). `strip_1m` controls
/// whether the `[1m]` display tag is stripped from the id first:
///
/// - **stream-json** passes `strip_1m = true`: `claude --model X[1m]` is
///   rejected (→ silent default to sonnet), so the base id goes on argv and the
///   1M window is re-requested via `set_model` post-init.
/// - **tmux/tui** passes `strip_1m = false`: it preserves the tui path's
///   historical behavior of forwarding the model id verbatim (see the divergence
///   note in the module extraction handoff — this is preserved, not unified).
pub fn push_model_arg(argv: &mut Vec<String>, model_id: Option<&str>, strip_1m: bool) {
    if let Some(model) = model_id.map(str::trim).filter(|m| !m.is_empty()) {
        argv.push("--model".to_string());
        let value = if strip_1m {
            strip_context_tag(model)
        } else {
            model
        };
        argv.push(value.to_string());
    }
}

/// The **shared core** of the permission-posture argv segment, common to both
/// spawn paths:
///
/// - `Skip` → `["--dangerously-skip-permissions"]` (every tool runs, no prompt).
/// - `Hitl` → `["--permission-mode", "default"]` (drops the skip flag so the
///   native ask-path stays alive; MANDATORY because a user-global auto mode
///   would otherwise mask prompts).
///
/// Transport-specific extras are added by the caller AROUND this core, never
/// folded in here: the stream-json path prepends `--permission-prompt-tool
/// stdio` for `Hitl` (its `can_use_tool` reverse-RPC channel); the tmux path
/// carries no such flag (it uses the `PermissionRequest` hook instead).
pub fn permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Skip => vec!["--dangerously-skip-permissions".to_string()],
        PermissionMode::Hitl => vec!["--permission-mode".to_string(), "default".to_string()],
    }
}

/// Environment key for the chat session's role persona (read by the tmux-path
/// hook subprocess + the in-pane `session_*` MCP forwarder).
pub const CHAT_ROLE_ENV: &str = "CCTEAM_CHAT_ROLE";
/// Environment key for the chat session's project slug.
pub const CHAT_SLUG_ENV: &str = "CCTEAM_CHAT_SLUG";
/// Environment key for the per-session secret authenticating `session_*` calls
/// to the daemon's `sid -> {role, secret}` map.
pub const CHAT_SECRET_ENV: &str = "CCTEAM_CHAT_SECRET";
/// Environment key carrying ccteam's `s<N>` session id (NOT the Anthropic
/// session UUID) to the hook / forwarder.
pub const CHAT_SID_ENV: &str = "CCTEAM_CHAT_SID";

/// The always-present base of a chat session's spawn env: `CCTEAM_CHAT_ROLE` +
/// `CCTEAM_CHAT_SLUG`. Callers append protocol-specific vars (e.g. the
/// stream-json `CCTEAM_HOOKLESS` marker) BEFORE calling [`push_secret_sid`], so
/// the on-wire ordering each adapter historically produced is preserved.
pub fn chat_env_role_slug(role: &str, slug: &str) -> Vec<(String, String)> {
    vec![
        (CHAT_ROLE_ENV.to_string(), role.to_string()),
        (CHAT_SLUG_ENV.to_string(), slug.to_string()),
    ]
}

/// Append `CCTEAM_CHAT_SECRET` / `CCTEAM_CHAT_SID` to `env`, each ONLY when
/// non-empty (an empty secret / sid — tests, legacy — omits the var entirely,
/// preserving a minimal env exactly). Shared so the "inject the secret/sid only
/// when present" rule can never drift between the two spawn paths.
pub fn push_secret_sid(env: &mut Vec<(String, String)>, secret: &str, sid: &str) {
    if !secret.is_empty() {
        env.push((CHAT_SECRET_ENV.to_string(), secret.to_string()));
    }
    if !sid.is_empty() {
        env.push((CHAT_SID_ENV.to_string(), sid.to_string()));
    }
}

/// A per-prompt unique [`crate::ChoicePrompt`] token (≤16B ASCII, no `:`): the
/// low 40 bits of the current wall-clock nanos, hex, behind a caller-supplied
/// `prefix`. The gateway resolves picker callbacks token-globally, so a
/// name-based token would collide when two sessions raise the same command's
/// picker at once. Prefixes distinguish the source (`cj` = tui popup, `cm` =
/// stream-json model/choice picker).
pub fn unique_prompt_token(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}{:x}", (nanos as u64) & 0xff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_agent_arg_omits_for_roleless() {
        let mut argv = vec!["claude".to_string()];
        push_agent_arg(&mut argv, "");
        assert_eq!(argv, vec!["claude".to_string()]);
        push_agent_arg(&mut argv, "dev");
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--agent".to_string(),
                "dev".to_string()
            ]
        );
    }

    #[test]
    fn strip_context_tag_removes_1m_suffix() {
        assert_eq!(strip_context_tag("claude-opus-4-8[1m]"), "claude-opus-4-8");
        assert_eq!(strip_context_tag("opus[1m]"), "opus");
        assert_eq!(
            strip_context_tag("claude-sonnet-4-6[1M]"),
            "claude-sonnet-4-6"
        );
        assert_eq!(strip_context_tag("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(strip_context_tag("sonnet"), "sonnet");
    }

    #[test]
    fn push_model_arg_respects_strip_flag_and_skips_blank() {
        // strip=true (stream-json): [1m] tag removed.
        let mut a = Vec::new();
        push_model_arg(&mut a, Some("opus[1m]"), true);
        assert_eq!(a, vec!["--model".to_string(), "opus".to_string()]);
        // strip=false (tui): verbatim.
        let mut b = Vec::new();
        push_model_arg(&mut b, Some("opus[1m]"), false);
        assert_eq!(b, vec!["--model".to_string(), "opus[1m]".to_string()]);
        // Blank / None → no pair either way.
        let mut c = Vec::new();
        push_model_arg(&mut c, Some("  "), true);
        push_model_arg(&mut c, None, false);
        assert!(c.is_empty());
    }

    #[test]
    fn permission_args_core() {
        assert_eq!(
            permission_args(PermissionMode::Skip),
            vec!["--dangerously-skip-permissions".to_string()]
        );
        assert_eq!(
            permission_args(PermissionMode::Hitl),
            vec!["--permission-mode".to_string(), "default".to_string()]
        );
        assert!(!permission_args(PermissionMode::Hitl)
            .contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn env_base_and_optional_secret_sid() {
        let mut env = chat_env_role_slug("dev", "demo");
        assert_eq!(
            env,
            vec![
                ("CCTEAM_CHAT_ROLE".to_string(), "dev".to_string()),
                ("CCTEAM_CHAT_SLUG".to_string(), "demo".to_string()),
            ]
        );
        // Empty secret + sid → nothing appended.
        push_secret_sid(&mut env, "", "");
        assert_eq!(env.len(), 2);
        // Non-empty → appended in order.
        push_secret_sid(&mut env, "sek", "s3");
        let map: std::collections::HashMap<_, _> = env.iter().cloned().collect();
        assert_eq!(
            map.get("CCTEAM_CHAT_SECRET").map(String::as_str),
            Some("sek")
        );
        assert_eq!(map.get("CCTEAM_CHAT_SID").map(String::as_str), Some("s3"));
    }

    #[test]
    fn unique_prompt_token_carries_prefix_and_is_hex() {
        let t = unique_prompt_token("cj");
        assert!(t.starts_with("cj"), "token {t} missing prefix");
        assert!(!t.contains(':'), "token {t} must have no colon");
        assert!(t.len() <= 16, "token {t} exceeds 16 bytes");
        assert!(t[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }
}
