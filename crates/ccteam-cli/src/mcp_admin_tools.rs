//! V0.6.1 F128 — `ccteam__admin_change_persona` +
//! `ccteam__admin_add_tool` MCP tool definitions + dispatch.
//!
//! Both tools edit `<project>/.claude/agents/<bot>.md`. The skill
//! (`ccteam-control`) is responsible for translating user NL into the
//! concrete arguments — this module does no LLM call, no NL parse;
//! it's a thin wrapper around `ccteam_core::admin_actions`.
//!
//! On success each tool appends one event to the project's
//! `progress.jsonl` (`persona_changed` or `tool_added`) so the web /
//! meta-agent can show "the user changed alice's persona at <ts>".
//!
//! Both tools require a live orchestrator daemon: the persona file
//! lives inside the project tree but the orchestrator watches the
//! agents dir for change events. Without a daemon, the file is
//! updated but the bot won't reload until the next `start`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ccteam_core::{admin_actions, paths::CcteamPaths, progress};

/// 2 admin tool schemas appended to `mcp_serve::tool_definitions()`.
pub fn admin_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ccteam__admin_change_persona",
            "description": "V0.6.1 F128 — replace the persona body of a chat-mode bot (`<project>/.claude/agents/<bot>.md`). `new_persona_md` is the COMPLETE replacement file content (YAML frontmatter + body); the calling skill / agent is responsible for assembling it from any user NL description. The bot picks up the new persona on the next turn / `/clear`. Emits `persona_changed` to progress.jsonl.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "bot": { "type": "string", "description": "Bot persona id (must match an existing .claude/agents/<bot>.md)." },
                    "new_persona_md": { "type": "string", "description": "Complete replacement file content — YAML frontmatter + body." }
                },
                "required": ["slug", "bot", "new_persona_md"],
            }),
        }),
        json!({
            "name": "ccteam__admin_add_tool",
            "description": "V0.6.1 F128 — append a tool to a chat-mode bot's `.claude/agents/<bot>.md` frontmatter `tools:` CSV list. `tool_descriptor` is the Claude Code tool name (e.g. `WebFetch`) or — for skills / MCP tools — the dotted handle the bot should reference. Idempotent: re-adding an existing tool is a no-op. The bot picks up the new tool list on the next turn. Emits `tool_added` to progress.jsonl.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Project slug." },
                    "bot": { "type": "string", "description": "Bot persona id." },
                    "tool_descriptor": { "type": "string", "description": "Tool name / handle to append (verbatim — taken as-is into the CSV list)." }
                },
                "required": ["slug", "bot", "tool_descriptor"],
            }),
        }),
    ]
}

/// Returns `Ok(Some(body))` when `name` is one of the admin tools we
/// own. `Ok(None)` lets the caller fall through to the next
/// dispatcher.
pub fn dispatch(paths: &CcteamPaths, name: &str, args: &Value) -> Result<Option<String>> {
    match name {
        "ccteam__admin_change_persona" => Ok(Some(dispatch_change_persona(paths, args)?)),
        "ccteam__admin_add_tool" => Ok(Some(dispatch_add_tool(paths, args)?)),
        _ => Ok(None),
    }
}

/// Names of admin tools that require a live daemon. The `_ls`
/// admin tool stays daemon-independent (read-only), so we list
/// the two new mutators explicitly.
pub fn requires_daemon(name: &str) -> bool {
    matches!(
        name,
        "ccteam__admin_change_persona" | "ccteam__admin_add_tool"
    )
}

fn dispatch_change_persona(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_str(args, "slug")?;
    let bot = arg_str(args, "bot")?;
    let new_persona_md = arg_str(args, "new_persona_md")?;
    let project_dir = paths.project_dir(&slug);
    if !project_dir.exists() {
        return Err(anyhow!(
            "no project named `{slug}` (looked under {})",
            project_dir.display()
        ));
    }
    let written = admin_actions::change_persona(&project_dir, &bot, &new_persona_md)?;
    let bytes = new_persona_md.len();
    let event = admin_actions::build_persona_changed_event(&bot, &written, bytes);
    let progress_path = paths.progress_jsonl(&slug);
    progress::append_event(&progress_path, &event)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "bot": bot,
        "path": written.display().to_string(),
        "bytes_written": bytes,
        "event": event,
    }))?)
}

fn dispatch_add_tool(paths: &CcteamPaths, args: &Value) -> Result<String> {
    let slug = arg_str(args, "slug")?;
    let bot = arg_str(args, "bot")?;
    let tool = arg_str(args, "tool_descriptor")?;
    let project_dir = paths.project_dir(&slug);
    if !project_dir.exists() {
        return Err(anyhow!(
            "no project named `{slug}` (looked under {})",
            project_dir.display()
        ));
    }
    let res = admin_actions::add_tool(&project_dir, &bot, &tool)?;
    let event = admin_actions::build_tool_added_event(
        &bot,
        &res.path,
        &res.added,
        &res.new_tools_csv,
        res.already_present,
    );
    let progress_path = paths.progress_jsonl(&slug);
    progress::append_event(&progress_path, &event)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "slug": slug,
        "bot": bot,
        "path": res.path.display().to_string(),
        "tool": res.added,
        "tools_csv": res.new_tools_csv,
        "already_present": res.already_present,
        "event": event,
    }))?)
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("missing required string arg `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_admin_tools_registered() {
        let tools = admin_tool_definitions();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ccteam__admin_change_persona"));
        assert!(names.contains(&"ccteam__admin_add_tool"));
    }

    #[test]
    fn all_admin_tools_carry_admin_prefix() {
        for t in admin_tool_definitions() {
            let n = t["name"].as_str().unwrap();
            assert!(
                n.starts_with("ccteam__admin_"),
                "admin tool name must start with ccteam__admin_: {n}"
            );
        }
    }

    #[test]
    fn dispatch_returns_none_for_foreign_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        assert!(dispatch(&paths, "ccteam__workflow_ls", &json!({}))
            .unwrap()
            .is_none());
        assert!(dispatch(&paths, "ccteam__chat_register_bot", &json!({}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn schemas_declare_required_args() {
        let tools = admin_tool_definitions();
        for t in &tools {
            let required = t["inputSchema"]["required"].as_array().unwrap();
            let required_str: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
            assert!(required_str.contains(&"slug"));
            assert!(required_str.contains(&"bot"));
        }
        // Tool-specific extras:
        let cp = tools
            .iter()
            .find(|t| t["name"] == "ccteam__admin_change_persona")
            .unwrap();
        assert!(cp["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("new_persona_md")));
        let at = tools
            .iter()
            .find(|t| t["name"] == "ccteam__admin_add_tool")
            .unwrap();
        assert!(at["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("tool_descriptor")));
    }
}
