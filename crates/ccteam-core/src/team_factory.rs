//! V0.2 M0.22 — Team factory: scaffold a Claude Code plugin from a
//! `TeamSpec` so a user-authored team becomes a shareable artifact.
//!
//! Output layout (staging at `~/.config/ccteam/teams/<name>/`):
//!
//! ```text
//! <staging>/
//!   .claude-plugin/
//!     plugin.json                  # Claude Code plugin manifest
//!   team.yaml                      # ccteam team config (top-level
//!                                  #   unknown field for plugin loader)
//!   phases/
//!     01-<phase-1>.md              # frontmatter + body template
//!     ...
//!   README.md                      # author / install pointer
//! ```
//!
//! The factory does **not** write `agents/` / `commands/` /
//! `hooks/hooks.json` / `.mcp.json`. Those are optional plugin
//! conventions the team author can drop in by hand once the staging
//! tree exists; `ccteam doctor --validate-team` accepts them when
//! present and ignores them when absent.
//!
//! Why staging is separate from "published":
//! - PRD §4 — staging is the user's draft; publish promotes it to a
//!   marketplace (local symlink) or remote (github push). Two-stage
//!   keeps `ccteam team init` cheap (no network) and `publish` strict
//!   (validates first, fails loud if `gh` missing for github target).
//!
//! What rides as the plugin manifest:
//! - `name` / `description` / `author` — strict Claude Code plugin
//!   schema (plugin.json sample bodies inspected under
//!   `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/*/.claude-plugin/plugin.json`
//!   carry only those keys + occasional `version`).
//! - `team.yaml` lives at the plugin root as a ccteam-private file. The
//!   plugin loader's zod schema strips unknown root keys, so the
//!   plugin loads cleanly; ccteam reads the file directly via
//!   `team_resolver`. (alignment-review §2.7 confirms zod default
//!   behaviour.)

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::team::TeamSpec;
use crate::team_resolver::default_user_staging_dir;

/// Claude Code plugin manifest fields the factory writes. Mirrors the
/// schema observed in `claude-plugins-official` plugin.json files —
/// `name` + `description` + `author { name, email? }`. `version` is
/// optional but conventional (eg explanatory-output-style ships it).
///
/// The struct is **strict** on serialization (no unknown keys leak
/// into plugin.json) but the on-disk schema validation is **lenient**
/// — Claude Code's plugin loader strips unknown root fields by zod
/// default (alignment-review §2.7), so older / future schema additions
/// don't break ccteam.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub name: String,
    pub description: String,
    pub author: PluginAuthor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `author` block in plugin.json. `email` is optional.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl PluginManifest {
    /// Validate manifest fields the plugin loader cares about. Mirrors
    /// the constraints on `team.yaml.name` (ascii lower / digit / `-`
    /// / `_`) since the manifest name doubles as the plugin directory
    /// + ccteam team name.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("plugin.json: `name` must be non-empty");
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
        {
            bail!(
                "plugin.json: `name` must be ascii lower / digit / `-` / `_`; got `{}`",
                self.name,
            );
        }
        if self.description.trim().is_empty() {
            bail!("plugin.json: `description` must be non-empty");
        }
        if self.author.name.trim().is_empty() {
            bail!("plugin.json: `author.name` must be non-empty");
        }
        Ok(())
    }
}

/// V0.2 M0.22.2 input for `init_team_staging`. The factory
/// orchestrates the file layout; the caller (CLI / meta-agent skill)
/// supplies the per-team data.
#[derive(Debug, Clone)]
pub struct TeamInitInput<'a> {
    pub spec: &'a TeamSpec,
    pub manifest: &'a PluginManifest,
    /// Phase scaffolds to write under `phases/<NN>-<name>.md`. Order
    /// drives both filename ordering (NN prefix from index) and
    /// the on-disk DAG order. Empty list = evergreen team (no phases).
    pub phases: &'a [PhaseScaffold<'a>],
    /// `XDG_CONFIG_HOME` override for tests. None = real
    /// `default_user_staging_dir()`.
    pub staging_root_override: Option<&'a Path>,
}

/// One phase scaffold. The factory writes a phase markdown file with
/// minimal frontmatter (the required keys for `PhaseTemplate::parse` +
/// `validate_m0`) and a domain-task body template. Protocol literals
/// (`PHASE_DONE: …` / `ESCALATE: …`) are deliberately absent from the
/// body — V0.2 M0.18 keeps those inside the orchestrator's inject
/// prompt only.
#[derive(Debug, Clone)]
pub struct PhaseScaffold<'a> {
    pub name: &'a str,
    /// Free-text one-liner. Renders inside the body's "## 任务" section.
    pub task_summary: &'a str,
    pub required_inputs: &'a [&'a str],
    pub required_outputs: &'a [&'a str],
    /// V0.2 M0.19 default `auto_loop: true`. Authors who want a phase
    /// to run once and stop set this to false.
    pub auto_loop: bool,
}

/// Outcome of `init_team_staging` — surfaces every file written so the
/// CLI report can list them and tests can assert exact layout.
#[derive(Debug, Clone)]
pub struct InitReport {
    pub staging_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub team_yaml_path: PathBuf,
    pub phase_paths: Vec<PathBuf>,
    pub readme_path: PathBuf,
}

/// Resolve the staging dir for `team_name`. Honors
/// `staging_root_override` for tests.
pub fn staging_dir_for(team_name: &str, staging_root_override: Option<&Path>) -> PathBuf {
    let root = match staging_root_override {
        Some(p) => p.to_path_buf(),
        None => default_user_staging_dir(),
    };
    root.join("teams").join(team_name)
}

/// V0.2 M0.22.2: write the staging tree for a new team. Idempotent —
/// re-running with the same input produces the same files (overwrites).
/// Strictly refuses to clobber an existing staging dir whose
/// `.claude-plugin/plugin.json` carries a different `name`, since that
/// indicates a different team's tree at the same path.
pub fn init_team_staging(input: &TeamInitInput<'_>) -> Result<InitReport> {
    input.manifest.validate().context("plugin manifest")?;
    if input.spec.name != input.manifest.name {
        bail!(
            "team factory: spec.name `{}` != manifest.name `{}` — \
             keep them in lock-step (the plugin directory uses manifest.name)",
            input.spec.name,
            input.manifest.name,
        );
    }
    input.spec.validate().context("team.yaml spec")?;

    let staging = staging_dir_for(&input.spec.name, input.staging_root_override);
    if let Some(existing) = read_existing_manifest(&staging)? {
        if existing.name != input.manifest.name {
            bail!(
                "team factory: {} already holds plugin `{}` — refusing to overwrite. \
                 Delete or rename it before re-init.",
                staging.display(),
                existing.name,
            );
        }
    }

    let plugin_dir = staging.join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir)
        .with_context(|| format!("create {}", plugin_dir.display()))?;
    let manifest_path = plugin_dir.join("plugin.json");
    let manifest_body =
        serde_json::to_string_pretty(input.manifest).context("serialize plugin.json")?;
    std::fs::write(&manifest_path, format!("{manifest_body}\n"))
        .with_context(|| format!("write {}", manifest_path.display()))?;

    let team_yaml_path = staging.join("team.yaml");
    let team_yaml_body =
        serde_yaml::to_string(input.spec).context("serialize team.yaml")?;
    std::fs::write(&team_yaml_path, team_yaml_body)
        .with_context(|| format!("write {}", team_yaml_path.display()))?;

    let phase_dir_name: &str = if input.spec.phase_dir.is_empty() {
        "phases"
    } else {
        input.spec.phase_dir.as_str()
    };
    let phase_dir = staging.join(phase_dir_name);
    std::fs::create_dir_all(&phase_dir)
        .with_context(|| format!("create {}", phase_dir.display()))?;
    let mut phase_paths = Vec::with_capacity(input.phases.len());
    for (idx, scaffold) in input.phases.iter().enumerate() {
        let filename = format!("{:02}-{}.md", idx + 1, scaffold.name);
        let path = phase_dir.join(&filename);
        std::fs::write(&path, render_phase_scaffold(scaffold))
            .with_context(|| format!("write {}", path.display()))?;
        phase_paths.push(path);
    }

    let readme_path = staging.join("README.md");
    std::fs::write(&readme_path, render_readme(input))
        .with_context(|| format!("write {}", readme_path.display()))?;

    Ok(InitReport {
        staging_dir: staging,
        manifest_path,
        team_yaml_path,
        phase_paths,
        readme_path,
    })
}

fn read_existing_manifest(staging: &Path) -> Result<Option<PluginManifest>> {
    let path = staging.join(".claude-plugin").join("plugin.json");
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let manifest: PluginManifest = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(manifest))
}

/// Render one phase scaffold. Frontmatter carries only the required +
/// commonly-tweaked fields; serde defaults handle everything else at
/// parse time. Body has two sections (task summary + required outputs)
/// and zero protocol literals.
fn render_phase_scaffold(scaffold: &PhaseScaffold<'_>) -> String {
    let mut yaml = String::new();
    yaml.push_str("---\n");
    yaml.push_str(&format!("name: {}\n", scaffold.name));
    if !scaffold.required_inputs.is_empty() {
        yaml.push_str("required_inputs:\n");
        for input in scaffold.required_inputs {
            yaml.push_str(&format!("  - {input}\n"));
        }
    }
    if !scaffold.required_outputs.is_empty() {
        yaml.push_str("required_outputs:\n");
        for output in scaffold.required_outputs {
            yaml.push_str(&format!("  - {output}\n"));
        }
    }
    yaml.push_str("parallelism: solo\n");
    yaml.push_str(&format!("auto_loop: {}\n", scaffold.auto_loop));
    yaml.push_str("---\n\n");
    yaml.push_str(&format!("# {}\n\n", scaffold.name));
    yaml.push_str("## 任务\n\n");
    yaml.push_str(scaffold.task_summary);
    if !scaffold.task_summary.ends_with('\n') {
        yaml.push('\n');
    }
    if !scaffold.required_outputs.is_empty() {
        yaml.push_str("\n## 产出\n\n");
        for output in scaffold.required_outputs {
            yaml.push_str(&format!("- `{output}`\n"));
        }
    }
    yaml.push_str(
        "\n> 本文件由 ccteam team factory 生成。正文是任务描述,正文不写协议关键字\n\
         > (`PHASE_DONE` / `ESCALATE` 由 orchestrator inject prompt 注入)。\n",
    );
    yaml
}

fn render_readme(input: &TeamInitInput<'_>) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", input.manifest.name));
    s.push_str(&format!("{}\n\n", input.manifest.description));
    s.push_str("## Install\n\n");
    s.push_str("```bash\n");
    s.push_str(&format!(
        "ccteam team publish {} --target local\n",
        input.manifest.name,
    ));
    s.push_str(&format!(
        "claude /plugin enable {}@ccteam-local\n",
        input.manifest.name,
    ));
    s.push_str("```\n\n");
    s.push_str("## Phases\n\n");
    if input.phases.is_empty() {
        s.push_str("(no phases — evergreen team)\n");
    } else {
        for scaffold in input.phases {
            s.push_str(&format!("- `{}` — {}\n", scaffold.name, scaffold.task_summary));
        }
    }
    s.push('\n');
    s.push_str("Authored via `ccteam team init`. Edit `phases/*.md` bodies to fill in domain detail.\n");
    s
}

/// Publish target — selects between local marketplace symlink and a
/// remote (github) push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishTarget {
    /// Symlink staging dir into
    /// `~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`.
    Local,
    /// Push to a new GitHub repo + return the URL. Caller supplies the
    /// repo coordinates (`owner/name`); the factory shells out to
    /// `gh repo create` + git push.
    Github { repo: String },
}

/// Publish report — every path / URL touched.
#[derive(Debug, Clone)]
pub struct PublishReport {
    pub target: PublishTarget,
    /// Local install handle when target = Local
    /// (`~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>`).
    pub local_link: Option<PathBuf>,
    /// Remote URL when target = Github.
    pub github_url: Option<String>,
}

/// Options for `publish_team`. `claude_dir_override` and
/// `git_runner_override` are test-injection points.
#[derive(Debug, Clone)]
pub struct PublishInput<'a> {
    pub team_name: &'a str,
    pub target: PublishTarget,
    pub staging_root_override: Option<&'a Path>,
    pub claude_dir_override: Option<&'a Path>,
}

/// V0.2 M0.22.3: publish a staged team. For Local target, link the
/// staging tree into the ccteam-local marketplace. For Github, do not
/// run the actual gh CLI from this function; the CLI command shell-runs
/// the gh / git steps and uses this only to validate the staging dir
/// exists. (Test boundary — the unit-tested function returns the
/// expected paths; the CLI handles the side effects.)
pub fn publish_team(input: &PublishInput<'_>) -> Result<PublishReport> {
    let staging = staging_dir_for(input.team_name, input.staging_root_override);
    if !staging.exists() {
        bail!(
            "ccteam team publish: staging dir {} not found — \
             run `ccteam team init {}` first",
            staging.display(),
            input.team_name,
        );
    }
    if !staging.join(".claude-plugin/plugin.json").exists() {
        bail!(
            "ccteam team publish: {} is not a complete plugin staging tree \
             (missing .claude-plugin/plugin.json) — re-run `ccteam team init`",
            staging.display(),
        );
    }

    match &input.target {
        PublishTarget::Local => {
            let claude = match input.claude_dir_override {
                Some(p) => p.to_path_buf(),
                None => crate::tool_surface::user_claude_dir()?,
            };
            let marketplace_root = claude
                .join("plugins")
                .join("marketplaces")
                .join("ccteam-local")
                .join("plugins");
            std::fs::create_dir_all(&marketplace_root).with_context(|| {
                format!(
                    "create ccteam-local marketplace dir {}",
                    marketplace_root.display(),
                )
            })?;
            let link = marketplace_root.join(input.team_name);
            // Idempotent: remove any prior symlink before re-linking.
            // Directory entries are not removed (avoid clobbering an
            // unrelated team that happens to share the name).
            match std::fs::symlink_metadata(&link) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    std::fs::remove_file(&link).with_context(|| {
                        format!("remove stale symlink {}", link.display())
                    })?;
                }
                Ok(_) => bail!(
                    "ccteam team publish: {} exists and is not a symlink — \
                     refusing to overwrite",
                    link.display(),
                ),
                Err(_) => {}
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&staging, &link).with_context(|| {
                format!(
                    "symlink {} -> {}",
                    link.display(),
                    staging.display(),
                )
            })?;
            #[cfg(not(unix))]
            std::fs::create_dir_all(&link).context("non-unix publish-local fallback")?;
            ensure_marketplace_json(&claude)?;
            Ok(PublishReport {
                target: input.target.clone(),
                local_link: Some(link),
                github_url: None,
            })
        }
        PublishTarget::Github { repo } => {
            // Repo coordinate sanity check — `<owner>/<name>` shape so
            // `gh repo create` doesn't fail with a confusing error.
            if !repo.contains('/') || repo.split('/').count() != 2 {
                bail!(
                    "ccteam team publish --target github: --repo must be `<owner>/<name>`; got `{repo}`",
                );
            }
            let url = format!("https://github.com/{repo}");
            Ok(PublishReport {
                target: input.target.clone(),
                local_link: None,
                github_url: Some(url),
            })
        }
    }
}

/// Ensure `<claude>/plugins/marketplaces/ccteam-local/marketplace.json`
/// exists with a minimal schema so `claude /plugin enable` finds the
/// directory-source marketplace. Idempotent — overwrites a pre-existing
/// `name: "ccteam-local"` file each call (safe — no user content).
fn ensure_marketplace_json(claude_dir: &Path) -> Result<()> {
    let path = claude_dir
        .join("plugins")
        .join("marketplaces")
        .join("ccteam-local")
        .join("marketplace.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::json!({
        "name": "ccteam-local",
        "description": "ccteam team factory — local staging marketplace",
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&body)
            .map_err(|e| anyhow!("serialize marketplace.json: {e}"))?,
    )
    .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// V0.2 M0.22.4: validation pass over a staged team. Re-uses
/// `TeamSpec::validate` and `PluginManifest::validate`, plus
/// cross-checks the two `name` fields agree. Returns a list of
/// human-readable findings (`[OK]` / `[WARN]` / `[FAIL]` lines) the
/// caller can append to a doctor report. Empty Ok list = nothing
/// found that warrants surfacing.
pub fn validate_staged_team(staging: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let manifest_path = staging.join(".claude-plugin").join("plugin.json");
    if !manifest_path.exists() {
        out.push(format!(
            "[FAIL] missing plugin manifest at {}",
            manifest_path.display(),
        ));
        return Ok(out);
    }
    let manifest_body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: PluginManifest = match serde_json::from_str(&manifest_body) {
        Ok(m) => m,
        Err(err) => {
            out.push(format!("[FAIL] plugin.json parse: {err}"));
            return Ok(out);
        }
    };
    if let Err(err) = manifest.validate() {
        out.push(format!("[FAIL] plugin.json: {err:#}"));
    } else {
        out.push(format!("[OK] plugin.json `name={}`", manifest.name));
    }

    let yaml_path = staging.join("team.yaml");
    if !yaml_path.exists() {
        out.push(format!(
            "[FAIL] missing team.yaml at {}",
            yaml_path.display(),
        ));
        return Ok(out);
    }
    let spec = match TeamSpec::load(&yaml_path) {
        Ok(s) => s,
        Err(err) => {
            out.push(format!("[FAIL] team.yaml: {err:#}"));
            return Ok(out);
        }
    };
    out.push(format!("[OK] team.yaml `name={}`", spec.name));
    if spec.name != manifest.name {
        out.push(format!(
            "[FAIL] team.yaml.name=`{}` != plugin.json.name=`{}` — \
             keep them in lock-step",
            spec.name, manifest.name,
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_spec() -> TeamSpec {
        TeamSpec::parse(
            "name: example\n\
             description: example team\n\
             phase_dir: phases\n",
        )
        .unwrap()
    }

    fn sample_manifest() -> PluginManifest {
        PluginManifest {
            name: "example".into(),
            description: "example team plugin".into(),
            author: PluginAuthor {
                name: "tester".into(),
                email: Some("t@example.com".into()),
            },
            version: Some("0.1.0".into()),
        }
    }

    fn run_init(tmp: &TempDir) -> InitReport {
        let spec = sample_spec();
        let manifest = sample_manifest();
        let phases = vec![
            PhaseScaffold {
                name: "kickoff",
                task_summary: "interview the user, write `.ccteam/spec.md`.",
                required_inputs: &[],
                required_outputs: &[".ccteam/spec.md"],
                auto_loop: true,
            },
            PhaseScaffold {
                name: "build",
                task_summary: "do the work; emit `.ccteam/build.md`.",
                required_inputs: &[".ccteam/spec.md"],
                required_outputs: &[".ccteam/build.md"],
                auto_loop: true,
            },
        ];
        init_team_staging(&TeamInitInput {
            spec: &spec,
            manifest: &manifest,
            phases: &phases,
            staging_root_override: Some(tmp.path()),
        })
        .unwrap()
    }

    #[test]
    fn init_writes_full_plugin_layout() {
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        assert!(report.manifest_path.exists());
        assert!(report.team_yaml_path.exists());
        assert_eq!(report.phase_paths.len(), 2);
        assert!(report.phase_paths[0].file_name().unwrap() == "01-kickoff.md");
        assert!(report.phase_paths[1].file_name().unwrap() == "02-build.md");
        assert!(report.readme_path.exists());
    }

    #[test]
    fn init_writes_valid_plugin_manifest_json() {
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        let body = std::fs::read_to_string(&report.manifest_path).unwrap();
        let manifest: PluginManifest = serde_json::from_str(&body).unwrap();
        assert_eq!(manifest.name, "example");
        assert_eq!(manifest.author.name, "tester");
        assert_eq!(manifest.version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn init_writes_team_yaml_at_plugin_root() {
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        // team.yaml lives at root, NOT under .claude-plugin/, so the
        // plugin loader's zod schema strips it as an unknown root file
        // (alignment-review §2.7).
        assert_eq!(
            report.team_yaml_path.parent().unwrap(),
            report.staging_dir,
        );
    }

    #[test]
    fn init_phase_body_excludes_protocol_literals() {
        // V0.2 M0.18: phase markdown bodies must not carry PHASE_DONE /
        // ESCALATE. Those are inject-prompt-only.
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        for path in &report.phase_paths {
            let body = std::fs::read_to_string(path).unwrap();
            assert!(
                !body.contains("PHASE_DONE:"),
                "phase {} body has PHASE_DONE literal",
                path.display(),
            );
            assert!(
                !body.contains("ESCALATE:"),
                "phase {} body has ESCALATE literal",
                path.display(),
            );
        }
    }

    #[test]
    fn init_phase_frontmatter_parses_back_into_phase_template() {
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        for path in &report.phase_paths {
            let template = crate::phases::PhaseTemplate::load(path).unwrap();
            template.validate_m0().unwrap();
        }
    }

    #[test]
    fn init_team_yaml_round_trips_through_team_spec_load() {
        let tmp = TempDir::new().unwrap();
        let report = run_init(&tmp);
        let reloaded = TeamSpec::load(&report.team_yaml_path).unwrap();
        assert_eq!(reloaded.name, "example");
    }

    #[test]
    fn init_refuses_to_overwrite_a_different_teams_staging_dir() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        // Tamper: rewrite the manifest with a different name.
        let staging = staging_dir_for("example", Some(tmp.path()));
        let manifest_path = staging.join(".claude-plugin/plugin.json");
        let body = std::fs::read_to_string(&manifest_path).unwrap();
        let mut manifest: PluginManifest = serde_json::from_str(&body).unwrap();
        manifest.name = "intruder".into();
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let spec = sample_spec();
        let new_manifest = sample_manifest();
        let err = init_team_staging(&TeamInitInput {
            spec: &spec,
            manifest: &new_manifest,
            phases: &[],
            staging_root_override: Some(tmp.path()),
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("intruder"));
    }

    #[test]
    fn init_rejects_mismatched_spec_and_manifest_names() {
        let tmp = TempDir::new().unwrap();
        let spec = sample_spec();
        let mut manifest = sample_manifest();
        manifest.name = "different-name".into();
        let err = init_team_staging(&TeamInitInput {
            spec: &spec,
            manifest: &manifest,
            phases: &[],
            staging_root_override: Some(tmp.path()),
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("lock-step"));
    }

    #[test]
    fn manifest_validate_rejects_invalid_name() {
        let mut m = sample_manifest();
        m.name = "Has Space".into();
        let err = m.validate().unwrap_err();
        assert!(format!("{err:#}").contains("ascii"));
    }

    #[test]
    fn manifest_validate_rejects_empty_description() {
        let mut m = sample_manifest();
        m.description = "  ".into();
        let err = m.validate().unwrap_err();
        assert!(format!("{err:#}").contains("description"));
    }

    #[test]
    fn manifest_validate_rejects_empty_author_name() {
        let mut m = sample_manifest();
        m.author.name = "".into();
        let err = m.validate().unwrap_err();
        assert!(format!("{err:#}").contains("author"));
    }

    #[test]
    fn validate_staged_team_returns_ok_for_a_just_initted_team() {
        let tmp = TempDir::new().unwrap();
        let _report = run_init(&tmp);
        let staging = staging_dir_for("example", Some(tmp.path()));
        let findings = validate_staged_team(&staging).unwrap();
        assert!(findings.iter().any(|l| l.starts_with("[OK] plugin.json")));
        assert!(findings.iter().any(|l| l.starts_with("[OK] team.yaml")));
        assert!(!findings.iter().any(|l| l.starts_with("[FAIL]")));
    }

    #[test]
    fn validate_staged_team_flags_missing_manifest() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("orphan");
        std::fs::create_dir_all(&staging).unwrap();
        let findings = validate_staged_team(&staging).unwrap();
        assert!(findings.iter().any(|l| l.contains("[FAIL] missing plugin manifest")));
    }

    #[test]
    fn validate_staged_team_flags_name_mismatch_between_manifest_and_yaml() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        let staging = staging_dir_for("example", Some(tmp.path()));
        // Tamper: rewrite team.yaml with a mismatched name.
        std::fs::write(staging.join("team.yaml"), "name: drifted\n").unwrap();
        let findings = validate_staged_team(&staging).unwrap();
        assert!(
            findings.iter().any(|l| l.starts_with("[FAIL]") && l.contains("lock-step")),
            "got: {findings:?}",
        );
    }

    #[test]
    fn publish_local_creates_marketplace_symlink() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        let claude = tmp.path().join("claude-home");
        std::fs::create_dir_all(&claude).unwrap();
        let report = publish_team(&PublishInput {
            team_name: "example",
            target: PublishTarget::Local,
            staging_root_override: Some(tmp.path()),
            claude_dir_override: Some(&claude),
        })
        .unwrap();
        let link = report.local_link.expect("local target produces link");
        assert!(link.exists());
        // Symlink target = staging dir.
        let canonical = std::fs::canonicalize(&link).unwrap();
        let staging = staging_dir_for("example", Some(tmp.path()));
        assert_eq!(canonical, std::fs::canonicalize(&staging).unwrap());
        // marketplace.json exists under ccteam-local/.
        let mkt = claude
            .join("plugins/marketplaces/ccteam-local/marketplace.json");
        assert!(mkt.exists());
    }

    #[test]
    fn publish_local_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        let claude = tmp.path().join("claude-home");
        std::fs::create_dir_all(&claude).unwrap();
        for _ in 0..2 {
            publish_team(&PublishInput {
                team_name: "example",
                target: PublishTarget::Local,
                staging_root_override: Some(tmp.path()),
                claude_dir_override: Some(&claude),
            })
            .unwrap();
        }
        // Symlink survives the second pass.
        let link = claude
            .join("plugins/marketplaces/ccteam-local/plugins/example");
        assert!(link.exists());
    }

    #[test]
    fn publish_fails_loud_when_staging_missing() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude-home");
        let err = publish_team(&PublishInput {
            team_name: "ghost",
            target: PublishTarget::Local,
            staging_root_override: Some(tmp.path()),
            claude_dir_override: Some(&claude),
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("staging dir"));
    }

    #[test]
    fn publish_github_validates_repo_coordinate_shape() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        let err = publish_team(&PublishInput {
            team_name: "example",
            target: PublishTarget::Github {
                repo: "missing-slash".into(),
            },
            staging_root_override: Some(tmp.path()),
            claude_dir_override: None,
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("owner"));
    }

    #[test]
    fn publish_github_returns_https_url() {
        let tmp = TempDir::new().unwrap();
        let _ = run_init(&tmp);
        let report = publish_team(&PublishInput {
            team_name: "example",
            target: PublishTarget::Github {
                repo: "alice/example".into(),
            },
            staging_root_override: Some(tmp.path()),
            claude_dir_override: None,
        })
        .unwrap();
        assert_eq!(
            report.github_url.as_deref(),
            Some("https://github.com/alice/example"),
        );
    }
}
