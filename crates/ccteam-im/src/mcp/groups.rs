//! MCP tool group enum + `CCTEAM_DISABLE_TOOLS` env filter.
//!
//! Each MCP tool exposed by `ccteam MCP server` belongs to exactly one
//! group: `admin`, `workflow`, `screenshot`, `chat`, `session`. The
//! server name remains `ccteam` (V0.5 user muscle memory preserved).
//! Group prefixes make the tool surface scannable in `/mcp` listings
//! and give a clean env-driven way to disable categories (e.g.
//! `CCTEAM_DISABLE_TOOLS=chat,screenshot`).
//!
//! Group model (v0.9 T1 — culled surface):
//! - 1 admin tool (`status`, renamed from `admin_ls`)
//! - 0 workflow tools (variant kept so the `workflow_` prefix routing
//!   + env toggle stay stable)
//! - 1 screenshot tool
//! - 1 chat tool (`send_file`)
//! - 5 session tools (spawn / dispatch / collect / list / stop)
//!
//! Total: 8 tools registered. Disabling a group hides every tool in
//! that group from `tools/list`; `tools/call` against a disabled tool
//! falls through to the standard "unknown tool" error.

use std::collections::HashSet;

/// Explicit allow-list of MCP tool names whose dispatch path is still a
/// STUB (returns "not implemented" / sentinel response). Empty after
/// V0.6.5 (every shipped MCP tool has a real dispatch).
///
/// Any PR introducing a new STUB tool MUST add its wire name (e.g.
/// `advise_something`) to this list — `ccteam doctor
/// --verify-mcp` reads this const to compute `stub_count` and exits
/// non-zero when it sees STUBs (in CI builds) so unwired tools cannot
/// silently ship. When the dispatch is later wired, drop the name from
/// this list in the same PR that removes the stub body.
pub const STUB_TOOLS: &[&str] = &[];

/// Group an MCP tool belongs to. Each tool is registered with exactly
/// one group. Names match the lowercase form accepted by
/// `CCTEAM_DISABLE_TOOLS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    /// Cross-project admin (`status` — daemon health + sessions + cost).
    Admin,
    /// Per-workflow operations (retired; variant kept for env toggle).
    Workflow,
    /// Pure-read tmux pane → PNG capture; its own group so heavy /
    /// privacy-sensitive deployments can disable it independently.
    Screenshot,
    /// Chat-mode bot workflow (`send_file` only after v0.9 T1).
    Chat,
    /// cto scheduling over the gateway session map (5 tools):
    /// spawn / dispatch / collect / list / stop.
    Session,
}

impl ToolGroup {
    /// Canonical lowercase name, matched against `CCTEAM_DISABLE_TOOLS`
    /// comma-list entries.
    #[allow(dead_code)] // Public API; used in tests + Wave 2 sub-skills.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolGroup::Admin => "admin",
            ToolGroup::Workflow => "workflow",
            ToolGroup::Screenshot => "screenshot",
            ToolGroup::Chat => "chat",
            ToolGroup::Session => "session",
        }
    }

    /// Parse a single group token. Returns `None` for unknown names so
    /// typos in `CCTEAM_DISABLE_TOOLS` are silently ignored rather
    /// than crashing the MCP server at startup. Case-insensitive.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_lowercase().as_str() {
            "admin" => Some(Self::Admin),
            "workflow" => Some(Self::Workflow),
            "screenshot" => Some(Self::Screenshot),
            "chat" => Some(Self::Chat),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

/// Parse `CCTEAM_DISABLE_TOOLS` env into a `HashSet<ToolGroup>`. Empty
/// / unset / all-whitespace returns the empty set. Unknown tokens are
/// dropped silently (logged at WARN by callers if desired).
///
/// Tests can pass `env_value` directly; production calls
/// [`disabled_groups_from_env`].
pub fn parse_disable_env(env_value: Option<&str>) -> HashSet<ToolGroup> {
    let mut out = HashSet::new();
    let Some(raw) = env_value else {
        return out;
    };
    if raw.trim().is_empty() {
        return out;
    }
    for tok in raw.split(',') {
        if let Some(g) = ToolGroup::parse(tok) {
            out.insert(g);
        }
    }
    out
}

/// Production entry point: reads `CCTEAM_DISABLE_TOOLS` from the
/// process env and parses it. `mcp-serve` calls this once per
/// `tools/list` so users can toggle groups without restarting the
/// server (cheap: a single env-var read + small split loop).
pub fn disabled_groups_from_env() -> HashSet<ToolGroup> {
    let env = std::env::var("CCTEAM_DISABLE_TOOLS").ok();
    parse_disable_env(env.as_deref())
}

/// Map a wire tool name (e.g. `session_spawn`) to its group.
/// Returns `None` for unknown names so unrecognised tools can't be
/// filtered out accidentally.
///
/// Wire names are BARE (no `ccteam__` prefix): MCP clients already
/// namespace by server key (Claude Code shows `mcp__ccteam__session_spawn`),
/// so a baked-in prefix would double up.
pub fn group_for_tool(name: &str) -> Option<ToolGroup> {
    let bare = name;
    // Single-member groups that do not carry a sub-prefix:
    // - `screenshot` (V0.5 muscle memory)
    // - `status` (v0.9 T1 rename of `admin_ls` — admin group, no prefix)
    if bare == "screenshot" {
        return Some(ToolGroup::Screenshot);
    }
    if bare == "status" {
        return Some(ToolGroup::Admin);
    }
    if bare.strip_prefix("admin_").is_some() {
        return Some(ToolGroup::Admin);
    }
    if bare.strip_prefix("workflow_").is_some() {
        return Some(ToolGroup::Workflow);
    }
    if bare.strip_prefix("chat_").is_some() {
        return Some(ToolGroup::Chat);
    }
    if bare.strip_prefix("session_").is_some() {
        return Some(ToolGroup::Session);
    }
    None
}

/// Filter a vector of tool definitions (each `serde_json::Value` with
/// a `name` string) by removing any whose group is in `disabled`.
/// Tools whose name doesn't map to any known group are kept (safe
/// default — `screenshot` etc. handled by [`group_for_tool`]).
pub fn filter_by_disabled(
    tools: Vec<serde_json::Value>,
    disabled: &HashSet<ToolGroup>,
) -> Vec<serde_json::Value> {
    if disabled.is_empty() {
        return tools;
    }
    tools
        .into_iter()
        .filter(|t| {
            let Some(name) = t.get("name").and_then(|v| v.as_str()) else {
                return true;
            };
            match group_for_tool(name) {
                Some(g) => !disabled.contains(&g),
                None => true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_disable_env_empty_returns_empty_set() {
        assert!(parse_disable_env(None).is_empty());
        assert!(parse_disable_env(Some("")).is_empty());
        assert!(parse_disable_env(Some("   ")).is_empty());
    }

    #[test]
    fn parse_disable_env_handles_comma_list_with_whitespace() {
        let s = parse_disable_env(Some("chat, screenshot ,session"));
        assert!(s.contains(&ToolGroup::Chat));
        assert!(s.contains(&ToolGroup::Screenshot));
        assert!(s.contains(&ToolGroup::Session));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn parse_disable_env_silently_drops_unknown_tokens() {
        let s = parse_disable_env(Some("chat,not-a-real-group,workflow"));
        assert_eq!(s.len(), 2);
        assert!(s.contains(&ToolGroup::Chat));
        assert!(s.contains(&ToolGroup::Workflow));
    }

    #[test]
    fn parse_disable_env_drops_retired_advise_token() {
        // v0.9 T1 dropped the advise group; the token is now unknown.
        let s = parse_disable_env(Some("advise"));
        assert!(s.is_empty());
    }

    #[test]
    fn group_for_tool_maps_each_prefix() {
        assert_eq!(group_for_tool("status"), Some(ToolGroup::Admin));
        assert_eq!(group_for_tool("workflow_show"), Some(ToolGroup::Workflow));
        assert_eq!(group_for_tool("screenshot"), Some(ToolGroup::Screenshot));
        assert_eq!(group_for_tool("chat_send_file"), Some(ToolGroup::Chat));
        assert_eq!(group_for_tool("session_spawn"), Some(ToolGroup::Session));
        assert_eq!(group_for_tool("bogus"), None);
        assert_eq!(group_for_tool("not-a-ccteam-tool"), None);
        // Culled tools no longer map (advise group dropped).
        assert_eq!(group_for_tool("advise_vote"), None);
        // Legacy prefixed wire names (pre-rename) no longer map either —
        // the client's server namespace made the baked-in prefix redundant.
        assert_eq!(group_for_tool("ccteam__session_spawn"), None);
    }

    #[test]
    fn filter_by_disabled_hides_matching_groups_only() {
        let tools = vec![
            json!({ "name": "status" }),
            json!({ "name": "screenshot" }),
            json!({ "name": "chat_send_file" }),
            json!({ "name": "session_list" }),
        ];
        let mut disabled = HashSet::new();
        disabled.insert(ToolGroup::Chat);
        disabled.insert(ToolGroup::Screenshot);
        let kept = filter_by_disabled(tools, &disabled);
        let names: Vec<&str> = kept.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["status", "session_list"]);
    }

    #[test]
    fn filter_by_disabled_empty_set_is_passthrough() {
        let tools = vec![
            json!({ "name": "status" }),
            json!({ "name": "chat_send_file" }),
        ];
        let kept = filter_by_disabled(tools.clone(), &HashSet::new());
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn group_as_str_roundtrips_with_parse() {
        for g in [
            ToolGroup::Admin,
            ToolGroup::Workflow,
            ToolGroup::Screenshot,
            ToolGroup::Chat,
            ToolGroup::Session,
        ] {
            assert_eq!(ToolGroup::parse(g.as_str()), Some(g));
            // Case insensitive.
            assert_eq!(ToolGroup::parse(&g.as_str().to_uppercase()), Some(g));
        }
    }
}
