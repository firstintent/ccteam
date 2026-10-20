//! Claude's `get_context_usage` answer → [`ContextBreakdown`] (issue #21).
//!
//! The control_request is the data behind claude's own `/context`: categories
//! (system prompt, tools, memory files, skills, MCP, messages, …) plus the
//! entries behind them and a split of the conversation by tool / attachment.
//! It is an internal protocol, not a published API — the live CLI already
//! answers with more than the reference schema (`id`, `kind`, `path`, …) — so
//! the parse is lenient: only `totalTokens` and a window are required, a
//! missing `id` / `kind` is derived, and anything unknown is ignored.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    ContextAttachmentUsage, ContextBreakdown, ContextCategory, ContextCategoryKind, ContextItem,
    ContextMessages, ContextRequestUsage, ContextToolUsage,
};

/// `None` only when the occupancy itself is missing — everything else degrades
/// to an empty list / `None` field.
pub(super) fn parse_context_usage(resp: &Value, now: DateTime<Utc>) -> Option<ContextBreakdown> {
    let used_tokens = num(resp.get("totalTokens"))?;
    let window_tokens = num(resp.get("maxTokens")).or_else(|| num(resp.get("rawMaxTokens")))?;
    let mut categories: Vec<ContextCategory> = list(resp.get("categories"))
        .filter_map(parse_category)
        .collect();
    fill_items(&mut categories, resp);
    let auto_compact_enabled = resp
        .get("isAutoCompactEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Some(ContextBreakdown {
        vendor: "claude".into(),
        model: text(resp.get("model")),
        captured_at: now,
        used_tokens,
        window_tokens,
        auto_compact_threshold: num(resp.get("autoCompactThreshold"))
            .filter(|_| auto_compact_enabled),
        categories,
        messages: resp.get("messageBreakdown").and_then(parse_messages),
        last_request: resp.get("apiUsage").and_then(parse_request_usage),
    })
}

fn parse_category(v: &Value) -> Option<ContextCategory> {
    let name = text(v.get("name"))?;
    let id = text(v.get("id")).unwrap_or_else(|| slug(&name));
    let kind = match v.get("kind").and_then(Value::as_str) {
        Some("used") => ContextCategoryKind::Used,
        Some("deferred") => ContextCategoryKind::Deferred,
        Some("buffer") => ContextCategoryKind::Buffer,
        Some("free") => ContextCategoryKind::Free,
        // Older CLIs only flag deferral; free space and the compaction buffer
        // are recognisable by the ids claude has always given them.
        _ if v.get("isDeferred").and_then(Value::as_bool) == Some(true) => {
            ContextCategoryKind::Deferred
        }
        _ if id == "free_space" => ContextCategoryKind::Free,
        _ if id == "autocompact_buffer" => ContextCategoryKind::Buffer,
        _ => ContextCategoryKind::Used,
    };
    Some(ContextCategory {
        id,
        name,
        tokens: num(v.get("tokens")).unwrap_or(0),
        kind,
        items: Vec::new(),
    })
}

/// Hang each itemized list under the category it makes up. Deferrable
/// entries (MCP tools, deferred built-in tools) split by `isLoaded`: a loaded
/// one occupies the window and belongs to the used category, the rest to its
/// `(deferred)` twin — the same split claude's own `/context` draws.
fn fill_items(categories: &mut [ContextCategory], resp: &Value) {
    let item = |v: &Value, name_key: &str, source_key: Option<&str>, detail_key: Option<&str>| {
        Some(ContextItem {
            name: text(v.get(name_key))?,
            source: source_key.and_then(|k| text(v.get(k))),
            detail: detail_key.and_then(|k| text(v.get(k))),
            tokens: num(v.get("tokens")).unwrap_or(0),
            loaded: v.get("isLoaded").and_then(Value::as_bool),
        })
    };
    let mut put = |id: &str, entry: ContextItem| {
        if let Some(c) = categories.iter_mut().find(|c| c.id == id) {
            c.items.push(entry);
        }
    };
    for v in list(resp.get("memoryFiles")) {
        if let Some(e) = item(v, "path", Some("type"), None) {
            put("memory_files", e);
        }
    }
    let skills = resp.get("skills").and_then(|s| s.get("skillFrontmatter"));
    for v in list(skills) {
        if let Some(e) = item(v, "name", Some("source"), Some("path")) {
            put("skills", e);
        }
    }
    for v in list(resp.get("agents")) {
        if let Some(e) = item(v, "agentType", Some("source"), Some("path")) {
            put("custom_agents", e);
        }
    }
    for v in list(resp.get("mcpInstructions")) {
        if let Some(e) = item(v, "serverName", None, None) {
            put("mcp_instructions", e);
        }
    }
    for v in list(resp.get("systemPromptSections")) {
        if let Some(e) = item(v, "name", None, None) {
            put("system_prompt", e);
        }
    }
    for v in list(resp.get("systemTools")) {
        if let Some(e) = item(v, "name", None, None) {
            put("system_tools", e);
        }
    }
    let has = |categories: &[ContextCategory], id: &str| categories.iter().any(|c| c.id == id);
    let mut deferrable: Vec<(&str, &str, ContextItem)> = Vec::new();
    for v in list(resp.get("mcpTools")) {
        if let Some(e) = item(v, "name", Some("serverName"), None) {
            deferrable.push(("mcp_tools", "mcp_tools_deferred", e));
        }
    }
    for v in list(resp.get("deferredBuiltinTools")) {
        if let Some(e) = item(v, "name", None, None) {
            deferrable.push(("system_tools", "system_tools_deferred", e));
        }
    }
    let placed: Vec<(String, ContextItem)> = deferrable
        .into_iter()
        .filter_map(|(loaded_id, deferred_id, e)| {
            let wants_deferred = e.loaded == Some(false);
            let (first, second) = if wants_deferred {
                (deferred_id, loaded_id)
            } else {
                (loaded_id, deferred_id)
            };
            let id = [first, second].into_iter().find(|id| has(categories, id))?;
            Some((id.to_string(), e))
        })
        .collect();
    for (id, e) in placed {
        if let Some(c) = categories.iter_mut().find(|c| c.id == id) {
            c.items.push(e);
        }
    }
}

fn parse_messages(v: &Value) -> Option<ContextMessages> {
    v.as_object()?;
    let n = |k: &str| num(v.get(k)).unwrap_or(0);
    Some(ContextMessages {
        tool_call_tokens: n("toolCallTokens"),
        tool_result_tokens: n("toolResultTokens"),
        attachment_tokens: n("attachmentTokens"),
        assistant_tokens: n("assistantMessageTokens"),
        user_tokens: n("userMessageTokens"),
        other_tokens: n("redirectedContextTokens") + n("unattributedTokens"),
        tools: list(v.get("toolCallsByType"))
            .filter_map(|t| {
                Some(ContextToolUsage {
                    name: text(t.get("name"))?,
                    call_tokens: num(t.get("callTokens")).unwrap_or(0),
                    result_tokens: num(t.get("resultTokens")).unwrap_or(0),
                })
            })
            .collect(),
        attachments: list(v.get("attachmentsByType"))
            .filter_map(|a| {
                Some(ContextAttachmentUsage {
                    name: text(a.get("name"))?,
                    tokens: num(a.get("tokens")).unwrap_or(0),
                })
            })
            .collect(),
    })
}

fn parse_request_usage(v: &Value) -> Option<ContextRequestUsage> {
    v.as_object()?;
    let n = |k: &str| num(v.get(k)).unwrap_or(0);
    Some(ContextRequestUsage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_creation_tokens: n("cache_creation_input_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
    })
}

fn list(v: Option<&Value>) -> impl Iterator<Item = &Value> {
    v.and_then(Value::as_array).into_iter().flatten()
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Token counts arrive as JSON numbers that are integral in practice; a float
/// is rounded rather than rejected, a negative or non-number is absent.
fn num(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f.round() as u64)
    })
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Trimmed from a real claude 2.1.304 answer on a resumed 250k session.
    fn live_payload() -> Value {
        json!({
            "categories": [
                {"id":"system_prompt","name":"System prompt","tokens":2247,"color":"promptBorder","kind":"used"},
                {"id":"system_tools","name":"System tools","tokens":6714,"color":"inactive","kind":"used"},
                {"id":"mcp_tools","name":"MCP tools","tokens":941,"color":"cyan","kind":"used"},
                {"id":"mcp_instructions","name":"MCP server instructions","tokens":436,"kind":"used"},
                {"id":"mcp_tools_deferred","name":"MCP tools (deferred)","tokens":649,"isDeferred":true,"kind":"deferred"},
                {"id":"system_tools_deferred","name":"System tools (deferred)","tokens":12011,"isDeferred":true,"kind":"deferred"},
                {"id":"custom_agents","name":"Custom agents","tokens":145,"kind":"used"},
                {"id":"memory_files","name":"Memory files","tokens":11592,"kind":"used"},
                {"id":"skills","name":"Skills","tokens":79,"kind":"used"},
                {"id":"messages","name":"Messages","tokens":231616,"kind":"used"},
                {"id":"autocompact_buffer","name":"Autocompact buffer","tokens":33000,"kind":"buffer"},
                {"id":"free_space","name":"Free space","tokens":713230,"kind":"free"}
            ],
            "totalTokens": 253770,
            "maxTokens": 1000000,
            "rawMaxTokens": 1000000,
            "autoCompactThreshold": 967000,
            "isAutoCompactEnabled": true,
            "percentage": 25,
            "model": "claude-sonnet-5-5",
            "gridRows": [[{"color":"x","isFilled":true}]],
            "memoryFiles": [
                {"path":"/home/u/.claude/CLAUDE.md","type":"User","tokens":103},
                {"path":"/repo/CLAUDE.md","type":"Project","tokens":11489}
            ],
            "mcpTools": [
                {"name":"mcp__ccteam__agent","serverName":"ccteam","tokens":941,"isLoaded":true},
                {"name":"mcp__ccteam__agent_read","serverName":"ccteam","tokens":649,"isLoaded":false}
            ],
            "mcpInstructions": [{"serverName":"ccteam","tokens":436}],
            "agents": [{"agentType":"cto","source":"projectSettings","path":"/repo/.claude/agents/cto.md","tokens":145}],
            "skills": {"totalSkills":1,"includedSkills":1,"tokens":79,
                "skillFrontmatter":[{"name":"graphify","source":"userSettings","path":"/home/u/.claude/skills/graphify/SKILL.md","tokens":79}]},
            "messageBreakdown": {
                "toolCallTokens":36149,"toolResultTokens":62805,"attachmentTokens":69528,
                "assistantMessageTokens":67354,"userMessageTokens":1732,
                "redirectedContextTokens":0,"unattributedTokens":5,
                "toolCallsByType":[{"name":"Bash","callTokens":34420,"resultTokens":61749}],
                "attachmentsByType":[{"name":"prompt_snapshot","tokens":26483}]
            },
            "apiUsage": {"input_tokens":2,"output_tokens":1259,"cache_creation_input_tokens":1022,"cache_read_input_tokens":252179}
        })
    }

    fn cat<'a>(b: &'a ContextBreakdown, id: &str) -> &'a ContextCategory {
        b.categories.iter().find(|c| c.id == id).expect(id)
    }

    #[test]
    fn a_live_answer_keeps_every_category_in_order_with_its_kind() {
        let b = parse_context_usage(&live_payload(), Utc::now()).unwrap();
        assert_eq!((b.used_tokens, b.window_tokens), (253_770, 1_000_000));
        assert_eq!(b.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(b.auto_compact_threshold, Some(967_000));
        let ids: Vec<&str> = b.categories.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.first(), Some(&"system_prompt"));
        assert_eq!(ids.last(), Some(&"free_space"));
        assert_eq!(
            cat(&b, "mcp_tools_deferred").kind,
            ContextCategoryKind::Deferred
        );
        assert_eq!(
            cat(&b, "autocompact_buffer").kind,
            ContextCategoryKind::Buffer
        );
        assert_eq!(cat(&b, "free_space").kind, ContextCategoryKind::Free);
        let used: u64 = b
            .categories
            .iter()
            .filter(|c| c.kind == ContextCategoryKind::Used)
            .map(|c| c.tokens)
            .sum();
        assert_eq!(
            used, b.used_tokens,
            "used categories account for the whole occupancy"
        );
    }

    #[test]
    fn entries_hang_under_the_category_they_make_up() {
        let b = parse_context_usage(&live_payload(), Utc::now()).unwrap();
        let memory = &cat(&b, "memory_files").items;
        assert_eq!(memory.len(), 2);
        assert_eq!(memory[1].name, "/repo/CLAUDE.md");
        assert_eq!(memory[1].source.as_deref(), Some("Project"));
        let skill = &cat(&b, "skills").items[0];
        assert_eq!(skill.source.as_deref(), Some("userSettings"));
        assert!(skill
            .detail
            .as_deref()
            .unwrap()
            .ends_with("graphify/SKILL.md"));
        assert_eq!(cat(&b, "custom_agents").items[0].name, "cto");
        assert_eq!(cat(&b, "mcp_instructions").items[0].name, "ccteam");
        // A loaded MCP tool occupies the window; an unloaded one does not.
        let loaded = &cat(&b, "mcp_tools").items;
        let deferred = &cat(&b, "mcp_tools_deferred").items;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].loaded, Some(true));
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].name, "mcp__ccteam__agent_read");
        assert_eq!(deferred[0].source.as_deref(), Some("ccteam"));
    }

    #[test]
    fn the_conversation_splits_by_tool_and_attachment() {
        let b = parse_context_usage(&live_payload(), Utc::now()).unwrap();
        let m = b.messages.unwrap();
        assert_eq!(m.tool_result_tokens, 62_805);
        assert_eq!(m.assistant_tokens, 67_354);
        assert_eq!(m.other_tokens, 5);
        assert_eq!(m.tools[0].name, "Bash");
        assert_eq!(m.tools[0].result_tokens, 61_749);
        assert_eq!(m.attachments[0].name, "prompt_snapshot");
        let r = b.last_request.unwrap();
        assert_eq!(
            (r.cache_read_tokens, r.cache_creation_tokens),
            (252_179, 1_022)
        );
    }

    #[test]
    fn a_bare_or_older_answer_degrades_instead_of_failing() {
        // The minimal shape older CLIs (and the test fakes) answer with.
        let b = parse_context_usage(&json!({"totalTokens":2000,"maxTokens":200000}), Utc::now())
            .unwrap();
        assert_eq!((b.used_tokens, b.window_tokens), (2_000, 200_000));
        assert!(b.categories.is_empty());
        assert_eq!(b.messages, None);
        assert_eq!(b.last_request, None);
        // No `id` / `kind`: derived from the name and the deferral flag.
        let b = parse_context_usage(
            &json!({"totalTokens":10,"rawMaxTokens":100,"isAutoCompactEnabled":false,
                    "autoCompactThreshold":90,"apiUsage":null,
                    "categories":[{"name":"MCP tools (deferred)","tokens":4.0,"isDeferred":true},
                                  {"name":"Free space","tokens":90}],
                    "mcpTools":[{"name":"t","serverName":"s","tokens":4,"isLoaded":false}]}),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(b.window_tokens, 100);
        assert_eq!(b.auto_compact_threshold, None, "auto-compaction is off");
        assert_eq!(b.categories[0].id, "mcp_tools_deferred");
        assert_eq!(b.categories[0].kind, ContextCategoryKind::Deferred);
        assert_eq!(b.categories[0].items[0].name, "t");
        assert_eq!(b.categories[1].kind, ContextCategoryKind::Free);
        // Occupancy is the one thing that cannot be invented.
        assert_eq!(
            parse_context_usage(&json!({"maxTokens":1}), Utc::now()),
            None
        );
        assert_eq!(
            parse_context_usage(&json!({"totalTokens":1}), Utc::now()),
            None
        );
    }
}
