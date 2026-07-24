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
use std::path::{Path, PathBuf};

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

/// One entry in the project skill list (`GET .../skills`) — a leaf directory
/// under the selected project-local skill face. `description` is pulled from
/// the SKILL.md frontmatter when present ("" otherwise) so the web composer's
/// skill picker can show what each skill does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    /// Directory name (`deep-research` for `.claude/skills/deep-research/`).
    pub skill: String,
    /// Frontmatter `description`, or "" when absent.
    pub description: String,
}

/// One entry in the global user-level skill library. Unlike project-local
/// [`SkillSummary`], `id` may be a nested POSIX path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySkillSummary {
    /// Directory path relative to the library root, using `/` separators.
    pub id: String,
    /// Frontmatter `description`, or "" when absent.
    pub description: String,
    /// Absolute path to the skill's `SKILL.md` entrypoint.
    pub path: PathBuf,
}

/// List the project's local skills, sorted by leaf id.
///
/// The neutral `<project>/.agents/skills` face wins whenever it exists and is
/// followed when it is a directory symlink. Only when that face is absent do
/// we inspect the legacy `<project>/.claude/skills`, and a symlink is never
/// accepted as the legacy entity. Missing faces yield an empty vec; an
/// unreadable SKILL.md is skipped, never fatal (mirrors [`list_roles`]).
pub fn list_skills(project_dir: &Path) -> Result<Vec<SkillSummary>> {
    let neutral = project_dir.join(".agents").join("skills");
    let dir = if neutral.exists() {
        neutral
    } else {
        let legacy = project_dir.join(".claude").join("skills");
        match fs::symlink_metadata(&legacy) {
            Ok(metadata) if metadata.file_type().is_dir() => legacy,
            _ => return Ok(Vec::new()),
        }
    };
    let rd = fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
    let mut out: Vec<SkillSummary> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(skill) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let md = path.join("SKILL.md");
        let text = match fs::read_to_string(&md) {
            Ok(s) => s,
            Err(_) => continue, // no SKILL.md → not a skill dir
        };
        let (frontmatter, _body) = split_frontmatter(&text);
        out.push(SkillSummary {
            skill: skill.to_string(),
            description: scalar_field(&frontmatter, "description"),
        });
    }
    out.sort_by(|a, b| a.skill.cmp(&b.skill));
    Ok(out)
}

/// Recursively list every valid `<root>/<id>/SKILL.md` in a global skill
/// library. Hidden or invalid path components are not traversed, unreadable
/// entries are skipped, and results are sorted deterministically by id.
pub fn list_library_skills(root: &Path) -> Vec<LibrarySkillSummary> {
    let Ok(root) = fs::canonicalize(root) else {
        return Vec::new();
    };
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    scan_library_dir(&root, &root, &mut out);
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Resolve `CCTEAM_HOME` and list its canonical [`crate::CcteamPaths::skills_dir`].
pub fn list_default_library_skills() -> Result<Vec<LibrarySkillSummary>> {
    let paths = crate::CcteamPaths::from_env()?;
    Ok(list_library_skills(&paths.skills_dir()))
}

fn scan_library_dir(root: &Path, dir: &Path, out: &mut Vec<LibrarySkillSummary>) {
    if dir != root {
        let md = dir.join("SKILL.md");
        if md.is_file() {
            if let Some(id) = library_id(root, dir) {
                match fs::read_to_string(&md) {
                    Ok(text) => {
                        let (frontmatter, _body) = split_frontmatter(&text);
                        out.push(LibrarySkillSummary {
                            id,
                            description: scalar_field(&frontmatter, "description"),
                            path: md,
                        });
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, path = %md.display(), "library SKILL.md read failed; skipping");
                    }
                }
            }
        }
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = %err, path = %dir.display(), "skill library directory read failed; skipping");
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(segment) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if segment.starts_with('.') || crate::validate_skill_library_id(&segment).is_err() {
            continue;
        }
        scan_library_dir(root, &entry.path(), out);
    }
}

fn library_id(root: &Path, dir: &Path) -> Option<String> {
    let relative = dir.strip_prefix(root).ok()?;
    let mut segments = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            return None;
        };
        let segment = segment.to_str()?;
        if segment.starts_with('.') {
            return None;
        }
        segments.push(segment);
    }
    let id = segments.join("/");
    crate::validate_skill_library_id(&id).ok()?;
    Some(id)
}

/// Mirror of `admin_actions::validate_bot_name` (the write/PUT path
/// validator) — kept independent here so the read-side resource API
/// doesn't reach across modules for a few lines, and so a malicious
/// `{role}` path param can never escape `.claude/agents/`. A `role`
/// containing `/`, `\`, `..`, a leading `.`, or that is empty is
/// rejected; only `[a-z0-9_-]` is allowed, which subsumes all of those
/// (axum percent-decodes the path param before it reaches us, so a
/// `..%2f..%2f` traversal arrives as literal `../../` and is caught by
/// the `/` / `.` rejection). Read and write **must** agree on the
/// accepted character set.
fn validate_role_name(role: &str) -> Result<()> {
    if role.is_empty() {
        anyhow::bail!("role name must be non-empty");
    }
    for ch in role.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-';
        if !ok {
            anyhow::bail!("role name `{role}`: character `{ch}` not allowed (only [a-z0-9_-])");
        }
    }
    Ok(())
}

/// Read a single role file. Returns `Ok(None)` when the file does not
/// exist (the caller maps that to 404); `Err` only on a genuine read
/// failure (which now includes an invalid / path-traversing `role` —
/// the web handler maps that to 400, not 404). Frontmatter parse
/// failures fold to an empty object rather than erroring, mirroring
/// [`list_roles`].
pub fn read_role(project_dir: &Path, role: &str) -> Result<Option<RoleDetail>> {
    validate_role_name(role)?;
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
    fn read_role_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        // Plant a sibling .md *outside* the project's agents/ dir that a
        // traversal would reach if validation were missing.
        let secret = tmp.path().join("secret.md");
        fs::write(&secret, "---\nmodel: opus\n---\ntop secret\n").unwrap();
        seed(tmp.path(), "cto", "---\nmodel: opus\n---\nlead body\n");

        // axum percent-decodes the path param, so these are the literal
        // values a `..%2f..%2fsecret` request would deliver to read_role.
        for evil in [
            "../secret",
            "../../etc/passwd",
            "..\\..\\windows",
            "/etc/passwd",
            ".hidden",
            "",
            "a/b",
        ] {
            assert!(
                read_role(tmp.path(), evil).is_err(),
                "expected traversal/invalid role `{evil}` to be rejected"
            );
        }
        // A normal role still reads fine.
        let detail = read_role(tmp.path(), "cto").unwrap().unwrap();
        assert_eq!(detail.role, "cto");
        assert_eq!(detail.frontmatter.get("model").unwrap(), "opus");
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

    #[test]
    fn list_skills_reads_skill_md_descriptions_sorted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(skills.join("zeta")).unwrap();
        std::fs::write(
            skills.join("zeta").join("SKILL.md"),
            "---\ndescription: z skill\n---\nbody\n",
        )
        .unwrap();
        std::fs::create_dir_all(skills.join("alpha")).unwrap();
        std::fs::write(skills.join("alpha").join("SKILL.md"), "no frontmatter\n").unwrap();
        // A dir WITHOUT SKILL.md is not a skill; a stray file is ignored.
        std::fs::create_dir_all(skills.join("not-a-skill")).unwrap();
        std::fs::write(skills.join("README.md"), "stray\n").unwrap();

        let out = list_skills(tmp.path()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].skill, "alpha");
        assert_eq!(out[0].description, "");
        assert_eq!(out[1].skill, "zeta");
        assert_eq!(out[1].description, "z skill");
    }

    #[test]
    fn list_skills_prefers_agents_face_over_legacy_entity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let neutral = tmp.path().join(".agents/skills/neutral");
        let legacy = tmp.path().join(".claude/skills/legacy");
        std::fs::create_dir_all(&neutral).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            neutral.join("SKILL.md"),
            "---\ndescription: neutral face\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            legacy.join("SKILL.md"),
            "---\ndescription: legacy face\n---\nbody\n",
        )
        .unwrap();

        let out = list_skills(tmp.path()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].skill, "neutral");
        assert_eq!(out[0].description, "neutral face");
    }

    #[cfg(unix)]
    #[test]
    fn list_skills_does_not_treat_symlinked_claude_face_as_legacy_entity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("legacy-target/linked");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "---\ndescription: linked\n---\n").unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::os::unix::fs::symlink(
            tmp.path().join("legacy-target"),
            tmp.path().join(".claude/skills"),
        )
        .unwrap();

        assert!(list_skills(tmp.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn list_skills_follows_agents_face_directory_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("skill-entities/linked");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "---\ndescription: linked\n---\n").unwrap();
        std::fs::create_dir_all(tmp.path().join(".agents")).unwrap();
        std::os::unix::fs::symlink(
            tmp.path().join("skill-entities"),
            tmp.path().join(".agents/skills"),
        )
        .unwrap();

        let out = list_skills(tmp.path()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].skill, "linked");
    }

    #[test]
    fn list_library_skills_is_recursive_hidden_safe_and_sorted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("skills");
        for dir in [
            "zeta",
            "baoyu-skills/baoyu-comic",
            ".git/hidden",
            "owner/.hidden",
            "Upper/hidden",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("zeta/SKILL.md"),
            "---\ndescription: z skill\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            root.join("baoyu-skills/baoyu-comic/SKILL.md"),
            "---\ndescription: nested skill\n---\nbody\n",
        )
        .unwrap();
        for hidden in [
            ".git/hidden/SKILL.md",
            "owner/.hidden/SKILL.md",
            "Upper/hidden/SKILL.md",
        ] {
            std::fs::write(root.join(hidden), "---\ndescription: hidden\n---\n").unwrap();
        }
        std::fs::write(root.join(".sources.json"), "{}\n").unwrap();

        let out = list_library_skills(&root);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "baoyu-skills/baoyu-comic");
        assert_eq!(out[0].description, "nested skill");
        assert_eq!(out[0].path, root.join("baoyu-skills/baoyu-comic/SKILL.md"));
        assert!(out[0].path.is_absolute());
        assert_eq!(out[1].id, "zeta");
        assert_eq!(out[1].description, "z skill");
    }

    #[test]
    fn list_skills_empty_without_skills_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(list_skills(tmp.path()).unwrap().is_empty());
    }
}
