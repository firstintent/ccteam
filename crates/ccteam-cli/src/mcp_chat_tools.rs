//! `ccteam__chat_*` MCP tools.
//!
//! After v0.9 T1 the chat group is a single tool: `chat_send_file`.
//! Tool **definitions** live in [`ccteam_im::mcp`]; this module keeps the
//! slug/handle validators the CLI `admin register-bot` path still reuses,
//! and re-exports the definition for local callers.

use std::path::Path;

use anyhow::{anyhow, Result};

use ccteam_core::agent_naming::pick_unused_bot_name;
use ccteam_im::list_bots_in;

/// Auto-mint an unused scientist nickname from the current registry.
/// Used by the CLI `admin register-bot` path (MCP register was culled).
pub(crate) fn mint_unused_handle(ccteam_root: &Path) -> Result<String> {
    let existing = list_bots_in(ccteam_root, None).unwrap_or_default();
    let in_use: Vec<String> = existing
        .iter()
        .map(|b| b.chat_handle.clone().unwrap_or_else(|| b.role.clone()))
        .collect();
    Ok(pick_unused_bot_name(&in_use))
}

/// Caller-supplied handles share the slug validator rules so registry
/// filenames + router parse paths stay clean (alphanumeric / `_` / `-`).
///
/// Shared by the CLI `admin register-bot` path.
pub(crate) fn validate_chat_handle(s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(anyhow!("`chat_handle` must be non-empty"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(anyhow!(
            "`chat_handle` may contain only [a-zA-Z0-9_-]: `{s}`"
        ));
    }
    Ok(())
}

/// Shared slug/role validator the CLI `admin register-bot` path reuses
/// (alphanumerics + `-` + `_`). Distinct from
/// `ccteam_core::validate_slug_format`, which is stricter (lowercase
/// + digits + dashes only) and gates init-time project slugs.
pub(crate) fn validate_slug(s: &str, field: &str) -> Result<()> {
    if s.is_empty() {
        return Err(anyhow!("`{field}` must be non-empty"));
    }
    // Path-injection guard — slug becomes a dir component.
    if s.contains('/') || s.contains('\\') || s == "." || s == ".." || s.starts_with('.') {
        return Err(anyhow!(
            "`{field}` contains illegal characters or path component: `{s}`"
        ));
    }
    // Conservative: alphanumeric + `-` + `_` only.
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!("`{field}` may contain only [a-zA-Z0-9_-]: `{s}`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_im::mcp::chat_tool_definitions;

    #[test]
    fn one_chat_tool_registered_send_file() {
        let tools = chat_tool_definitions();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "ccteam__chat_send_file");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        let req: Vec<&str> = tools[0]["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(req, vec!["path"]);
    }

    #[test]
    fn validate_slug_accepts_safe_tokens() {
        assert!(validate_slug("demo", "slug").is_ok());
        assert!(validate_slug("dev_gamma-2", "role").is_ok());
        assert!(validate_slug("", "slug").is_err());
        assert!(validate_slug("../x", "slug").is_err());
        assert!(validate_slug("a/b", "slug").is_err());
    }

    #[test]
    fn validate_chat_handle_charset() {
        assert!(validate_chat_handle("curie").is_ok());
        assert!(validate_chat_handle("").is_err());
        assert!(validate_chat_handle("bad name").is_err());
    }
}
