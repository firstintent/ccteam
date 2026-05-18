//! Content sanitization for IM → tmux injection.
//!
//! Mirrors `references/oh-my-claudecode/src/notifications/reply-listener.ts`
//! `sanitizeReplyInput()` (see `docs/v0-6-0/wave-2-decisions.md` §4 for
//! the OMC parity contract).
//!
//! Two-layer model:
//!
//! 1. **Content layer** ([`sanitize_reply_input`]) — strips control
//!    chars, bidi overrides, escapes shell metacharacters. Applied
//!    before any further processing.
//! 2. **Tmux layer** ([`sanitize_for_tmux`]) — additionally collapses
//!    newlines into spaces (`tmux send-keys -l` injects literally; an
//!    embedded `\n` would issue an Enter the user didn't intend).

/// Maximum length of a single IM-to-tmux turn after sanitization.
/// V0.6 picks 4096 to match Telegram's per-message ceiling; longer
/// turns are split or truncated by the caller.
pub const MAX_TURN_LEN: usize = 4096;

/// Strip dangerous characters, escape shell metas, normalize the
/// payload for downstream consumption. Direct port of the OMC TS
/// `sanitizeReplyInput()`.
///
/// Pipeline:
/// 1. Strip ASCII control chars (`\x00-\x08`, `\x0b`, `\x0c`,
///    `\x0e-\x1f`, `\x7f`). Keeps `\t`, `\n`, `\r` for now — those are
///    handled by [`sanitize_for_tmux`] when the destination is a tmux
///    pane.
/// 2. Strip Unicode bidi overrides (`U+202A`–`U+202E`,
///    `U+2066`–`U+2069`).
/// 3. Escape `\`, `` ` ``, `$(`, `${` so a payload like `"$(rm -rf /)"`
///    can't trigger command substitution if forwarded into a shell.
/// 4. `trim()`.
pub fn sanitize_reply_input(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let cp = ch as u32;
        // Control chars (keep \t \n \r — newline collapse is the tmux
        // layer's job).
        if matches!(cp, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f) {
            continue;
        }
        // Bidi overrides.
        if matches!(cp, 0x202a..=0x202e | 0x2066..=0x2069) {
            continue;
        }
        out.push(ch);
    }
    // Escape shell-substitution sequences. Order matters: `\` first so
    // we don't double-escape the backslashes we add later.
    out = out
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("$(", "\\$(")
        .replace("${", "\\${");
    out.trim().to_string()
}

/// Stronger variant — runs [`sanitize_reply_input`] then collapses
/// newlines/CR/tab into single spaces. Use this immediately before
/// `tmux send-keys -l <payload>` to prevent literal Enters being
/// pasted into the agent's prompt mid-turn.
pub fn sanitize_for_tmux(text: &str) -> String {
    let mut out = sanitize_reply_input(text);
    out = out.replace(['\r', '\n', '\t'], " ");
    // Collapse runs of whitespace introduced by the line-flattening.
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.trim().to_string()
}

/// Truncate to [`MAX_TURN_LEN`] (UTF-8 char boundary aware).
pub fn truncate_to_max(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    text.chars().take(max_len).collect()
}

/// Sanity-check that a captured tmux pane has non-whitespace content
/// before we inject a turn — empty/whitespace panes are the typical
/// signature of a dead session that we should not paste into. Mirrors
/// OMC `injectReply()` lines 368–374.
pub fn verify_pane_not_empty(pane_capture: &str) -> bool {
    !pane_capture.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_chars_but_keeps_newline_and_tab() {
        let raw = "hello\x00\x01world\x07\nnext\tcol\x7fbad";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("helloworld"));
        assert!(cleaned.contains("\nnext\tcol"));
        assert!(!cleaned.contains('\x00'));
        assert!(!cleaned.contains('\x7f'));
    }

    #[test]
    fn strips_bidi_overrides() {
        // U+202E = right-to-left override (classic phishing payload).
        let raw = "safe \u{202e}!evilcode\u{2069}";
        let cleaned = sanitize_reply_input(raw);
        assert!(!cleaned.contains('\u{202e}'));
        assert!(!cleaned.contains('\u{2069}'));
        assert!(cleaned.contains("evilcode"));
    }

    #[test]
    fn escapes_command_substitution() {
        let raw = "innocent text $(rm -rf /) and ${HOME}/bad";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("\\$("));
        assert!(cleaned.contains("\\${"));
        // The escape is in place — a literal `$(` (no leading
        // backslash) must not survive. Use a window-search that
        // requires the preceding byte to NOT be a backslash.
        let unescaped_dollar_paren = cleaned
            .as_bytes()
            .windows(2)
            .enumerate()
            .any(|(i, w)| w == b"$(" && (i == 0 || cleaned.as_bytes()[i - 1] != b'\\'));
        assert!(
            !unescaped_dollar_paren,
            "found unescaped `$(` in {cleaned:?}"
        );
    }

    #[test]
    fn escapes_backticks_and_backslashes() {
        let raw = "before `whoami` after \\ndone";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("\\`whoami\\`"));
        // Original backslash got doubled.
        assert!(cleaned.contains("\\\\ndone"));
    }

    #[test]
    fn tmux_variant_collapses_newlines() {
        let raw = "line one\nline two\r\nline three";
        let cleaned = sanitize_for_tmux(raw);
        assert!(!cleaned.contains('\n'));
        assert!(!cleaned.contains('\r'));
        assert!(cleaned.contains("line one line two line three"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let raw = "αβγδε".repeat(2000);
        let out = truncate_to_max(&raw, 100);
        assert_eq!(out.chars().count(), 100);
        // Must still be valid UTF-8 — implicit because we used .chars().
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn verify_pane_not_empty_rejects_whitespace() {
        assert!(!verify_pane_not_empty(""));
        assert!(!verify_pane_not_empty("   \n\t  "));
        assert!(verify_pane_not_empty("$ ccteam start"));
    }
}
