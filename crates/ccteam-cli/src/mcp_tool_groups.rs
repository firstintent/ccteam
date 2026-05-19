//! V0.6.0 F111 — MCP tool group enum + `CCTEAM_DISABLE_TOOLS` env filter.
//!
//! Each MCP tool exposed by `ccteam mcp-serve` belongs to exactly one
//! group: `admin`, `workflow`, `screenshot`, `chat`, `advise`. The
//! server name remains `ccteam` (V0.5 user muscle memory preserved —
//! see `docs/versions/v0-6-0/README.md` §九 F111 decision). Group prefixes
//! make the tool surface scannable in `/mcp` listings and give us a
//! clean env-driven way to disable categories the user doesn't care
//! about (e.g. `CCTEAM_DISABLE_TOOLS=chat,screenshot`).
//!
//! Wave 1 model:
//! - 1 admin tool (`ls`)
//! - 15 workflow tools (V0.5 9 + V0.4 F65 7, minus screenshot)
//! - 1 screenshot tool
//! - 5 chat stubs (Wave 2 F108)
//! - 2 advise stubs (Wave 3 F112)
//!
//! Total: 24 tools registered Wave 1. Disabling a group hides every
//! tool in that group from `tools/list`; `tools/call` against a
//! disabled tool falls through to the standard "unknown tool" error.

use std::collections::HashSet;

/// Group an MCP tool belongs to. Each tool is registered with exactly
/// one group. Names match the lowercase form accepted by
/// `CCTEAM_DISABLE_TOOLS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    /// Cross-project admin (`ls`).
    Admin,
    /// Per-workflow operations (`show`, `peek`, F65 spawn/stop/...).
    Workflow,
    /// Pure-read tmux pane → PNG capture; its own group so heavy /
    /// privacy-sensitive deployments can disable it independently.
    Screenshot,
    /// Wave 2 F108 chat workflow (5 tools).
    Chat,
    /// Wave 3 F112 Codex + Claude parallel advisor (2 tools).
    Advise,
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
            ToolGroup::Advise => "advise",
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
            "advise" => Some(Self::Advise),
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

/// Map a full tool name (e.g. `ccteam__workflow_show`) to its group.
/// Returns `None` for unknown names so unrecognised tools can't be
/// filtered out accidentally.
pub fn group_for_tool(name: &str) -> Option<ToolGroup> {
    // Strip the `ccteam__` server prefix; bail if the convention
    // doesn't hold (defensive — callers should always pass our own
    // tool names).
    let bare = name.strip_prefix("ccteam__")?;
    // `screenshot` is the one single-member group that does not carry
    // a sub-prefix (V0.5 muscle memory: `ccteam__screenshot` stays).
    if bare == "screenshot" {
        return Some(ToolGroup::Screenshot);
    }
    if let Some(rest) = bare.strip_prefix("admin_") {
        let _ = rest; // intentionally unused; presence is enough.
        return Some(ToolGroup::Admin);
    }
    if bare.strip_prefix("workflow_").is_some() {
        return Some(ToolGroup::Workflow);
    }
    if bare.strip_prefix("chat_").is_some() {
        return Some(ToolGroup::Chat);
    }
    if bare.strip_prefix("advise_").is_some() {
        return Some(ToolGroup::Advise);
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
        let s = parse_disable_env(Some("chat, screenshot ,advise"));
        assert!(s.contains(&ToolGroup::Chat));
        assert!(s.contains(&ToolGroup::Screenshot));
        assert!(s.contains(&ToolGroup::Advise));
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
    fn group_for_tool_maps_each_prefix() {
        assert_eq!(group_for_tool("ccteam__admin_ls"), Some(ToolGroup::Admin));
        assert_eq!(
            group_for_tool("ccteam__workflow_show"),
            Some(ToolGroup::Workflow)
        );
        assert_eq!(
            group_for_tool("ccteam__screenshot"),
            Some(ToolGroup::Screenshot)
        );
        assert_eq!(
            group_for_tool("ccteam__chat_send_input"),
            Some(ToolGroup::Chat)
        );
        assert_eq!(
            group_for_tool("ccteam__advise_vote"),
            Some(ToolGroup::Advise)
        );
        assert_eq!(group_for_tool("ccteam__bogus"), None);
        assert_eq!(group_for_tool("not-a-ccteam-tool"), None);
    }

    #[test]
    fn filter_by_disabled_hides_matching_groups_only() {
        let tools = vec![
            json!({ "name": "ccteam__admin_ls" }),
            json!({ "name": "ccteam__workflow_show" }),
            json!({ "name": "ccteam__screenshot" }),
            json!({ "name": "ccteam__chat_send_input" }),
            json!({ "name": "ccteam__advise_vote" }),
        ];
        let mut disabled = HashSet::new();
        disabled.insert(ToolGroup::Chat);
        disabled.insert(ToolGroup::Screenshot);
        let kept = filter_by_disabled(tools, &disabled);
        let names: Vec<&str> = kept
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "ccteam__admin_ls",
                "ccteam__workflow_show",
                "ccteam__advise_vote",
            ]
        );
    }

    #[test]
    fn filter_by_disabled_empty_set_is_passthrough() {
        let tools = vec![
            json!({ "name": "ccteam__admin_ls" }),
            json!({ "name": "ccteam__chat_send_input" }),
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
            ToolGroup::Advise,
        ] {
            assert_eq!(ToolGroup::parse(g.as_str()), Some(g));
            // Case insensitive.
            assert_eq!(
                ToolGroup::parse(&g.as_str().to_uppercase()),
                Some(g)
            );
        }
    }
}
