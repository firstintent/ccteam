//! V0.5.0 F96 — Claude Code subagent definition resolver.
//!
//! Resolves `.claude/agents/<agentType>.md` across Claude Code's
//! subagent scopes in priority order:
//!
//! 1. **Project** — `<member_cwd>/.claude/agents/<agentType>.md`
//! 2. **User** — `<claude_home>/agents/<agentType>.md`
//! 3. **Plugin** — `<claude_home>/plugins/marketplaces/*/agents/<agentType>.md`
//! 4. **Managed** — `<claude_home>/managed/agents/<agentType>.md`
//!    (V0.4.x ccteam-injected fallback; same file structure)
//!
//! Anthropic's subagent definition file is a markdown doc with a YAML
//! frontmatter fence:
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: ...
//! tools: Read, Grep
//! model: sonnet
//! skills: [security-review]
//! mcpServers: [github]
//! ---
//! You are a code reviewer for the team...
//! ```
//!
//! Per Anthropic's V0.5.0 behaviour, `skills` and `mcpServers` in the
//! frontmatter are **not applied** when the definition runs as a team
//! member (they apply only to subagent Task tool calls). We surface
//! the field list under `skills_not_applied` / `mcp_servers_not_
//! applied` so the SPA can warn the user.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Result of `resolve_definition`. `None` for ad-hoc members; `Some`
/// with `body.is_empty()` when the file is missing but the member
/// claimed a definition-backed `agentType` — the SPA renders that as
/// the "definition file missing" warning (PRD §F96 acceptance #4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentDefinition {
    /// Concrete `<scope>/<agentType>.md` we ended up reading.
    pub path: String,
    /// One of `project | user | plugin | managed`.
    pub scope: ResolvedScope,
    /// Parsed YAML frontmatter as a free-form JSON object (we don't
    /// pin a struct because Anthropic's schema can drift; the SPA
    /// renders whatever fields are there).
    pub frontmatter: serde_json::Value,
    /// Markdown body after the second `---` fence.
    pub body: String,
    /// `frontmatter.skills` list — empty if absent.
    pub skills_not_applied: Vec<String>,
    /// `frontmatter.mcpServers` list — empty if absent.
    pub mcp_servers_not_applied: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedScope {
    Project,
    User,
    Plugin,
    Managed,
}

/// Search every scope in priority order; return the first hit. Caller
/// must have verified the member is definition-backed (`agentType`
/// not in `{general-purpose, team-lead}`) before calling — we don't
/// re-check here.
pub fn resolve_definition(
    claude_home: &Path,
    member_cwd: Option<&Path>,
    agent_type: &str,
) -> Option<AgentDefinition> {
    let candidates = candidate_paths(claude_home, member_cwd, agent_type);
    for (scope, path) in candidates {
        if !path.exists() {
            continue;
        }
        let body = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "subagent definition read failed");
                continue;
            }
        };
        let parsed = parse_definition(&body);
        let (frontmatter, body) = parsed;
        let skills = list_field(&frontmatter, "skills");
        let mcp = list_field(&frontmatter, "mcpServers");
        return Some(AgentDefinition {
            path: path.display().to_string(),
            scope,
            frontmatter,
            body,
            skills_not_applied: skills,
            mcp_servers_not_applied: mcp,
        });
    }
    None
}

/// Build the search list in priority order. Public for testing.
pub fn candidate_paths(
    claude_home: &Path,
    member_cwd: Option<&Path>,
    agent_type: &str,
) -> Vec<(ResolvedScope, PathBuf)> {
    let mut out = Vec::new();
    let file = format!("{agent_type}.md");
    if let Some(cwd) = member_cwd {
        out.push((
            ResolvedScope::Project,
            cwd.join(".claude/agents").join(&file),
        ));
    }
    out.push((ResolvedScope::User, claude_home.join("agents").join(&file)));
    // Plugin marketplaces — glob `<claude_home>/plugins/marketplaces/*/agents/<file>`.
    let marketplaces = claude_home.join("plugins").join("marketplaces");
    if marketplaces.exists() {
        if let Ok(rd) = fs::read_dir(&marketplaces) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let candidate = p.join("agents").join(&file);
                out.push((ResolvedScope::Plugin, candidate));
            }
        }
    }
    out.push((
        ResolvedScope::Managed,
        claude_home.join("managed").join("agents").join(&file),
    ));
    out
}

/// Split `--- yaml --- body` into the parsed frontmatter (as JSON
/// `Value`) plus the body string. No frontmatter → empty JSON object
/// + full text as body.
pub fn parse_definition(text: &str) -> (serde_json::Value, String) {
    // We accept the leading `\u{FEFF}` BOM (some authors edit on
    // Windows + git autocrlf). `\r\n` normalised below.
    let normalised = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let bytes = normalised.as_bytes();
    if !bytes.starts_with(b"---\n") && !bytes.starts_with(b"---") {
        return (
            serde_json::Value::Object(serde_json::Map::new()),
            normalised,
        );
    }
    // Strip the opening fence + newline.
    let after_open = match normalised.strip_prefix("---\n") {
        Some(s) => s,
        None => match normalised.strip_prefix("---") {
            Some(s) => s.trim_start_matches('\n'),
            None => {
                return (
                    serde_json::Value::Object(serde_json::Map::new()),
                    normalised,
                )
            }
        },
    };
    // Find the closing fence.
    let Some(close_idx) = find_closing_fence(after_open) else {
        return (
            serde_json::Value::Object(serde_json::Map::new()),
            normalised,
        );
    };
    let frontmatter_text = &after_open[..close_idx];
    let body_start = close_idx + 3; // skip `---`
    let mut body = &after_open[body_start..];
    // Drop a single leading newline after the closing fence so the
    // body doesn't start with a blank line every time.
    if let Some(rest) = body.strip_prefix('\n') {
        body = rest;
    }
    let frontmatter = match serde_yaml::from_str::<serde_json::Value>(frontmatter_text) {
        Ok(v) if v.is_null() => serde_json::Value::Object(serde_json::Map::new()),
        Ok(v) => v,
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };
    (frontmatter, body.to_string())
}

fn find_closing_fence(s: &str) -> Option<usize> {
    // The closing fence is `---` on its own line. Scan line-by-line so
    // we don't mistake `---` inside a YAML block scalar for a fence.
    let mut idx = 0;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return Some(idx);
        }
        idx += line.len();
    }
    None
}

fn list_field(fm: &serde_json::Value, key: &str) -> Vec<String> {
    let Some(node) = fm.get(key) else {
        return Vec::new();
    };
    match node {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|item| item.as_str().map(|s| s.to_string()))
            .collect(),
        serde_json::Value::String(s) => {
            // Comma-separated form: `tools: Read, Grep`.
            s.split(',').map(|p| p.trim().to_string()).collect()
        }
        _ => Vec::new(),
    }
}
