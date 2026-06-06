//! v0.8.6 W5b ResDisk — read-side reader for project-scoped agent roles.
//!
//! A "role" is a Claude Code subagent definition file living at
//! `<project_dir>/.claude/agents/<role>.md`. The write side already
//! exists in [`crate::admin_actions`] (`change_persona` / `add_tool` /
//! `agent_md_path`); this module is the read-side counterpart the
//! network resource API (`GET /api/v1/projects/{slug}/roles[/{role}]`)
//! composes.
//!
//! The file shape is the standard Anthropic subagent doc — a YAML
//! frontmatter fence followed by a markdown body:
//!
//! ```markdown
//! ---
//! name: reviewer
//! description: Reviews diffs for correctness
//! model: sonnet
//! tools: Read, Grep
//! ---
//! You are a careful reviewer...
//! ```
//!
//! `description` / `model` are surfaced as convenience scalars in the
//! list view; the single-role view returns the parsed frontmatter as a
//! free-form JSON object plus the raw body so callers don't depend on a
//! pinned schema (Anthropic's frontmatter keys drift).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// `<project_dir>/.claude/agents/` — the directory holding `<role>.md`
/// subagent definition files.
pub fn agents_dir(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".claude").join("agents")
}

/// One entry in the role list (`GET .../roles`). `description` / `model`
/// are pulled from the frontmatter when present; both fall back to an
/// empty string so the wire shape is stable for files that omit them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSummary {
    /// File stem (`reviewer` for `reviewer.md`) — the `{role}` path param.
    pub role: String,
    /// Frontmatter `description`, or "" when absent.
    pub description: String,
    /// Frontmatter `model`, or "" when absent.
    pub model: String,
}

/// Full single-role payload (`GET .../roles/{role}`). `frontmatter` is a
/// free-form JSON object (empty object when the file has no frontmatter
/// fence); `body` is the markdown after the closing fence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleDetail {
    pub role: String,
    pub frontmatter: serde_json::Value,
    pub body: String,
}

/// List every `*.md` under `<project_dir>/.claude/agents/`, parse each
/// file's frontmatter, and return the summaries sorted by role name.
///
/// - A missing `agents/` directory is **not** an error — it yields an
///   empty list (a freshly-bootstrapped project may have only `cto.md`,
///   and an adopted repo may have none).
/// - Non-`.md` entries are skipped.
/// - A file that fails to read or whose frontmatter fails to parse is
///   skipped with a `tracing::warn` (one malformed file must not 500 the
///   whole list).
pub fn list_roles(project_dir: &Path) -> Result<Vec<RoleSummary>> {
    let dir = agents_dir(project_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let rd = fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
    let mut out: Vec<RoleSummary> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(role) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let text = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "role .md read failed; skipping");
                continue;
            }
        };
        let (frontmatter, _body) = split_frontmatter(&text);
        out.push(RoleSummary {
            role: role.to_string(),
            description: scalar_field(&frontmatter, "description"),
            model: scalar_field(&frontmatter, "model"),
        });
    }
    out.sort_by(|a, b| a.role.cmp(&b.role));
    Ok(out)
}

/// Read a single role file. Returns `Ok(None)` when the file does not
/// exist (the caller maps that to 404); `Err` only on a genuine read
/// failure. Frontmatter parse failures fold to an empty object rather
/// than erroring, mirroring [`list_roles`].
pub fn read_role(project_dir: &Path, role: &str) -> Result<Option<RoleDetail>> {
    let path = agents_dir(project_dir).join(format!("{role}.md"));
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("read role {}", path.display()))?;
    let (frontmatter, body) = split_frontmatter(&text);
    Ok(Some(RoleDetail {
        role: role.to_string(),
        frontmatter,
        body,
    }))
}

/// Split a subagent `.md` into `(frontmatter_json, body)`. No fence →
/// empty object + the full text as body. Accepts a leading BOM and
/// CRLF line endings (Windows-authored files). A frontmatter block that
/// fails to parse as YAML folds to an empty object (the body still
/// returns intact) so a typo in one file never poisons the list.
///
/// This mirrors `ccteam-web::teams::subagent_resolver::parse_definition`
/// — kept independent here so the read-side resource API doesn't reach
/// across crates for a 30-line parser.
fn split_frontmatter(text: &str) -> (serde_json::Value, String) {
    let empty = || serde_json::Value::Object(serde_json::Map::new());
    let normalised = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    // Opening fence must be `---` at the very start (optionally `---\n`).
    let after_open = if let Some(rest) = normalised.strip_prefix("---\n") {
        rest
    } else if let Some(rest) = normalised.strip_prefix("---") {
        // `---` with no trailing newline (e.g. EOF or `--- name: x`): only
        // treat it as a fence when the very next char is a newline.
        match rest.strip_prefix('\n') {
            Some(r) => r,
            None => return (empty(), normalised),
        }
    } else {
        return (empty(), normalised);
    };
    let Some(close_idx) = find_closing_fence(after_open) else {
        return (empty(), normalised);
    };
    let frontmatter_text = &after_open[..close_idx];
    let mut body = &after_open[close_idx + 3..]; // skip the closing `---`
    if let Some(rest) = body.strip_prefix('\n') {
        body = rest;
    }
    let frontmatter = match serde_yaml::from_str::<serde_json::Value>(frontmatter_text) {
        Ok(v) if v.is_null() => empty(),
        Ok(v) => v,
        Err(_) => empty(),
    };
    (frontmatter, body.to_string())
}

/// Find the closing `---` fence (a line that is exactly `---`), scanning
/// line-by-line so a `---` inside a YAML block scalar isn't mistaken for
/// the fence. Returns the byte offset of the fence within `s`.
fn find_closing_fence(s: &str) -> Option<usize> {
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

/// Pull a string scalar from the frontmatter, returning "" when the key
/// is absent or not a string.
fn scalar_field(fm: &serde_json::Value, key: &str) -> String {
    fm.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed(dir: &Path, role: &str, body: &str) {
        let agents = agents_dir(dir);
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join(format!("{role}.md")), body).unwrap();
    }

    #[test]
    fn list_roles_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(list_roles(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn list_roles_parses_and_sorts() {
        let tmp = TempDir::new().unwrap();
        seed(
            tmp.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: Reviews diffs\nmodel: sonnet\n---\nbody here\n",
        );
        seed(
            tmp.path(),
            "cto",
            "---\ndescription: Lead\nmodel: opus\n---\nlead body\n",
        );
        let roles = list_roles(tmp.path()).unwrap();
        assert_eq!(roles.len(), 2);
        // Sorted by role name: cto, reviewer.
        assert_eq!(roles[0].role, "cto");
        assert_eq!(roles[0].model, "opus");
        assert_eq!(roles[1].role, "reviewer");
        assert_eq!(roles[1].description, "Reviews diffs");
    }

    #[test]
    fn list_roles_skips_non_md_and_missing_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let agents = agents_dir(tmp.path());
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("notes.txt"), "ignore me").unwrap();
        fs::write(agents.join("plain.md"), "no frontmatter at all").unwrap();
        let roles = list_roles(tmp.path()).unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].role, "plain");
        // Missing frontmatter → empty scalars, no error.
        assert_eq!(roles[0].description, "");
        assert_eq!(roles[0].model, "");
    }

    #[test]
    fn list_roles_tolerates_malformed_yaml() {
        let tmp = TempDir::new().unwrap();
        seed(
            tmp.path(),
            "broken",
            "---\ndescription: [unterminated\n---\nbody\n",
        );
        let roles = list_roles(tmp.path()).unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].role, "broken");
        assert_eq!(roles[0].description, "");
    }

    #[test]
    fn read_role_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(read_role(tmp.path(), "ghost").unwrap().is_none());
    }

    #[test]
    fn read_role_returns_frontmatter_and_body() {
        let tmp = TempDir::new().unwrap();
        seed(
            tmp.path(),
            "reviewer",
            "---\nname: reviewer\nmodel: sonnet\n---\nYou are a reviewer.\nLine two.\n",
        );
        let detail = read_role(tmp.path(), "reviewer").unwrap().unwrap();
        assert_eq!(detail.role, "reviewer");
        assert_eq!(detail.frontmatter.get("model").unwrap(), "sonnet");
        assert_eq!(detail.body, "You are a reviewer.\nLine two.\n");
    }

    #[test]
    fn read_role_no_frontmatter_yields_empty_object_and_full_body() {
        let tmp = TempDir::new().unwrap();
        seed(tmp.path(), "plain", "just a body, no fence\n");
        let detail = read_role(tmp.path(), "plain").unwrap().unwrap();
        assert!(detail.frontmatter.as_object().unwrap().is_empty());
        assert_eq!(detail.body, "just a body, no fence\n");
    }

    #[test]
    fn split_frontmatter_handles_bom_and_crlf() {
        let text = "\u{feff}---\r\nmodel: opus\r\n---\r\nbody\r\n";
        let (fm, body) = split_frontmatter(text);
        assert_eq!(fm.get("model").unwrap(), "opus");
        assert_eq!(body, "body\n");
    }
}
