//! Project bootstrap helpers used by `ccteam new` (and reusable by the
//! M3+ inbox triage path). Pure: no tmux side effects, just file
//! creation under `~/projects/<slug>/`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::templates::write_project_settings;

/// Slugify a free-text project request: keep `[a-z0-9]`, collapse other
/// runs to `-`, trim, lower-case, and cap at 40 chars. Empty result is
/// replaced by `project`.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    let truncated: String = trimmed.chars().take(40).collect();
    if truncated.is_empty() {
        "project".into()
    } else {
        truncated
    }
}

/// 4-char hex suffix derived from the current sub-second wallclock.
/// Good enough for collision avoidance under interactive use; the
/// caller can retry on collision.
pub fn random_suffix() -> String {
    let nanos = Utc::now().timestamp_subsec_nanos();
    format!("{:04x}", nanos & 0xFFFF)
}

/// Pick an unused slug under `paths.projects_root`. Tries the bare
/// slugified base first, then `<base>-<suffix>` with up to 16 retries.
pub fn pick_unused_slug(paths: &CcteamPaths, base: &str) -> Result<String> {
    let base = slugify(base);
    if !paths.project_dir(&base).exists() {
        return Ok(base);
    }
    for _ in 0..16 {
        let candidate = format!("{base}-{}", random_suffix());
        if !paths.project_dir(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not pick an unused slug after 16 attempts (base: {base})",
    ))
}

/// Write the bootstrap files for a fresh project:
/// - `<project>/.ccteam/spec.md` ← `request`
/// - `<project>/.ccteam/state.json` ← `ProjectState::initial`
/// - `<project>/.claude/settings.json` ← M0.4 template
/// - `<project>/CLAUDE.md` ← header + spec link
///
/// Returns the full project directory path.
pub fn bootstrap_project(
    paths: &CcteamPaths,
    slug: &str,
    request: &str,
) -> Result<PathBuf> {
    let project_dir = paths.project_dir(slug);
    let ccteam_dir = paths.project_ccteam_dir(slug);
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let spec_path = ccteam_dir.join("spec.md");
    let spec_body = format!(
        "---\nslug: {slug}\ncreated_at: {now}\n---\n\n# 用户需求\n\n{request}\n",
    );
    std::fs::write(&spec_path, spec_body)
        .with_context(|| format!("write {}", spec_path.display()))?;

    let state = ProjectState::initial(slug.to_string());
    state.save(&paths.project_state(slug))?;

    write_project_settings(&project_dir)?;

    let claude_md = project_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        let body = format!(
            "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n- 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 不要交互式询问。所有决策已在 .ccteam/plan-eng.md 中。\n- 测试不过不算完成。\n\n## 不做的事\n- 不要 git push(被 hook 拦截)\n- 不要修改 .ccteam/ 之外的元数据\n",
        );
        std::fs::write(&claude_md, body)
            .with_context(|| format!("write {}", claude_md.display()))?;
    }

    Ok(project_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_keeps_alphanumeric_lowercase() {
        assert_eq!(slugify("Hello World 123"), "hello-world-123");
        assert_eq!(slugify("Bookmark Manager (PWA)"), "bookmark-manager-pwa");
        assert_eq!(slugify("--leading-and-trailing--"), "leading-and-trailing");
        assert_eq!(slugify("multiple   spaces"), "multiple-spaces");
        assert_eq!(slugify("CamelCaseName"), "camelcasename");
    }

    #[test]
    fn slugify_falls_back_to_project_for_empty_input() {
        assert_eq!(slugify(""), "project");
        assert_eq!(slugify("中文 only"), "only");
    }

    #[test]
    fn slugify_truncates_to_40_chars() {
        let long = "a".repeat(80);
        assert!(slugify(&long).len() <= 40);
    }
}
