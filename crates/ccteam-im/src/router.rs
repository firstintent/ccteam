//! @mention parsing for the IM gateway.
//!
//! IM messages address sessions via `@<handle>` (a role handle or a
//! `s<N>` sid for roleless sessions). The gateway resolves the first
//! mention against its session/template maps (`gateway.rs`); text with
//! no resolvable mention flows to the chat's current session. There is
//! no reserved meta-handle: deterministic control is the slash-command
//! surface (`/status` / `/sessions` / `/stop` …), and free-form ops
//! questions are ordinary chat to the session (e.g. the `cto` role).

/// Extract the **first** `@handle` in the text. Returns the handle
/// (without `@`) plus the rest of the text with that mention stripped.
pub fn parse_first_mention(text: &str) -> Option<(String, String)> {
    let mut handle = String::new();
    let mut start = None;
    for (i, ch) in text.char_indices() {
        if ch == '@' {
            start = Some(i);
            continue;
        }
        if start.is_some() {
            // Handle chars: alphanum / `_` / `-`.
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                handle.push(ch);
            } else {
                break;
            }
        }
    }
    let start = start?;
    if handle.is_empty() {
        return None;
    }
    let end = start + 1 + handle.len();
    let mut stripped = String::with_capacity(text.len() - handle.len() - 1);
    stripped.push_str(&text[..start]);
    if end < text.len() {
        stripped.push_str(&text[end..]);
    }
    Some((handle, stripped.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_mention_only() {
        let (h, rest) = parse_first_mention("@a then @b").unwrap();
        assert_eq!(h, "a");
        assert!(rest.contains("@b"));
    }

    #[test]
    fn handles_alphanum_and_dash_underscore() {
        let (h, _) = parse_first_mention("@bot_1-foo hi").unwrap();
        assert_eq!(h, "bot_1-foo");
    }

    #[test]
    fn ignores_bare_at_symbol() {
        // `@ ` (no handle chars) returns None.
        assert!(parse_first_mention("@ hi").is_none());
    }
}
