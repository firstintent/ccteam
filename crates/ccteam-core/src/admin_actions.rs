//! V0.6.1 F128 — admin file-mutation helpers behind
//! `/ccteam-control change-persona` + `/ccteam-control add-tool`.
//!
//! Both subcommands edit a chat-mode bot's
//! `<project>/.claude/agents/<bot>.md` (the canonical Claude Code
//! agent-definition file). Keeping the IO + parsing helpers in
//! `ccteam-core` lets the MCP dispatcher (`ccteam-cli`) stay a thin
//! arg-parse + emit-event shell and lets the integration tests cover
//! the parsing edge cases without driving a subprocess.
//!
//! **Why agent .md instead of `workflow.yaml`** — the PRD F128 spec
//! says "workflow.yaml `tools:` append" but Claude Code reads the
//! per-agent tool allow-list from the `.md` frontmatter `tools:` line
//! (see `docs/claude-code-tool-surface.md`). For the user-facing
//! claim "bot picks up the new tool on its next turn" to be true,
//! we must mutate the file Claude Code actually consults. The
//! workflow.yaml mention in the PRD is a doc oversight; the
//! implementation follows the runtime semantics.
//!
//! No backward-compat shims (CLAUDE.md §五 Pre-v1.0): operations
//! refuse if the persona file is missing rather than auto-creating
//! a stub.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

/// `<project_dir>/.claude/agents/<bot>.md`.
pub fn agent_md_path(project_dir: &Path, bot: &str) -> PathBuf {
    project_dir
        .join(".claude")
        .join("agents")
        .join(format!("{bot}.md"))
}

/// V0.6.1 F128 — replace the persona file's full contents.
///
/// The skill caller is responsible for assembling `new_persona_md`
/// (YAML frontmatter + body); this helper does not parse / merge
/// frontmatter. Returns the absolute path that was written.
///
/// Refuses with an error if `bot` contains characters outside
/// `[a-z0-9_-]`, if `new_persona_md` is empty, or if the existing
/// persona file is missing (operations on phantom bots are
/// surface-level user error — surface them loudly).
pub fn change_persona(project_dir: &Path, bot: &str, new_persona_md: &str) -> Result<PathBuf> {
    validate_bot_name(bot)?;
    if new_persona_md.trim().is_empty() {
        bail!("change_persona: new_persona_md is empty");
    }
    let path = agent_md_path(project_dir, bot);
    if !path.exists() {
        bail!(
            "change_persona: no persona file at {} — does bot `{bot}` exist?",
            path.display()
        );
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("agent .md path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    // Atomic write: tmp + rename keeps a concurrent Claude Code
    // open-on-read from racing the rewrite.
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, new_persona_md).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(path)
}

/// v0.8.6 W5b ResDisk — create-or-replace a role's persona file.
///
/// Unlike [`change_persona`] (which refuses if the file is missing —
/// the F128 "operate on an existing bot" semantics), this is the
/// idempotent **PUT** primitive the resource API uses: it writes
/// `<project_dir>/.claude/agents/<role>.md`, creating the file (and the
/// `.claude/agents/` parent) when absent. Same atomic tmp + rename write
/// as `change_persona` so a concurrent Claude Code open-on-read can't
/// race a half-written file. Refuses an empty body (a content-less role
/// file is surface-level user error) or a `role` outside `[a-z0-9_-]`.
/// Returns the absolute path written.
pub fn write_role(project_dir: &Path, role: &str, content: &str) -> Result<PathBuf> {
    validate_bot_name(role)?;
    if content.trim().is_empty() {
        bail!("write_role: content is empty");
    }
    let path = agent_md_path(project_dir, role);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("agent .md path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(path)
}

/// `<project_dir>/.claude/skills/<skill>/SKILL.md` — v0.8.9 Phase 2.
///
/// The Anthropic-native skill layout is a *directory* per skill (the skill
/// may carry sibling resources), with `SKILL.md` (frontmatter + body) as the
/// entry doc. ccteam-hub has zero skill plugins today, but the marketplace
/// installer (`ccteam_im::hub::install_plugin`) builds the path so a future
/// `type: "skill"` hub entry installs cleanly.
pub fn skill_md_path(project_dir: &Path, skill: &str) -> PathBuf {
    project_dir
        .join(".claude")
        .join("skills")
        .join(skill)
        .join("SKILL.md")
}

/// v0.8.9 Phase 2 — create-or-replace a skill's `SKILL.md`.
///
/// The skill sibling of [`write_role`]: writes
/// `<project_dir>/.claude/skills/<skill>/SKILL.md`, creating the
/// `.claude/skills/<skill>/` parent chain when absent, with the same atomic
/// tmp + rename so a concurrent reader can't race a half-written file. Same
/// validation posture as `write_role` — refuses an empty body or a `skill`
/// name outside `[a-z0-9_-]` (which also forbids path-traversal: `/`, `\`,
/// `..`, leading `.` are all rejected, so a hostile id can never escape
/// `.claude/skills/`). Returns the absolute path written.
pub fn write_skill(project_dir: &Path, skill: &str, content: &str) -> Result<PathBuf> {
    validate_bot_name(skill)?;
    if content.trim().is_empty() {
        bail!("write_skill: content is empty");
    }
    let path = skill_md_path(project_dir, skill);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("skill SKILL.md path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(path)
}

/// `<project_dir>/.claude/skills/<skill>/` — the directory a multi-file skill
/// installs into (the parent of its `SKILL.md`).
pub fn skill_dir_path(project_dir: &Path, skill: &str) -> PathBuf {
    project_dir.join(".claude").join("skills").join(skill)
}

/// Validate a skill-relative file path: a plain relative path of only
/// `Normal` components (rejects absolute paths, `..`, `.`, and Windows
/// prefixes) so a hostile manifest `relpath` can never escape the skill dir.
fn validate_skill_relpath(relpath: &str) -> Result<PathBuf> {
    if relpath.trim().is_empty() {
        bail!("skill file relpath must be non-empty");
    }
    let p = Path::new(relpath);
    if p.is_absolute() {
        bail!("skill file relpath must be relative: `{relpath}`");
    }
    for comp in p.components() {
        if !matches!(comp, std::path::Component::Normal(_)) {
            bail!("skill file relpath `{relpath}` must contain only normal segments (no `..`)");
        }
    }
    Ok(p.to_path_buf())
}

/// v0.8.12 — write one file of a multi-file skill to
/// `<project_dir>/.claude/skills/<skill>/<relpath>`, creating the parent
/// chain, with the same atomic tmp + rename as [`write_skill`]. `relpath` is
/// validated to stay inside the skill dir (no path traversal). The
/// marketplace installer uses this to land a skill that carries sibling
/// resources (`scripts/…`) beyond its `SKILL.md`. Returns the absolute path.
pub fn write_skill_file(
    project_dir: &Path,
    skill: &str,
    relpath: &str,
    content: &str,
) -> Result<PathBuf> {
    validate_bot_name(skill)?;
    let rel = validate_skill_relpath(relpath)?;
    let path = skill_dir_path(project_dir, skill).join(&rel);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("skill file path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("skill file path has no file name: {}", path.display()))?;
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(path)
}

/// Return value from [`add_tool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddToolResult {
    /// Absolute path of the agent .md that was rewritten.
    pub path: PathBuf,
    /// The tool descriptor as added (trimmed copy of the caller arg).
    pub added: String,
    /// The full rewritten `tools:` CSV (`Read, Grep, WebFetch`).
    pub new_tools_csv: String,
    /// `true` if the tool was already present (file untouched apart
    /// from idempotent rewrite). Callers can use this to suppress a
    /// redundant `tool_added` event if desired.
    pub already_present: bool,
}

/// V0.6.1 F128 — append `tool_descriptor` to the agent .md
/// frontmatter `tools:` CSV. Idempotent: re-adding an existing tool
/// is a no-op (returns [`AddToolResult::already_present`] = true and
/// rewrites the file with the existing list to preserve mtime
/// semantics callers may rely on).
///
/// If the frontmatter has no `tools:` line we insert one at the end
/// of the frontmatter block (just before the closing `---`).
pub fn add_tool(project_dir: &Path, bot: &str, tool_descriptor: &str) -> Result<AddToolResult> {
    validate_bot_name(bot)?;
    let tool = tool_descriptor.trim();
    if tool.is_empty() {
        bail!("add_tool: tool_descriptor is empty");
    }
    let path = agent_md_path(project_dir, bot);
    if !path.exists() {
        bail!(
            "add_tool: no persona file at {} — does bot `{bot}` exist?",
            path.display()
        );
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let (new_body, new_tools_csv, already_present) = append_tool_to_frontmatter(&body, tool)?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, &new_body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(AddToolResult {
        path,
        added: tool.to_string(),
        new_tools_csv,
        already_present,
    })
}

/// Build a `persona_changed` event for `progress.jsonl`.
pub fn build_persona_changed_event(bot: &str, persona_path: &Path, bytes_written: usize) -> Value {
    json!({
        "event": "persona_changed",
        "role": bot,
        "path": persona_path.display().to_string(),
        "bytes_written": bytes_written,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `tool_added` event for `progress.jsonl`.
pub fn build_tool_added_event(
    bot: &str,
    persona_path: &Path,
    tool: &str,
    new_tools_csv: &str,
    already_present: bool,
) -> Value {
    json!({
        "event": "tool_added",
        "role": bot,
        "path": persona_path.display().to_string(),
        "tool": tool,
        "tools": new_tools_csv,
        "already_present": already_present,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Mirror of the workflow.rs `validate_role_name` rule so this module
/// can stay independent of `WorkflowError`. Both surfaces must accept
/// the same character set or `chat:` schema validation diverges from
/// admin-edit validation.
fn validate_bot_name(bot: &str) -> Result<()> {
    if bot.is_empty() {
        bail!("bot name must be non-empty");
    }
    for ch in bot.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-';
        if !ok {
            bail!("bot name `{bot}`: character `{ch}` not allowed (only [a-z0-9_-])");
        }
    }
    Ok(())
}

/// Walk the YAML frontmatter (between the leading `---` and the next
/// `---`), find or insert a `tools:` line, append `new_tool` to its
/// CSV value, and return `(rewritten_body, rendered_tools_csv,
/// already_present)`.
///
/// **Scope**: this is a deliberately narrow parser — it only matches
/// the canonical line form `tools: A, B, C` produced by
/// `ccteam-creator` and the existing `.claude/agents/*.md` templates.
/// Block-scalar `tools:` (rare; not used by templates) is treated as
/// "no tools line found" and a new flat line is appended after the
/// frontmatter. Tests pin both branches.
fn append_tool_to_frontmatter(body: &str, new_tool: &str) -> Result<(String, String, bool)> {
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.first().map(|s| s.trim()) != Some("---") {
        bail!("agent .md missing YAML frontmatter (no leading `---`)");
    }
    let close_rel = lines[1..]
        .iter()
        .position(|s| s.trim() == "---")
        .ok_or_else(|| anyhow!("agent .md frontmatter has no closing `---`"))?;
    let close_idx = close_rel + 1;
    // `frontmatter` = lines[1..close_idx]; `tail` = lines[close_idx..]
    let frontmatter_slice: Vec<&str> = lines[1..close_idx].to_vec();
    let tail_slice: Vec<&str> = lines[close_idx..].to_vec();

    let mut rewritten_front: Vec<String> = Vec::with_capacity(frontmatter_slice.len() + 1);
    let mut tools_found = false;
    let mut new_tools_csv = String::new();
    let mut already_present = false;
    for line in &frontmatter_slice {
        if let Some(rest) = line.strip_prefix("tools:") {
            tools_found = true;
            // If the value is empty (`tools:` alone) it might be a
            // block scalar starting on the next line — bail to the
            // append-new-line fallthrough so we don't corrupt YAML.
            let trimmed_val = rest.trim();
            if trimmed_val.is_empty()
                || trimmed_val.starts_with('|')
                || trimmed_val.starts_with('>')
            {
                // Treat as not-found; preserve original line.
                rewritten_front.push((*line).to_string());
                tools_found = false;
                continue;
            }
            let existing: Vec<String> = trimmed_val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let already = existing.iter().any(|t| t == new_tool);
            already_present = already;
            let merged: Vec<String> = if already {
                existing.clone()
            } else {
                let mut v = existing.clone();
                v.push(new_tool.to_string());
                v
            };
            new_tools_csv = merged.join(", ");
            rewritten_front.push(format!("tools: {new_tools_csv}"));
        } else {
            rewritten_front.push((*line).to_string());
        }
    }
    if !tools_found {
        new_tools_csv = new_tool.to_string();
        rewritten_front.push(format!("tools: {new_tools_csv}"));
    }

    let mut out = String::new();
    out.push_str("---\n");
    for l in &rewritten_front {
        out.push_str(l);
        out.push('\n');
    }
    // tail_slice[0] is the closing `---`; emit verbatim + the rest.
    for (i, l) in tail_slice.iter().enumerate() {
        out.push_str(l);
        if i + 1 < tail_slice.len() {
            out.push('\n');
        }
    }
    Ok((out, new_tools_csv, already_present))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_persona(dir: &Path, bot: &str, body: &str) -> PathBuf {
        let path = agent_md_path(dir, bot);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn validate_bot_name_rejects_uppercase() {
        assert!(validate_bot_name("Helper").is_err());
    }

    #[test]
    fn validate_bot_name_rejects_empty() {
        assert!(validate_bot_name("").is_err());
    }

    #[test]
    fn validate_bot_name_accepts_kebab() {
        validate_bot_name("helper-bot").unwrap();
        validate_bot_name("helper_2").unwrap();
        validate_bot_name("a").unwrap();
    }

    #[test]
    fn change_persona_writes_full_body() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "alice", "---\nname: alice\n---\nbody\n");
        let new = "---\nname: alice\ntools: Read\n---\nrevised body\n";
        let written = change_persona(tmp.path(), "alice", new).unwrap();
        assert_eq!(written, agent_md_path(tmp.path(), "alice"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), new);
    }

    #[test]
    fn change_persona_rejects_missing_bot() {
        let tmp = TempDir::new().unwrap();
        let err = change_persona(tmp.path(), "ghost", "---\nname: x\n---\n")
            .err()
            .unwrap();
        assert!(err.to_string().contains("no persona file"));
    }

    #[test]
    fn change_persona_rejects_empty_body() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "alice", "---\nname: alice\n---\n");
        assert!(change_persona(tmp.path(), "alice", "   \n").is_err());
    }

    #[test]
    fn write_role_creates_missing_file() {
        let tmp = TempDir::new().unwrap();
        // No seed: write_role must create the file (unlike change_persona).
        let content = "---\nname: reviewer\nmodel: sonnet\n---\nbody\n";
        let written = write_role(tmp.path(), "reviewer", content).unwrap();
        assert_eq!(written, agent_md_path(tmp.path(), "reviewer"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), content);
    }

    #[test]
    fn write_role_replaces_existing_file() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "reviewer", "---\nname: reviewer\n---\nold\n");
        let new = "---\nname: reviewer\n---\nnew body\n";
        write_role(tmp.path(), "reviewer", new).unwrap();
        let got = std::fs::read_to_string(agent_md_path(tmp.path(), "reviewer")).unwrap();
        assert_eq!(got, new);
    }

    #[test]
    fn write_role_rejects_empty_and_bad_name() {
        let tmp = TempDir::new().unwrap();
        assert!(write_role(tmp.path(), "reviewer", "   \n").is_err());
        assert!(write_role(tmp.path(), "Bad Name", "---\nx\n---\n").is_err());
    }

    #[test]
    fn write_skill_creates_nested_skill_md() {
        let tmp = TempDir::new().unwrap();
        let content = "---\nname: do-thing\ndescription: does a thing\n---\nbody\n";
        let written = write_skill(tmp.path(), "do-thing", content).unwrap();
        assert_eq!(written, skill_md_path(tmp.path(), "do-thing"));
        // Lands under .claude/skills/<id>/SKILL.md, verbatim.
        assert!(written.ends_with(".claude/skills/do-thing/SKILL.md"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), content);
    }

    #[test]
    fn write_skill_replaces_existing() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "do-thing", "---\nname: do-thing\n---\nold\n").unwrap();
        let new = "---\nname: do-thing\n---\nnew body\n";
        write_skill(tmp.path(), "do-thing", new).unwrap();
        assert_eq!(
            std::fs::read_to_string(skill_md_path(tmp.path(), "do-thing")).unwrap(),
            new
        );
    }

    #[test]
    fn write_skill_rejects_empty_and_bad_name() {
        let tmp = TempDir::new().unwrap();
        assert!(write_skill(tmp.path(), "do-thing", "   \n").is_err());
        // Path-traversal / charset escapes are rejected by validate_bot_name.
        assert!(write_skill(tmp.path(), "../escape", "---\nx\n---\n").is_err());
        assert!(write_skill(tmp.path(), "Bad Name", "---\nx\n---\n").is_err());
    }

    #[test]
    fn add_tool_appends_to_existing_csv() {
        let tmp = TempDir::new().unwrap();
        seed_persona(
            tmp.path(),
            "alice",
            "---\nname: alice\ntools: Read, Grep\n---\nbody\n",
        );
        let res = add_tool(tmp.path(), "alice", "WebFetch").unwrap();
        assert!(!res.already_present);
        assert_eq!(res.new_tools_csv, "Read, Grep, WebFetch");
        let body = std::fs::read_to_string(&res.path).unwrap();
        assert!(body.contains("tools: Read, Grep, WebFetch"));
        assert!(body.contains("body"));
    }

    #[test]
    fn add_tool_idempotent_when_already_present() {
        let tmp = TempDir::new().unwrap();
        seed_persona(
            tmp.path(),
            "alice",
            "---\nname: alice\ntools: Read, WebFetch\n---\nbody\n",
        );
        let res = add_tool(tmp.path(), "alice", "WebFetch").unwrap();
        assert!(res.already_present);
        assert_eq!(res.new_tools_csv, "Read, WebFetch");
    }

    #[test]
    fn add_tool_inserts_line_when_absent() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "alice", "---\nname: alice\n---\nbody\n");
        let res = add_tool(tmp.path(), "alice", "Bash").unwrap();
        assert!(!res.already_present);
        assert_eq!(res.new_tools_csv, "Bash");
        let body = std::fs::read_to_string(&res.path).unwrap();
        assert!(body.contains("tools: Bash"));
        // Frontmatter still closes properly.
        let after_close = body.split("---").nth(2).unwrap();
        assert!(after_close.contains("body"));
    }

    #[test]
    fn add_tool_rejects_missing_persona() {
        let tmp = TempDir::new().unwrap();
        assert!(add_tool(tmp.path(), "ghost", "Read").is_err());
    }

    #[test]
    fn add_tool_rejects_empty_descriptor() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "alice", "---\nname: alice\n---\n");
        assert!(add_tool(tmp.path(), "alice", "   ").is_err());
    }

    #[test]
    fn build_persona_changed_event_shape() {
        let p = PathBuf::from("/x/.claude/agents/alice.md");
        let ev = build_persona_changed_event("alice", &p, 42);
        assert_eq!(ev["event"], "persona_changed");
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["bytes_written"], 42);
        assert!(ev["ts"].is_string());
    }

    #[test]
    fn build_tool_added_event_shape() {
        let p = PathBuf::from("/x/.claude/agents/alice.md");
        let ev = build_tool_added_event("alice", &p, "WebFetch", "Read, WebFetch", false);
        assert_eq!(ev["event"], "tool_added");
        assert_eq!(ev["tool"], "WebFetch");
        assert_eq!(ev["tools"], "Read, WebFetch");
        assert_eq!(ev["already_present"], false);
    }

    #[test]
    fn add_tool_rejects_missing_frontmatter() {
        let tmp = TempDir::new().unwrap();
        seed_persona(tmp.path(), "alice", "no frontmatter here\n");
        let err = add_tool(tmp.path(), "alice", "Read").err().unwrap();
        assert!(err.to_string().contains("frontmatter"));
    }
}
