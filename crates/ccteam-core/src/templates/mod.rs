//! V0.4.0 F60 — embedded ccteam project bootstrap templates.
//!
//! Pre-F60 this module also held the shipped phase template payloads
//! (`PHASE_TEMPLATES`, `TEAM_BUNDLES`, `write_*_phase_templates`,
//! `write_all_global_team_templates`). Those went with the rest of
//! the phase machinery; F66 reintroduces a much smaller bootstrap path
//! against the new `workflow.yaml` schema.
//!
//! What survives F60:
//! - the per-project settings template + render helper — produced-project
//!   hook plumbing has not changed shape, so we still ship the same
//!   scaffold; as of v0.8.6 W2b the render is written to the **local**
//!   layer `.claude/settings.local.json` so it never dirties the user's
//!   committed `.claude/settings.json`;
//! - the helper-template writer (`write_global_helper_templates`) —
//!   helper markdown is referenced by user-authored agent prompts via
//!   `@~/.ccteam/templates/<name>` and is not coupled to phases.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

// V0.6.0 F111 — per-project `.mcp.json` template + merge helper.
pub mod project_mcp_json;
pub use project_mcp_json::{
    merge_project_mcp_json, render_project_mcp_json, CCTEAM_MCP_SERVER_KEY, CCTEAM_MCP_SERVE_ARGS,
};

// V0.6.0 Wave 2 F114 — `ccteam-creator` preset workflow.yaml templates.
// Five presets (inproc-solo / inproc-team / bg-overnight / chat-pocket /
// chat-squad) rendered via dep-free `{{var}}` substitution. The skill
// picks a preset from `mode_inferrer` + persona, fills `TemplateCtx`,
// calls `render(preset, &ctx)`, then writes the result to
// `<project>/.ccteam/workflow.yaml`.
pub mod workflow_templates;
pub use workflow_templates::{
    apply_probe_defaults as apply_probe_defaults_to_workflow_ctx,
    default_ctx as default_workflow_ctx, render as render_workflow_template,
    render_agents_block as render_workflow_agents_block, AgentTemplateEntry as WorkflowAgentEntry,
    Preset as WorkflowPreset, RenderError as WorkflowTemplateRenderError,
    TemplateCtx as WorkflowTemplateCtx,
};

// V0.6.6 F167 note: there is no separate `render_workflow_template_with_probe`
// function. Callers build the ctx, then call
// `apply_probe_defaults_to_workflow_ctx(&mut ctx, preset, &probe)` to
// overlay sensible defaults, then call `render_workflow_template` as
// usual. This keeps the render entry point single + strict, and lets
// callers stack their own ctx overrides between the probe overlay and
// the render call.

// V0.6.6 F167 — project-type probe for `/ccteam-creator` sensible
// defaults. Surfaces ProjectKind / languages / probable_scope from a
// file-existence sweep so the skill's PROJECT PLAN can pre-populate
// the rendered workflow.yaml's `scope:` field instead of shipping a
// generic stub the user has to hand-edit before first run.
pub mod project_probe;
pub use project_probe::{probe as probe_project, Language, ProjectKind, ProjectProbe};

/// Per-project ccteam settings template.
///
/// v0.8.6 W2b: rendered output is written to the **local** settings layer
/// `<project>/.claude/settings.local.json` (see [`write_project_settings`])
/// so it never dirties the user's committed `.claude/settings.json`. The
/// `include_str!` source below is still the in-crate `settings.json`
/// scaffold file (template payload, not an output path).
///
/// V0.6.1 F139: hook commands route through a single shell wrapper
/// (`__CCTEAM_HOOK_SH__`, materialized at `~/.ccteam/hooks/hook.sh` by
/// `ccteam init` / `ccteam doctor --install-hooks`). The wrapper POSTs
/// the hook stdin to the long-running ccteam daemon's
/// `/internal/hook/:kind[/:action]` route so per-hook latency drops from
/// ~200 ms (cold Rust binary spawn) to ~10 ms (curl round-trip). The
/// wrapper falls back to `ccteam internal hook ...` when the daemon is
/// down so the behaviour matches the pre-F139 path. Pre-F139 templates
/// substituted `__CCTEAM_BIN__` with the absolute `ccteam` binary path;
/// the `rewrite_legacy_hook_commands` rewriter (`tool_surface.rs`) +
/// `ccteam doctor --migrate-hook-commands` rewrite older renders into
/// the new shape.
pub const PROJECT_SETTINGS_JSON: &str = include_str!("settings.json");

/// v8.3 session=role: the default `cto` persona seeded into every
/// project's `.claude/agents/cto.md` by `ccteam init` + core
/// `bootstrap_project_at_dir`. IM/chat sessions launch
/// `claude --agent cto` by default, so this file must exist in every
/// ccteam-created project. Single source for both seed paths.
pub const CTO_ROLE_MD: &str = include_str!("cto_role.md");

/// M2.4: helper templates that user-authored agent / workflow markdown
/// can `@`-reference. Shipped inside the binary so a fresh install (or
/// `ccteam doctor`) can stamp them into `~/.ccteam/templates/` without
/// an external git checkout.
///
/// `(on_disk_filename, body)` — markdown references them as
/// `@~/.ccteam/templates/<on_disk_filename>` and Claude Code's native
/// `@` mechanism inlines the body at prompt-build time. The orchestrator
/// does not parse these — they're pure Claude Code surface.
///
/// V0.5.0 F101: the V0.2-era helpers `review-with-user-loop.md` and
/// `kickoff-reverse-interview.md` were removed alongside the phase-era
/// meta-agent reshape. memory_bridge_*.md lives in `memory_bridge.rs`
/// and is written to `~/.claude/rules/`, not `~/.ccteam/templates/`,
/// so it does not belong in this list. The list stays as a single
/// const so future helpers can be added back in one place.
pub const HELPER_TEMPLATES: &[(&str, &str)] = &[];

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

/// Optional `enabledPlugins` map for the rendered settings.json.
///
/// Each entry is the `<plugin>@<marketplace>` key Claude Code's plugin
/// pipeline expects.
#[derive(Debug, Default, Clone)]
pub struct EnabledPluginsSetting {
    pub plugin_ids: std::collections::BTreeSet<String>,
}

/// Render `PROJECT_SETTINGS_JSON` with `__CCTEAM_HOOK_SH__` replaced by
/// the given absolute path to the `ccteam` hook dispatcher
/// (`~/.ccteam/hooks/hook.sh` in a real install), `extra_env` merged
/// into the top-level `env` block, and `enabled` written under
/// `enabledPlugins`. Validates that the rewritten body is still valid
/// JSON so a path with shell-hostile characters can't silently corrupt
/// the settings file.
///
/// V0.6.1 F139 swap: the V0.4.6 `__CCTEAM_BIN__` placeholder (substituted
/// with the absolute ccteam binary path) is retired. Hook commands now
/// invoke a thin shell wrapper that routes through the long-running
/// daemon's HTTP server (`POST /internal/hook/...`) with a CLI fallback,
/// shaving ~190 ms per hook firing.
pub fn render_project_settings(
    hook_sh: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    render_settings_template(PROJECT_SETTINGS_JSON, hook_sh, extra_env, enabled)
}

/// Shared implementation for [`render_project_settings`].
fn render_settings_template(
    template: &str,
    hook_sh: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    let hook = hook_sh
        .to_str()
        .ok_or_else(|| anyhow!("ccteam hook.sh path not valid UTF-8: {}", hook_sh.display()))?;
    if hook.contains('"') || hook.contains('\\') {
        return Err(anyhow!(
            "ccteam hook.sh path contains characters that can't be embedded in settings.json: {hook}"
        ));
    }
    let body = template.replace("__CCTEAM_HOOK_SH__", hook);
    let mut v: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.json is not valid JSON (hook_sh={hook})"))?;
    if let Some(env) = v.get_mut("env").and_then(|e| e.as_object_mut()) {
        if let Some(home) = &extra_env.ccteam_home {
            env.insert("CCTEAM_HOME".into(), Value::String(home.clone()));
        }
        if let Some(proj) = &extra_env.ccteam_projects_root {
            env.insert("CCTEAM_PROJECTS_ROOT".into(), Value::String(proj.clone()));
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

/// Write `<project_dir>/.claude/settings.local.json` with hook commands
/// pointing at the running ccteam binary by absolute path, the
/// effective `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` baked into `env`,
/// and the spawned-session `enabledPlugins` set. Creates the parent
/// dir if missing. Idempotent — re-running after a ccteam upgrade
/// refreshes the ccteam-managed keys (paths + plugin set) in place.
///
/// v0.8.6 W2b — this targets the **local** settings layer
/// (`settings.local.json`), not the user-committed `settings.json`, so
/// the fresh-project ccteam base config never dirties the user's repo.
///
/// v0.8.6 review-fix — re-init / in-place init **merges** the rendered
/// ccteam base config into any pre-existing `settings.local.json`
/// instead of clobbering it. ccteam *owns* (and overwrites) the
/// top-level `hooks` block and the specific nested keys it renders into
/// `env` / `permissions` / `enabledPlugins`; it *preserves* every
/// user-authored top-level key, plus any nested `env` / `permissions` /
/// `enabledPlugins` key ccteam does not render. See
/// [`merge_ccteam_settings`].
pub fn write_project_settings(project_dir: &Path, enabled: &EnabledPluginsSetting) -> Result<()> {
    write_settings_template(project_dir, enabled, ProjectSettingsKind::ArtifactDriven)
}

/// Discriminator for settings template writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectSettingsKind {
    ArtifactDriven,
}

fn write_settings_template(
    project_dir: &Path,
    enabled: &EnabledPluginsSetting,
    kind: ProjectSettingsKind,
) -> Result<()> {
    let dir = project_dir.join(".claude");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    // v0.8.6 W2b — ccteam writes its fresh-project base config to the
    // **local** settings layer so the user's committed
    // `.claude/settings.json` is never touched.
    let path = dir.join("settings.local.json");
    let hook_sh = effective_hook_sh_path()?;
    let extra = SettingsEnv {
        ccteam_home: std::env::var("CCTEAM_HOME").ok(),
        ccteam_projects_root: std::env::var("CCTEAM_PROJECTS_ROOT").ok(),
    };
    let body = match kind {
        ProjectSettingsKind::ArtifactDriven => render_project_settings(&hook_sh, &extra, enabled)?,
    };
    // v0.8.6 review-fix — merge the rendered ccteam base config into the
    // existing settings.local.json (read + merge + write), mirroring
    // `ensure_chat_hooks_installed`, so an in-place / re-init never
    // destroys the user's own gitignored local settings.
    let rendered: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.local.json is not valid JSON for {kind:?}"))?;
    let mut root: Value = if path.exists() {
        let existing =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        // A user file that is missing or corrupt JSON degrades to a fresh
        // object rather than aborting the init (same policy as the hook
        // installer); ccteam-managed keys are then written on top.
        serde_json::from_str(&existing).unwrap_or_else(|_| Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };
    merge_ccteam_settings(&mut root, &rendered)?;
    let merged =
        serde_json::to_string_pretty(&root).context("serialize merged settings.local.json")?;
    std::fs::write(&path, merged).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Top-level keys whose *nested* members ccteam renders into the
/// settings template. For these we deep-merge — ccteam overwrites only
/// the nested keys it produces (e.g. `env.CCTEAM_HOME`,
/// `permissions.allow`, a given `enabledPlugins` id) and leaves every
/// other nested key the user authored untouched. The top-level `hooks`
/// block is *not* listed here: ccteam owns it wholesale and replaces it
/// (matching `ensure_chat_hooks_installed`, which the chat path layers
/// on top afterwards).
const CCTEAM_DEEP_MERGE_KEYS: &[&str] = &["env", "permissions", "enabledPlugins"];

/// Merge the freshly-`rendered` ccteam settings into `root` (the user's
/// existing — possibly empty — `settings.local.json`) in place.
///
/// ccteam **owns** (overwrites): the top-level `hooks` block, plus the
/// individual nested keys it renders under `env` / `permissions` /
/// `enabledPlugins`. ccteam **preserves**: every user-authored top-level
/// key not present in the template, and every nested `env` /
/// `permissions` / `enabledPlugins` key ccteam does not render.
fn merge_ccteam_settings(root: &mut Value, rendered: &Value) -> Result<()> {
    let dst = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.local.json root is not a JSON object"))?;
    let src = rendered
        .as_object()
        .ok_or_else(|| anyhow!("rendered ccteam settings root is not a JSON object"))?;
    for (key, src_val) in src {
        if CCTEAM_DEEP_MERGE_KEYS.contains(&key.as_str()) {
            // Deep-merge: copy each rendered nested key over the user's,
            // creating the parent object if the user had none.
            if let Some(src_obj) = src_val.as_object() {
                let dst_entry = dst
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Default::default()));
                match dst_entry.as_object_mut() {
                    Some(dst_obj) => {
                        for (nested_key, nested_val) in src_obj {
                            dst_obj.insert(nested_key.clone(), nested_val.clone());
                        }
                    }
                    // User wrote a non-object under this key — ccteam
                    // takes it over rather than silently dropping its
                    // managed nested keys.
                    None => {
                        dst.insert(key.clone(), src_val.clone());
                    }
                }
                continue;
            }
            // Rendered value is unexpectedly not an object — overwrite.
            dst.insert(key.clone(), src_val.clone());
        } else {
            // ccteam owns this top-level key wholesale (e.g. `hooks`).
            dst.insert(key.clone(), src_val.clone());
        }
    }
    Ok(())
}

/// V0.6.1 F139 — resolve the absolute path to `~/.ccteam/hooks/hook.sh`
/// honouring `CCTEAM_HOME` (test seam + ops override). Used by
/// [`write_project_settings`] so freshly-rendered settings.local.json
/// files point at the dispatcher actually materialized on disk by
/// `ccteam init` / `ccteam doctor --install-hooks`.
fn effective_hook_sh_path() -> Result<std::path::PathBuf> {
    let paths = crate::CcteamPaths::from_env()?;
    Ok(paths.hooks_script())
}

/// M2.4 / V0.5.0 F101: ensure `<global_dir>/templates/` exists + write
/// any embedded payloads in [`HELPER_TEMPLATES`] so user-authored
/// markdown's `@~/.ccteam/templates/<filename>` reference resolves.
/// Idempotent; `force == false` preserves operator hand-edits.
///
/// V0.5.0 F101 emptied `HELPER_TEMPLATES`, so the writer now just
/// guarantees the directory exists. Future helpers added back to the
/// list will be stamped here automatically.
pub fn write_global_helper_templates(global_dir: &Path, force: bool) -> Result<()> {
    let dir = global_dir.join("templates");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (filename, body) in HELPER_TEMPLATES {
        let path = dir.join(filename);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
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
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
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
    fn template_session_start_routes_through_hook_sh() {
        // V0.6.1 F139: hook commands now route through the
        // `~/.ccteam/hooks/hook.sh` wrapper (HTTP-to-daemon + CLI
        // fallback) instead of cold-spawning `ccteam internal hook ...`
        // every firing.
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
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
                "/home/u/.ccteam/hooks/hook.sh load-context",
                "/home/u/.ccteam/hooks/hook.sh progress-append session_start"
            ],
        );
    }

    #[test]
    fn raw_template_uses_hook_sh_placeholder() {
        assert!(
            PROJECT_SETTINGS_JSON.contains("__CCTEAM_HOOK_SH__"),
            "template should reference __CCTEAM_HOOK_SH__ placeholder (V0.6.1 F139)",
        );
        // V0.4.6 F89 / V0.6.1 F139: legacy bare `ccteam hook` forms
        // must not survive the migration.
        assert!(
            !PROJECT_SETTINGS_JSON.contains("\"ccteam hook"),
            "template should not embed bare `ccteam hook`",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains(" ccteam hook"),
            "template must not embed bare `ccteam hook` form",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains("__CCTEAM_BIN__"),
            "V0.4.6 `__CCTEAM_BIN__` placeholder was retired by F139",
        );
        // F44 sweep guard: F39's `{{CCT_BIN}}` placeholder must not return.
        assert!(
            !PROJECT_SETTINGS_JSON.contains("{{CCT_BIN}}"),
            "template should not embed F39-era `{{CCT_BIN}}` placeholder (F44)",
        );
    }

    #[test]
    fn render_project_settings_rejects_quoted_path() {
        let err = render_project_settings(
            Path::new("/tmp/has\"quote/hook.sh"),
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
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
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
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
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
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("enabledPlugins").is_none());
    }

    #[test]
    fn merge_ccteam_settings_preserves_user_keys() {
        // Pre-seed an existing settings.local.json with user-authored
        // top-level keys plus user-authored nested keys under the
        // deep-merge keys ccteam touches. The merge must keep all of them
        // while still stamping ccteam-managed config on top.
        let existing = serde_json::json!({
            "env": { "USER_X": "1", "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "user-was-here" },
            "permissions": { "ask": ["Bash(rm:*)"] },
            "model": "opus",
            "statusLine": { "type": "command", "command": "my-status" }
        });
        let rendered = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv {
                ccteam_home: Some("/tmp/sandbox/home".into()),
                ccteam_projects_root: None,
            },
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let rendered: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        let mut root = existing;
        merge_ccteam_settings(&mut root, &rendered).unwrap();

        // User-authored top-level keys ccteam does not own SURVIVE.
        assert_eq!(root["model"], "opus");
        assert_eq!(root["statusLine"]["command"], "my-status");
        // User-authored nested env key SURVIVES.
        assert_eq!(root["env"]["USER_X"], "1");
        // User-authored nested permissions key SURVIVES.
        assert_eq!(root["permissions"]["ask"][0], "Bash(rm:*)");
        // ccteam-managed nested keys are present (and overwrite where they
        // collide on a key ccteam owns).
        assert_eq!(root["env"]["CCTEAM_HOME"], "/tmp/sandbox/home");
        assert_eq!(
            root["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1",
            "ccteam owns this env key and overwrites the user's stale value",
        );
        // ccteam widens permissions via the valid `defaultMode` (newer Claude
        // Code rejects a bare `"*"` in an `allow` rule). ccteam-spawned sessions
        // pass `--dangerously-skip-permissions` / `--permission-mode default` on
        // the CLI (which override settings), so this only governs manual runs.
        assert_eq!(root["permissions"]["defaultMode"], "bypassPermissions");
        // ccteam owns the hooks block wholesale.
        assert!(root["hooks"]["SessionStart"].is_array());
    }

    #[test]
    fn write_project_settings_merges_into_existing_local_file() {
        // Full end-to-end: an in-place re-init over a project that already
        // has a user-authored settings.local.json must NOT destroy it.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        let claude_dir = project.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.local.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "env": { "USER_X": "1" },
                "permissions": { "ask": ["Bash(rm:*)"] },
                "model": "opus"
            }))
            .unwrap(),
        )
        .unwrap();

        write_project_settings(project, &EnabledPluginsSetting::default())
            .expect("in-place write must succeed");

        let body = std::fs::read_to_string(&settings_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        // User keys SURVIVE the re-init (top-level + nested).
        assert_eq!(v["env"]["USER_X"], "1");
        assert_eq!(v["model"], "opus");
        assert_eq!(v["permissions"]["ask"][0], "Bash(rm:*)");
        // ccteam-managed keys are present.
        assert_eq!(v["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
        assert!(v["hooks"]["SessionStart"].is_array());
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
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &enabled,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let map = v["enabledPlugins"]
            .as_object()
            .expect("enabledPlugins object");
        assert_eq!(map.len(), 2);
        assert_eq!(map["pr-review-toolkit@claude-plugins-official"], true);
        assert_eq!(map["feature-dev@claude-plugins-official"], true);
    }

    /// The seeded `cto` role MUST NOT restrict its tools via a frontmatter
    /// `tools:` line. Omitting it makes the spawned session inherit ALL tools
    /// (Claude Code treats a missing `tools:` as the wildcard), so every
    /// `mcp__ccteam__ccteam__*` tool is available ambiently via `.mcp.json` —
    /// the cto needs no per-agent enumeration. (The earlier template DID carry
    /// such a line, but listed the handles as single-`ccteam`
    /// `mcp__ccteam__session_*`, which never matched the real double-`ccteam`
    /// names and so granted nothing.) The cto's scheduling PRIVILEGE is
    /// enforced by the daemon `(role, secret)` gate, and the scheduling tool
    /// SET is guarded by `cto_scheduling_tools_present_in_canonical_set` in
    /// `mcp_session_tools` — neither lives in this fragile frontmatter.
    #[test]
    fn cto_role_template_does_not_restrict_tools() {
        // Frontmatter is the block between the first two `---` fences.
        let after = CTO_ROLE_MD
            .strip_prefix("---")
            .expect("cto_role.md starts with frontmatter fence");
        let end = after.find("---").expect("frontmatter closing fence");
        let frontmatter = &after[..end];
        assert!(
            !frontmatter
                .lines()
                .any(|l| l.trim_start().starts_with("tools:")),
            "cto_role.md frontmatter must NOT carry a restrictive `tools:` line \
             (omit it so the session inherits all tools, incl. ambient \
             mcp__ccteam__ccteam__* via .mcp.json); the cto's scheduling \
             privilege is enforced by the daemon (role, secret) gate, not by a \
             per-agent allow-list"
        );
    }

    #[test]
    fn cto_role_template_has_fresh_user_guidance() {
        // Whitespace-normalized so the assertions check for the GUIDANCE, not the
        // prose line-wrapping (a needle must not silently fail when a sentence
        // happens to wrap across two lines).
        let body = CTO_ROLE_MD.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "New user asking",
            "ccteam config",
            "ccteam start",
            "/pair <code>",
            "/cd <project>",
            "roleless = bare Claude",
            "`cto` = the default steward",
            "work-role = a specialist in `.claude/agents/<role>.md`",
        ] {
            assert!(
                body.contains(needle),
                "cto_role.md missing fresh-user guidance {needle:?}"
            );
        }
    }

    #[test]
    fn cto_role_template_describes_session_spawn_as_fresh_sid() {
        assert!(
            CTO_ROLE_MD.contains("mints a NEW sid"),
            "cto_role.md must say session_spawn mints a fresh sid"
        );
        assert!(
            !CTO_ROLE_MD.contains("(project, role) dedup") && !CTO_ROLE_MD.contains("idempotent"),
            "cto_role.md must not describe removed (project, role) dedup"
        );
    }
}
