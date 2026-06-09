//! Model-family classification used for user-facing support warnings.
//!
//! This is deliberately separate from pricing. Pricing answers "do we
//! have rates for this string"; this module answers "is this model name
//! plausibly a Claude-family model when routed through the Claude CLI".

/// Return true for Claude-family model identifiers and common Claude CLI
/// aliases. Suffixes such as `[1m]` are ignored.
pub fn is_claude_family(model: &str) -> bool {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return true;
    }
    let base = trimmed.split_once('[').map_or(trimmed, |(head, _)| head);
    let lower = base.to_ascii_lowercase();
    lower.starts_with("claude-")
        || lower.starts_with("sonnet")
        || lower.starts_with("opus")
        || lower.starts_with("haiku")
}

#[cfg(test)]
mod tests {
    use super::is_claude_family;

    #[test]
    fn claude_family_accepts_aliases_and_future_claude_ids() {
        for model in [
            "sonnet",
            "opus",
            "haiku",
            "sonnet[1m]",
            "claude-sonnet-4-6",
            "claude-future-99",
        ] {
            assert!(is_claude_family(model), "{model} should be Claude-family");
        }
    }

    #[test]
    fn claude_family_rejects_non_claude_models() {
        for model in ["deepseek-via-claude", "gpt-5", "o3"] {
            assert!(
                !is_claude_family(model),
                "{model} should not be Claude-family"
            );
        }
    }
}
