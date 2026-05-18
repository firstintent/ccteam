//! V0.6.0 F115 — agent handoff doc 机制.
//!
//! 每 stage / wave / fix-loop iteration done → orchestrator emit
//! `stage_done` (or escalation) event → trigger 当前 agent 在自己 turn
//! 内写一份 10-30 行 markdown 到
//! `<project>/.ccteam/handoffs/<workflow-slug>/stage-<N>-<role>.md`.
//!
//! 后续 stage / fix-loop iteration spawn prompt 通过
//! [`crate::spawn_brief::render_spawn_brief`] 的
//! `{{include_prev_handoffs}}` token 自动注入前序 handoff,
//! 避免 context compact 后丢决策。
//!
//! 借 OMC `.omc/handoffs/<stage>.md` pattern,见
//! `references/oh-my-claudecode/skills/team/SKILL.md` §"Stage Handoff
//! Convention"。
//!
//! Atomic write semantics: handoff doc 写入 `.tmp.<role>` sibling 后
//! `std::fs::rename` 原子 swap,与 `progress::append_event` /
//! `pending_inject::save` 等其它持久化路径一致。
//!
//! Path layout:
//! ```text
//! <project>/
//!   .ccteam/
//!     handoffs/
//!       <workflow-slug>/
//!         stage-1-explorer.md
//!         stage-1-fixer.md     # 多 role 同 stage 各一文件
//!         stage-2-explorer.md
//!         stage-3-explorer.md
//! ```

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Markdown template for a handoff doc.
///
/// Placeholders are simple `{{key}}` tokens — callers can `.replace(...)`
/// them before writing, or feed the rendered string straight into
/// [`write_handoff`]. The template intentionally mirrors OMC's
/// Decided / Rejected / Risks / Files / Remaining structure for cross-
/// system reading. Length budget: **10–30 行**.
pub const HANDOFF_TEMPLATE: &str = r#"<!-- ccteam handoff -->
# Stage {{stage_num}}: {{stage_name}} ({{role}})

**Decided**:
- TODO

**Rejected**:
- TODO

**Risks**:
- TODO

**Files changed**:
- `path/to/file.rs` — what + why

**Remaining**:
- TODO
"#;

/// Sub-directory under `<project>/.ccteam/` that holds handoff docs.
pub const HANDOFFS_DIRNAME: &str = "handoffs";

/// Default number of prior handoffs to splice into a spawn brief
/// (`{{include_prev_handoffs}}` token). Tuned to keep prompt size
/// reasonable; advanced workflows can override via direct
/// [`read_concat`] call with a different `last_n`.
pub const DEFAULT_INCLUDE_LAST_N: usize = 3;

/// Sanitize a slug or role for use as a path component.
///
/// Rules:
/// - Allowed chars: `[A-Za-z0-9_-]`. Everything else (including `.`,
///   `/`, `\\`, `:`, spaces) maps to `_`. Dropping `.` from the allow
///   list nukes path-traversal attempts (`..`, `../foo`) without
///   special-casing them.
/// - Empty / all-stripped input → `"unknown"` so the path is always
///   valid.
fn sanitize_component(raw: &str) -> String {
    if raw.is_empty() {
        return "unknown".to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// Compute the absolute path of a handoff file.
///
/// `<project_dir>/.ccteam/handoffs/<workflow-slug>/stage-<N>-<role>.md`
///
/// Slug + role are sanitized via [`sanitize_component`] — callers can
/// pass raw user input safely.
pub fn handoff_path(
    project_dir: &Path,
    workflow_slug: &str,
    stage_num: u32,
    role: &str,
) -> PathBuf {
    let slug = sanitize_component(workflow_slug);
    let role = sanitize_component(role);
    project_dir
        .join(".ccteam")
        .join(HANDOFFS_DIRNAME)
        .join(slug)
        .join(format!("stage-{}-{}.md", stage_num, role))
}

/// Directory holding all handoff docs for a given workflow slug.
pub fn handoffs_dir(project_dir: &Path, workflow_slug: &str) -> PathBuf {
    let slug = sanitize_component(workflow_slug);
    project_dir
        .join(".ccteam")
        .join(HANDOFFS_DIRNAME)
        .join(slug)
}

/// Parse a handoff filename → `(stage_num, role)` tuple. Returns `None`
/// when the filename does not match `stage-<N>-<role>.md`.
fn parse_handoff_filename(name: &str) -> Option<(u32, String)> {
    let stem = name.strip_suffix(".md")?;
    let rest = stem.strip_prefix("stage-")?;
    let dash = rest.find('-')?;
    let (num_str, role_with_dash) = rest.split_at(dash);
    let role = &role_with_dash[1..]; // drop leading '-'
    let stage_num: u32 = num_str.parse().ok()?;
    if role.is_empty() {
        return None;
    }
    Some((stage_num, role.to_string()))
}

/// List every handoff doc for a workflow, sorted ascending by
/// `(stage_num, role)`.
///
/// Returns `Ok(vec![])` when the dir does not yet exist — the first
/// agent in a workflow has nothing to read, and that's not an error.
pub fn list_handoffs(project_dir: &Path, workflow_slug: &str) -> Result<Vec<PathBuf>> {
    let dir = handoffs_dir(project_dir, workflow_slug);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(u32, String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some((stage_num, role)) = parse_handoff_filename(name) {
            entries.push((stage_num, role, path));
        }
    }
    entries.sort_by(|a, b| match a.0.cmp(&b.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        ord => ord,
    });
    Ok(entries.into_iter().map(|(_, _, p)| p).collect())
}

/// Read the **most recent** `last_n` handoff docs and concatenate them
/// in chronological (ascending stage) order.
///
/// - Returns `Ok("")` when there are zero handoffs (graceful: first
///   agent in workflow → empty token replacement).
/// - When `last_n == 0` → returns `Ok("")`.
/// - When fewer than `last_n` handoffs exist → returns all of them.
///
/// Each handoff is separated by a blank line, prefixed with a small
/// marker comment for downstream readability:
///
/// ```text
/// <!-- ccteam handoff: stage-1-explorer.md -->
/// ...body...
///
/// <!-- ccteam handoff: stage-2-fixer.md -->
/// ...body...
/// ```
pub fn read_concat(
    project_dir: &Path,
    workflow_slug: &str,
    last_n: usize,
) -> Result<String> {
    if last_n == 0 {
        return Ok(String::new());
    }
    let mut all = list_handoffs(project_dir, workflow_slug)?;
    if all.is_empty() {
        return Ok(String::new());
    }
    // Take the last_n highest-stage entries, but keep ascending order
    // for chronological flow.
    if all.len() > last_n {
        let drop = all.len() - last_n;
        all.drain(0..drop);
    }
    let mut out = String::new();
    for (idx, path) in all.iter().enumerate() {
        if idx > 0 {
            out.push_str("\n\n");
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>");
        out.push_str(&format!("<!-- ccteam handoff: {} -->\n", name));
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read handoff {}", path.display()))?;
        out.push_str(&body);
    }
    Ok(out)
}

/// Args for [`write_handoff`].
#[derive(Debug, Clone)]
pub struct WriteHandoffOptions {
    pub project_dir: PathBuf,
    pub workflow_slug: String,
    pub stage_num: u32,
    pub role: String,
    /// Full markdown body. Caller is responsible for templating
    /// (typically by `.replace()`-ing on [`HANDOFF_TEMPLATE`]).
    pub content: String,
}

/// Atomically write a handoff doc and return its final path.
///
/// Steps:
/// 1. Ensure parent dir exists (`mkdir -p`).
/// 2. Write `<final>.tmp` then `std::fs::rename` → atomic swap.
///
/// Overwrites existing handoff at the same `(stage, role)` — re-runs of
/// the same stage are the expected case (fix-loop iterations).
pub fn write_handoff(opts: &WriteHandoffOptions) -> Result<PathBuf> {
    let final_path = handoff_path(
        &opts.project_dir,
        &opts.workflow_slug,
        opts.stage_num,
        &opts.role,
    );
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let mut tmp = final_path.clone();
    // Sibling .tmp avoids cross-device rename failures + lets concurrent
    // writers for different roles in the same stage not stomp each other.
    let tmp_name = format!(
        "{}.tmp.{}",
        final_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("handoff"),
        std::process::id()
    );
    tmp.set_file_name(tmp_name);
    std::fs::write(&tmp, &opts.content)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &final_path)
        .with_context(|| format!("rename {} → {}", tmp.display(), final_path.display()))?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_filename() {
        assert_eq!(
            parse_handoff_filename("stage-1-explorer.md"),
            Some((1, "explorer".to_string()))
        );
        assert_eq!(
            parse_handoff_filename("stage-42-fixer.md"),
            Some((42, "fixer".to_string()))
        );
        // Role with dash inside is preserved (split on FIRST dash after num).
        assert_eq!(
            parse_handoff_filename("stage-3-team-lead.md"),
            Some((3, "team-lead".to_string()))
        );
    }

    #[test]
    fn rejects_malformed_filename() {
        assert_eq!(parse_handoff_filename("README.md"), None);
        assert_eq!(parse_handoff_filename("stage-x-foo.md"), None);
        assert_eq!(parse_handoff_filename("stage-1-.md"), None);
        assert_eq!(parse_handoff_filename("stage-1.md"), None);
        assert_eq!(parse_handoff_filename("stage-1-explorer.txt"), None);
    }

    #[test]
    fn sanitize_blocks_path_traversal() {
        assert_eq!(sanitize_component("../../etc"), "______etc");
        assert_eq!(sanitize_component("foo/bar"), "foo_bar");
        assert_eq!(sanitize_component(".."), "__");
        assert_eq!(sanitize_component(""), "unknown");
        // `.` is not on the allow-list (path-traversal hardening).
        assert_eq!(sanitize_component("good-slug.v2"), "good-slug_v2");
        assert_eq!(sanitize_component("kebab-case-ok_2"), "kebab-case-ok_2");
    }
}
