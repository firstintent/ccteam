//! Embedded ccteam templates rendered into produced-project trees by
//! `ccteam new` and the global `~/.ccteam/` skeleton produced by
//! `ccteam init`. The settings.json source uses the placeholder
//! `__CCTEAM_BIN__` for the binary path so we can rewrite hook commands
//! to absolute paths at install time (otherwise hook subprocesses
//! inherit Claude Code's PATH and silently fail to find `ccteam`).

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// Per-project `.claude/settings.json` template. The template uses
/// `__CCTEAM_BIN__` everywhere a real install would name `ccteam`; we
/// substitute the running binary's absolute path at write time so hook
/// subprocesses don't depend on the user's PATH (which Claude Code
/// inherits and may not include the ccteam install dir).
pub const PROJECT_SETTINGS_JSON: &str = include_str!("templates/settings.json");

/// Phase template payloads — `(global_filename_with_index_prefix, body)`.
/// Project-local copies use the unprefixed name (e.g. `plan-eng.md`)
/// because the phase prompt in `progress::build_phase_prompt` references
/// `@.ccteam/phases/<phase>.md` without the numeric prefix.
///
/// **Backwards-compat alias** for the dev team's phase set. M0–M2
/// callers loading dev phases directly still work; M3.3+ should look
/// up via `team_bundle("dev")` so multi-team installs keep working.
pub const PHASE_TEMPLATES: &[(&str, &str)] = DEV_PHASE_TEMPLATES;

// V0.2 M0.17.1: shipped phases now live under `teams/<name>/phases/`
// alongside their `team.yaml` (was `phases/` / `phases-product-research/`
// at repo root). Layout matches what `~/.ccteam/teams/<name>/` looks
// like after seeding, so the include_str! paths are 1:1 with on-disk.
const DEV_PHASE_TEMPLATES: &[(&str, &str)] = &[
    ("02-plan-eng.md", include_str!("../../../teams/dev/phases/02-plan-eng.md")),
    ("03-implement.md", include_str!("../../../teams/dev/phases/03-implement.md")),
    ("04-test-author.md", include_str!("../../../teams/dev/phases/04-test-author.md")),
    ("05-test-run.md", include_str!("../../../teams/dev/phases/05-test-run.md")),
    ("06-fix.md", include_str!("../../../teams/dev/phases/06-fix.md")),
    ("09-ship.md", include_str!("../../../teams/dev/phases/09-ship.md")),
];

/// product-research team phase set (M3.4). Six phases, all
/// `parallelism: solo`. Last two phases use `decision_mode: async`
/// so user-decision points write to outbox instead of blocking.
const PRODUCT_RESEARCH_PHASE_TEMPLATES: &[(&str, &str)] = &[
    (
        "01-kickoff.md",
        include_str!("../../../teams/product-research/phases/01-kickoff.md"),
    ),
    (
        "02-market-survey.md",
        include_str!("../../../teams/product-research/phases/02-market-survey.md"),
    ),
    (
        "03-differentiation-analysis.md",
        include_str!("../../../teams/product-research/phases/03-differentiation-analysis.md"),
    ),
    (
        "04-value-proposition.md",
        include_str!("../../../teams/product-research/phases/04-value-proposition.md"),
    ),
    (
        "05-feasibility.md",
        include_str!("../../../teams/product-research/phases/05-feasibility.md"),
    ),
    (
        "06-verdict.md",
        include_str!("../../../teams/product-research/phases/06-verdict.md"),
    ),
];

/// Embedded `team.yaml` files keyed by team name. M3.4 ships dev +
/// product-research; V0.2 M0.16 adds meta-agent (evergreen). `ccteam
/// init` writes these to `~/.ccteam/teams/<name>/team.yaml`. The
/// orchestrator reads them at startup to resolve `phase_dir`,
/// registered ESCALATE prefixes, the V0.2 `evergreen` / `cost_policy`
/// flags, and (eventually) team-wide golden rules.
const DEV_TEAM_YAML: &str = include_str!("../../../teams/dev/team.yaml");
const PRODUCT_RESEARCH_TEAM_YAML: &str =
    include_str!("../../../teams/product-research/team.yaml");
const META_AGENT_TEAM_YAML: &str = include_str!("../../../teams/meta-agent/team.yaml");

/// Empty phase set for evergreen teams (meta-agent). The orchestrator
/// builds a `Dag::from_templates(&[])` for these, which `decide_tick`
/// short-circuits to `NoOp`. `process_meta_project` runs the
/// alternative event-loop path instead.
const META_AGENT_PHASE_TEMPLATES: &[(&str, &str)] = &[];

/// One team's compile-time bundle: a `team.yaml` body + the phase
/// markdowns to stamp into the project's `<project>/.ccteam/phases/`
/// dir on `ccteam new`. Looking up by team name keeps every
/// per-team artifact in one place — adding a team is a config change.
///
/// V0.2 §6.4 candidate 3: `pub(crate)` — runtime code paths now read
/// disk (`~/.ccteam/teams/<name>/team.yaml`); this struct is only
/// the in-binary seed source for `write_all_global_team_templates`
/// and the bootstrap-time helpers in `projects.rs`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TeamTemplateBundle {
    pub team_yaml: &'static str,
    pub phases: &'static [(&'static str, &'static str)],
}

/// In-binary seed source for shipped teams. **Not** a runtime
/// registry — production lookups walk `~/.ccteam/teams/<name>/team.yaml`
/// (see `Orchestrator::team_runtime` /
/// `memory_bridge::discover_bridge_teams`). Adding a new shipped team
/// = add an entry here + author its YAML + markdowns; user-authored
/// teams skip this entirely (V0.2 M0.22 team factory).
pub(crate) const TEAM_BUNDLES: &[(&str, TeamTemplateBundle)] = &[
    (
        "dev",
        TeamTemplateBundle {
            team_yaml: DEV_TEAM_YAML,
            phases: DEV_PHASE_TEMPLATES,
        },
    ),
    (
        "product-research",
        TeamTemplateBundle {
            team_yaml: PRODUCT_RESEARCH_TEAM_YAML,
            phases: PRODUCT_RESEARCH_PHASE_TEMPLATES,
        },
    ),
    (
        "meta-agent",
        TeamTemplateBundle {
            team_yaml: META_AGENT_TEAM_YAML,
            phases: META_AGENT_PHASE_TEMPLATES,
        },
    ),
];

/// Resolve a shipped team's compile-time seed bundle. Returns `None`
/// for unknown teams so callers fall back to "no embedded templates" —
/// the right behavior for user-authored teams that live entirely on
/// disk. V0.2 §6.4 candidate 3: `pub(crate)` — only bootstrap-time
/// callers in ccteam-core may use this.
pub(crate) fn team_bundle(team: &str) -> Option<TeamTemplateBundle> {
    TEAM_BUNDLES
        .iter()
        .find_map(|(name, bundle)| (*name == team).then_some(*bundle))
}

/// M2.4: helper templates that phase markdown can `@`-reference. Shipped
/// inside the binary so a fresh install (or `ccteam doctor`) can stamp
/// them into `~/.ccteam/templates/` without an external git checkout.
///
/// `(on_disk_filename, body)` — phase markdown references them as
/// `@~/.ccteam/templates/<on_disk_filename>` and Claude Code's native
/// `@` mechanism inlines the body at prompt-build time. The orchestrator
/// does not parse these — they're pure Claude Code surface.
///
/// Filenames use hyphens to match the @-reference convention; the Rust
/// source filenames use underscores to match Rust module conventions.
pub const HELPER_TEMPLATES: &[(&str, &str)] = &[
    (
        "review-with-user-loop.md",
        include_str!("templates/review_with_user_loop.md"),
    ),
    (
        "kickoff-reverse-interview.md",
        include_str!("templates/kickoff_reverse_interview.md"),
    ),
];

/// Strip a leading `NN-` index prefix off a global phase filename so it
/// matches the path the phase prompt asks claude to read
/// (`@.ccteam/phases/<phase>.md`). Returns the original name unchanged
/// when no prefix is present.
pub fn project_phase_filename(global: &str) -> &str {
    let bytes = global.as_bytes();
    if bytes.len() > 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'-'
    {
        &global[3..]
    } else {
        global
    }
}

/// Resolve the path to the running ccteam binary. Falls back from
/// canonicalized to raw `current_exe()` because `canonicalize` rejects
/// some `/proc/self/exe`-style paths in container test environments.
pub fn current_ccteam_bin() -> Result<std::path::PathBuf> {
    let raw = std::env::current_exe().context("std::env::current_exe")?;
    Ok(raw.canonicalize().unwrap_or(raw))
}

/// Extra `env` keys to inject into the rendered settings.json. Used to
/// freeze `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` per-project so hook
/// subprocesses see the same paths the orchestrator uses, regardless of
/// the tmux server's potentially stale environment block.
#[derive(Debug, Default, Clone)]
pub struct SettingsEnv {
    pub ccteam_home: Option<String>,
    pub ccteam_projects_root: Option<String>,
}

/// Optional `enabledPlugins` map for the rendered settings.json. V0.2
/// M0.20: spawned project sessions enable each plugin a phase's
/// `tools_required.subagents` resolves to (via `plugin_resolution`) so
/// Claude Code's in-memory plugin pipeline auto-namespaces the agent
/// without ccteam-core ln -sf'ing into `~/.claude/agents/`.
///
/// Each entry is the `<plugin>@<marketplace>` key Claude Code's plugin
/// pipeline expects.
#[derive(Debug, Default, Clone)]
pub struct EnabledPluginsSetting {
    pub plugin_ids: std::collections::BTreeSet<String>,
}

/// Render `PROJECT_SETTINGS_JSON` with `__CCTEAM_BIN__` replaced by the
/// given absolute binary path, `extra_env` merged into the top-level
/// `env` block, and `enabled` written under `enabledPlugins`. Validates
/// that the rewritten body is still valid JSON so a path with
/// shell-hostile characters can't silently corrupt the settings file.
pub fn render_project_settings(
    ccteam_bin: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    let bin = ccteam_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8: {}", ccteam_bin.display()))?;
    if bin.contains('"') || bin.contains('\\') {
        return Err(anyhow!(
            "ccteam binary path contains characters that can't be embedded in settings.json: {bin}"
        ));
    }
    let body = PROJECT_SETTINGS_JSON.replace("__CCTEAM_BIN__", bin);
    let mut v: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.json is not valid JSON (bin={bin})"))?;
    if let Some(env) = v.get_mut("env").and_then(|e| e.as_object_mut()) {
        if let Some(home) = &extra_env.ccteam_home {
            env.insert("CCTEAM_HOME".into(), Value::String(home.clone()));
        }
        if let Some(proj) = &extra_env.ccteam_projects_root {
            env.insert(
                "CCTEAM_PROJECTS_ROOT".into(),
                Value::String(proj.clone()),
            );
        }
    }
    if !enabled.plugin_ids.is_empty() {
        let mut map = serde_json::Map::new();
        for id in &enabled.plugin_ids {
            map.insert(id.clone(), Value::Bool(true));
        }
        v.as_object_mut()
            .ok_or_else(|| anyhow!("settings.json root is not an object"))?
            .insert("enabledPlugins".into(), Value::Object(map));
    }
    serde_json::to_string_pretty(&v).context("serialize rendered settings.json")
}

/// Write `<project_dir>/.claude/settings.json` with hook commands
/// pointing at the running ccteam binary by absolute path, the
/// effective `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` baked into `env`,
/// and the spawned-session `enabledPlugins` set. Creates the parent
/// dir if missing. Idempotent — overwrites any prior render so
/// re-running after a ccteam upgrade refreshes paths and the
/// plugin set.
pub fn write_project_settings(
    project_dir: &Path,
    enabled: &EnabledPluginsSetting,
) -> Result<()> {
    let dir = project_dir.join(".claude");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("settings.json");
    let bin = current_ccteam_bin()?;
    let extra = SettingsEnv {
        ccteam_home: std::env::var("CCTEAM_HOME").ok(),
        ccteam_projects_root: std::env::var("CCTEAM_PROJECTS_ROOT").ok(),
    };
    let body = render_project_settings(&bin, &extra, enabled)?;
    std::fs::write(&path, body)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Write each dev `PHASE_TEMPLATES` entry into
/// `<project_dir>/.ccteam/phases/` under its **unprefixed** name
/// (e.g. `plan-eng.md`) so it matches the path embedded in
/// `progress::build_phase_prompt`. Idempotent; later calls overwrite.
///
/// **M3.3 alias** for the dev team. Use
/// `write_project_phase_templates_for_team` for non-dev teams.
pub fn write_project_phase_templates(project_dir: &Path) -> Result<()> {
    write_project_phase_templates_for_team(project_dir, "dev")
}

/// M3.3: write the project-local phase templates for `team` under
/// `<project_dir>/.ccteam/phases/`. Evergreen teams (e.g. meta-agent)
/// ship an empty phase set — the loop body is a no-op for them, so
/// the function returns successfully without ever creating the
/// `phases/` directory. (V0.2 §6.4 candidate 5 — declarative "no
/// phases" via empty bundle, replacing the prior `team ==
/// META_TEAM_NAME` literal.)
pub fn write_project_phase_templates_for_team(
    project_dir: &Path,
    team: &str,
) -> Result<()> {
    let bundle = team_bundle(team).ok_or_else(|| {
        anyhow!(
            "no embedded phase templates for team `{team}` — \
             ensure `~/.ccteam/teams/{team}/team.yaml` and \
             `~/.ccteam/<phase_dir>/` are populated, or add the team \
             to TEAM_BUNDLES in templates.rs"
        )
    })?;
    if bundle.phases.is_empty() {
        // Evergreen / phase-less team — skip the directory creation
        // so we don't litter empty `.ccteam/phases/` dirs across
        // event-loop project trees.
        return Ok(());
    }
    let dir = project_dir.join(".ccteam").join("phases");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (global, body) in bundle.phases {
        let name = project_phase_filename(global);
        let path = dir.join(name);
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Write each `PHASE_TEMPLATES` entry into `<global_dir>/phases/` under
/// its full prefixed name (e.g. `02-plan-eng.md`). Used by `ccteam init`
/// so the orchestrator can load + validate templates from `~/.ccteam/`.
/// `force == false` skips files already on disk so an operator can hand-
/// edit a global template and not lose it on re-init.
///
/// **M3.3 backwards-compat shim**: also writes the dev phases through
/// the new team-aware path. `write_all_global_team_templates` is the
/// preferred entry point for new code.
pub fn write_global_phase_templates(global_dir: &Path, force: bool) -> Result<()> {
    let dir = global_dir.join("phases");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (global, body) in PHASE_TEMPLATES {
        let path = dir.join(global);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// V0.2 M0.17.2: write every shipped team's seed under the unified
/// `<global_dir>/teams/<name>/` layout — `team.yaml` at the team-dir
/// root, phases under `<team_dir>/<spec.phase_dir>/<NN-name>.md`
/// (default `phases`). Idempotent — `force=false` preserves operator
/// hand-edits.
///
/// Replaces the M3.x layout where phase_dir was relative to
/// `~/.ccteam/` and product-research used `phases-product-research`
/// to avoid collision with dev's `phases`. Under the new layout,
/// each team's phase dir lives inside its own team directory so the
/// per-team prefix is redundant.
pub fn write_all_global_team_templates(global_dir: &Path, force: bool) -> Result<()> {
    use crate::team::TeamSpec;
    for (name, bundle) in TEAM_BUNDLES {
        let spec = TeamSpec::parse(bundle.team_yaml).with_context(|| {
            format!("embedded team.yaml for `{name}` does not match TeamSpec schema")
        })?;
        let team_dir = global_dir.join("teams").join(name);
        std::fs::create_dir_all(&team_dir)
            .with_context(|| format!("create {}", team_dir.display()))?;
        // Phase markdowns → <global>/teams/<name>/<phase_dir>/<NN-name>.md.
        // Skip the directory creation for evergreen / phase-less teams
        // so we don't leave empty placeholder dirs around.
        if !bundle.phases.is_empty() {
            let phase_dir = team_dir.join(&spec.phase_dir);
            std::fs::create_dir_all(&phase_dir)
                .with_context(|| format!("create {}", phase_dir.display()))?;
            for (filename, body) in bundle.phases {
                let path = phase_dir.join(filename);
                if path.exists() && !force {
                    continue;
                }
                std::fs::write(&path, body)
                    .with_context(|| format!("write {}", path.display()))?;
            }
        }
        // team.yaml → <global>/teams/<name>/team.yaml.
        let team_yaml_path = team_dir.join("team.yaml");
        if team_yaml_path.exists() && !force {
            continue;
        }
        std::fs::write(&team_yaml_path, bundle.team_yaml).with_context(|| {
            format!("write {}", team_yaml_path.display())
        })?;
    }
    Ok(())
}

/// M2.4: write the embedded `HELPER_TEMPLATES` into
/// `<global_dir>/templates/<filename>` so phase markdown's
/// `@~/.ccteam/templates/<filename>` reference resolves. Idempotent;
/// `force == false` preserves operator hand-edits.
///
/// Called by `ccteam init` (global skeleton) and `bootstrap_project`
/// (defensive — covers the user who jumps straight to `ccteam new`
/// without `ccteam init`). The two callers don't conflict because
/// the writer is a no-op when files are already in place.
pub fn write_global_helper_templates(global_dir: &Path, force: bool) -> Result<()> {
    let dir = global_dir.join("templates");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (filename, body) in HELPER_TEMPLATES {
        let path = dir.join(filename);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn template_is_valid_json_with_expected_hook_keys() {
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("rendered template must be valid JSON");
        let hooks = v["hooks"].as_object().expect("hooks object");
        for required in [
            "SessionStart",
            "Stop",
            "Notification",
            "PreToolUse",
            "PostToolUse",
            "SubagentStop",
            "SessionEnd",
        ] {
            assert!(
                hooks.contains_key(required),
                "settings.json template missing `{required}` hook entry",
            );
        }
    }

    #[test]
    fn template_session_start_uses_absolute_ccteam_path() {
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entries = v["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap();
        let cmds: Vec<&str> = entries
            .iter()
            .map(|e| e["command"].as_str().unwrap())
            .collect();
        assert_eq!(
            cmds,
            vec![
                "/usr/local/bin/ccteam hook load-context",
                "/usr/local/bin/ccteam hook progress-append session_start"
            ],
        );
    }

    #[test]
    fn raw_template_uses_placeholder_not_bare_ccteam() {
        // Guard against accidentally re-introducing `"command": "ccteam …"`
        // — the un-substituted form makes hook subprocesses depend on PATH,
        // which Claude Code inherits from its parent and which often does
        // not include the ccteam install dir.
        assert!(
            PROJECT_SETTINGS_JSON.contains("__CCTEAM_BIN__"),
            "template should reference __CCTEAM_BIN__ placeholder",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains("\"ccteam hook"),
            "template should not embed bare `ccteam hook` — see render_project_settings",
        );
    }

    #[test]
    fn project_phase_filename_strips_two_digit_prefix() {
        assert_eq!(project_phase_filename("02-plan-eng.md"), "plan-eng.md");
        assert_eq!(project_phase_filename("09-ship.md"), "ship.md");
        assert_eq!(project_phase_filename("plan-eng.md"), "plan-eng.md");
        assert_eq!(project_phase_filename("ab-foo.md"), "ab-foo.md"); // not digits
    }

    #[test]
    fn write_project_phase_templates_drops_index_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_project_phase_templates(tmp.path()).unwrap();
        let phases = tmp.path().join(".ccteam/phases");
        assert!(phases.join("plan-eng.md").exists());
        assert!(phases.join("ship.md").exists());
        assert!(!phases.join("02-plan-eng.md").exists());
    }

    #[test]
    fn write_global_phase_templates_keeps_index_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        let phases = tmp.path().join("phases");
        assert!(phases.join("02-plan-eng.md").exists());
        assert!(phases.join("09-ship.md").exists());
    }

    #[test]
    fn write_global_phase_templates_preserves_user_edits_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        let path = tmp.path().join("phases/02-plan-eng.md");
        std::fs::write(&path, "USER EDITED").unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDITED");
        write_global_phase_templates(tmp.path(), true).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDITED");
    }

    #[test]
    fn render_project_settings_rejects_quoted_path() {
        let err = render_project_settings(
            Path::new("/tmp/has\"quote/ccteam"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("characters"));
    }

    #[test]
    fn render_project_settings_injects_ccteam_env_when_provided() {
        let env = SettingsEnv {
            ccteam_home: Some("/tmp/sandbox/home".into()),
            ccteam_projects_root: Some("/tmp/sandbox/projects".into()),
        };
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &env,
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["env"]["CCTEAM_HOME"], "/tmp/sandbox/home");
        assert_eq!(v["env"]["CCTEAM_PROJECTS_ROOT"], "/tmp/sandbox/projects");
        // Existing env keys must survive the merge.
        assert_eq!(v["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
    }

    #[test]
    fn render_project_settings_omits_env_when_default() {
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["env"]["CCTEAM_HOME"].is_null());
    }

    #[test]
    fn render_project_settings_omits_enabled_plugins_when_empty() {
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("enabledPlugins").is_none());
    }

    #[test]
    fn render_project_settings_writes_enabled_plugins_when_present() {
        let mut enabled = EnabledPluginsSetting::default();
        enabled
            .plugin_ids
            .insert("pr-review-toolkit@claude-plugins-official".into());
        enabled
            .plugin_ids
            .insert("feature-dev@claude-plugins-official".into());
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
            &SettingsEnv::default(),
            &enabled,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let map = v["enabledPlugins"].as_object().expect("enabledPlugins object");
        assert_eq!(map.len(), 2);
        assert_eq!(map["pr-review-toolkit@claude-plugins-official"], true);
        assert_eq!(map["feature-dev@claude-plugins-official"], true);
    }
}
