//! V0.2 M0.17.3 — Three-layer team config resolution
//! (PRD §5.2.1, alignment review §4: "Layered settings" pattern).
//!
//! Mirrors Claude Code's `SETTING_SOURCES` enum design: a small
//! ordered list of sources, **first-source-wins**, integer-team
//! granularity (no field-level merge — `dev` at the project layer
//! completely replaces `dev` at the user layer).
//!
//! ## Sources
//!
//! 1. **Project** — `<project_dir>/.ccteam/team/team.yaml`. A
//!    per-project override. Returns `None` when the resolver is
//!    invoked without a project context (the orchestrator's
//!    startup-time `discover_all` walk).
//! 2. **User** — `~/.config/ccteam/teams/<name>/team.yaml` (staging
//!    layout for the V0.2 M0.22 team factory). V0.3 will also walk
//!    `~/.claude/plugins/marketplaces/*/plugins/<team>/team.yaml`
//!    once the team-plugin install path lands.
//! 3. **Repo** — `<global_dir>/teams/<name>/team.yaml`. Shipped seed
//!    teams (dev / product-research / meta-agent) live here after
//!    `write_all_global_team_templates`.
//!
//! ## Read tolerance vs. write strictness
//!
//! Reading a yaml that fails to parse logs a warn and falls through
//! to the next source. Reading a missing file silently falls through.
//! Writing (`save_team`) is strict: a parse failure of the existing
//! file at the target layer fails the write so the operator can
//! diagnose before the bad yaml gets clobbered.
//!
//! ## Caching
//!
//! V0.2 M0.17 deliberately ships **no** in-memory cache — `cargo
//! test --workspace` baseline measures a few thousand `TeamSpec::load`
//! calls per process and disk yaml parse is sub-millisecond. A
//! per-source cache + explicit invalidation is documented as V0.3
//! follow-up if profiling shows it matters under real load.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::team::TeamSpec;

/// Ordered list of team sources. Resolution walks this list and
/// returns the first hit. Mirrors Claude Code's `SETTING_SOURCES`.
pub const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,
    TeamSource::User,
    TeamSource::Repo,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamSource {
    Project,
    User,
    Repo,
}

impl TeamSource {
    /// Disk path the source resolves to for `name`. Returns `None`
    /// when the source is inapplicable in this context (e.g. Project
    /// without a project_dir).
    pub fn path_for(&self, name: &str, ctx: &TeamResolveContext) -> Option<PathBuf> {
        match self {
            TeamSource::Project => ctx
                .project_dir
                .map(|p| p.join(".ccteam").join("team").join("team.yaml")),
            TeamSource::User => Some(
                ctx.user_staging_dir
                    .join("teams")
                    .join(name)
                    .join("team.yaml"),
            ),
            TeamSource::Repo => Some(
                ctx.global_dir
                    .join("teams")
                    .join(name)
                    .join("team.yaml"),
            ),
        }
    }

    /// Attempt to load `name`'s spec from this source. Returns:
    /// - `Ok(Some(spec))` when the file exists and parses cleanly,
    /// - `Ok(None)` when the file doesn't exist or the source
    ///   doesn't apply (lets the caller fall through),
    /// - `Err(_)` when the file exists but failed to parse (the
    ///   caller logs a warn and falls through — see `resolve_team`).
    pub fn try_load(
        &self,
        name: &str,
        ctx: &TeamResolveContext,
    ) -> Result<Option<TeamSpec>> {
        let Some(path) = self.path_for(name, ctx) else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let spec = TeamSpec::load(&path)
            .with_context(|| format!("load team `{name}` from {self:?} ({})", path.display()))?;
        // The Project source's yaml may declare a different `name` if
        // a project pins a renamed local override. We trust the
        // resolver caller; downstream code keys by the *requested*
        // name, not the spec's `name` field.
        Ok(Some(spec))
    }
}

/// Resolution context — paths the sources need to compute their
/// targets. Construct via `TeamResolveContext::for_orchestrator`
/// (no project) or `with_project(project_dir)`.
#[derive(Debug, Clone)]
pub struct TeamResolveContext<'a> {
    /// `~/.ccteam/` (or test-injected equivalent).
    pub global_dir: &'a Path,
    /// `~/.config/ccteam/` — V0.2 staging dir for user-authored
    /// teams. Resolved from `XDG_CONFIG_HOME` (or `$HOME/.config`)
    /// at process startup.
    pub user_staging_dir: &'a Path,
    /// `~/projects/<slug>/` for project-aware lookups; `None` for
    /// orchestrator startup discovery.
    pub project_dir: Option<&'a Path>,
}

impl<'a> TeamResolveContext<'a> {
    /// Builder convenience: orchestrator startup context (no project).
    pub fn for_orchestrator(global_dir: &'a Path, user_staging_dir: &'a Path) -> Self {
        Self {
            global_dir,
            user_staging_dir,
            project_dir: None,
        }
    }

    /// Builder convenience: per-project context.
    pub fn with_project(mut self, project_dir: &'a Path) -> Self {
        self.project_dir = Some(project_dir);
        self
    }
}

/// First-source-wins resolution. Yaml parse errors **do not** fail
/// the resolution — they get logged and the loop falls through to
/// the next source. ENOENT is silent. Returns `Err` only when no
/// source carried `name`.
///
/// V0.2.2 F40 — alias scan: when no source carries a directory named
/// `name`, walk every team yaml at every source and match by
/// `spec.aliases`. Lets old projects whose `state.json::team` carries
/// a legacy name (e.g. `product-research` → `research`) still resolve
/// against the renamed shipped team.
pub fn resolve_team(name: &str, ctx: &TeamResolveContext) -> Result<TeamSpec> {
    for source in TEAM_SOURCES {
        match source.try_load(name, ctx) {
            Ok(Some(spec)) => return Ok(spec),
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!(
                    team = %name,
                    source = ?source,
                    error = format!("{err:#}"),
                    "team source unreadable; falling through to next layer",
                );
                continue;
            }
        }
    }
    if let Some(spec) = resolve_by_alias(name, ctx) {
        return Ok(spec);
    }
    Err(anyhow!(
        "team `{name}` not found in any source (project / user / repo)"
    ))
}

/// V0.2.2 F40 — second-pass alias resolution. Walks every team yaml
/// under each source's `teams/` directory and returns the first spec
/// whose `aliases` list contains `query`. Project source is skipped:
/// the project layer is a single fixed yaml at
/// `<project>/.ccteam/team/team.yaml`, which `try_load` already
/// covered in pass one.
fn resolve_by_alias(query: &str, ctx: &TeamResolveContext) -> Option<TeamSpec> {
    for dir in [
        ctx.user_staging_dir.join("teams"),
        ctx.global_dir.join("teams"),
    ] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let yaml = entry.path().join("team.yaml");
            if !yaml.exists() {
                continue;
            }
            let spec = match TeamSpec::load(&yaml) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(
                        path = %yaml.display(),
                        error = format!("{err:#}"),
                        "alias scan: skipping unreadable team.yaml",
                    );
                    continue;
                }
            };
            if spec.aliases.iter().any(|a| a == query) {
                return Some(spec);
            }
        }
    }
    None
}

/// Enumerate the distinct team names discoverable across User + Repo
/// sources. Project source is intentionally skipped — without a
/// project context "what teams exist" is a global question.
///
/// Used by the orchestrator's startup walk so it can call
/// `resolve_team` for each name and build the `TeamRuntime` map.
pub fn discover_team_names(ctx: &TeamResolveContext) -> HashSet<String> {
    let mut out = HashSet::new();
    for dir in [
        ctx.user_staging_dir.join("teams"),
        ctx.global_dir.join("teams"),
    ] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if !entry.path().join("team.yaml").exists() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str().map(String::from) {
                out.insert(name);
            }
        }
    }
    out
}

/// Strict write — refuses to overwrite a yaml that doesn't currently
/// parse, so an operator hand-edit gone wrong gets surfaced before
/// the next save clobbers it.
///
/// Always writes to the User layer's path (V0.2 default — Project
/// layer writes are V0.3, Repo layer writes happen via the seed
/// helpers in `templates.rs`).
pub fn save_team(spec: &TeamSpec, ctx: &TeamResolveContext) -> Result<PathBuf> {
    let path = TeamSource::User
        .path_for(&spec.name, ctx)
        .ok_or_else(|| anyhow!("user staging dir unavailable"))?;
    if path.exists() {
        // Pre-write parse check: if the existing yaml is broken,
        // refuse the overwrite so the operator can investigate.
        TeamSpec::load(&path).with_context(|| {
            format!(
                "refusing to overwrite unreadable team yaml at {} — \
                 inspect / fix it before re-saving",
                path.display(),
            )
        })?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_yaml::to_string(spec)
        .context("serialize TeamSpec to yaml")?;
    std::fs::write(&path, body)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Resolve the user staging dir from XDG conventions:
/// `$XDG_CONFIG_HOME` if set, else `$HOME/.config`. Falls back to
/// the global dir's parent when neither is available — this keeps
/// the resolver functional in container test environments where
/// `$HOME` is unset.
pub fn default_user_staging_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("ccteam");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("ccteam");
    }
    // Fallback: a deterministic but unusable path so callers fail
    // loudly when they actually try to read/write.
    PathBuf::from("/tmp/ccteam-staging-fallback")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_yaml(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn ctx_for<'a>(global: &'a Path, user: &'a Path) -> TeamResolveContext<'a> {
        TeamResolveContext::for_orchestrator(global, user)
    }

    #[test]
    fn repo_layer_resolves_when_only_repo_yaml_exists() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\n",
        );
        let ctx = ctx_for(&global, &user);
        let spec = resolve_team("dev", &ctx).unwrap();
        assert_eq!(spec.name, "dev");
    }

    #[test]
    fn user_layer_wins_over_repo_when_both_present() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: from repo\n",
        );
        write_yaml(
            &user.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: from user\n",
        );
        let ctx = ctx_for(&global, &user);
        let spec = resolve_team("dev", &ctx).unwrap();
        assert_eq!(spec.description, "from user");
    }

    #[test]
    fn project_layer_wins_over_user_and_repo() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        let project = tmp.path().join("projects").join("foo");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: from repo\n",
        );
        write_yaml(
            &user.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: from user\n",
        );
        write_yaml(
            &project.join(".ccteam").join("team").join("team.yaml"),
            "name: dev\ndescription: from project\n",
        );
        let mut ctx = ctx_for(&global, &user);
        ctx.project_dir = Some(&project);
        let spec = resolve_team("dev", &ctx).unwrap();
        assert_eq!(spec.description, "from project");
    }

    #[test]
    fn malformed_user_yaml_warns_and_falls_back_to_repo() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: from repo\n",
        );
        write_yaml(
            &user.join("teams").join("dev").join("team.yaml"),
            "this: is: { not valid: yaml\n",
        );
        let ctx = ctx_for(&global, &user);
        let spec = resolve_team("dev", &ctx).unwrap();
        assert_eq!(spec.description, "from repo");
    }

    #[test]
    fn missing_team_returns_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        let ctx = ctx_for(&global, &user);
        let err = resolve_team("missing", &ctx).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("missing"));
        assert!(msg.contains("project / user / repo"));
    }

    #[test]
    fn discover_team_names_unions_user_and_repo_layers() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\n",
        );
        write_yaml(
            &global.join("teams").join("product-research").join("team.yaml"),
            "name: product-research\n",
        );
        write_yaml(
            &user.join("teams").join("custom").join("team.yaml"),
            "name: custom\n",
        );
        let ctx = ctx_for(&global, &user);
        let names = discover_team_names(&ctx);
        assert!(names.contains("dev"));
        assert!(names.contains("product-research"));
        assert!(names.contains("custom"));
    }

    #[test]
    fn discover_team_names_dedupes_when_team_exists_in_both_layers() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\n",
        );
        write_yaml(
            &user.join("teams").join("dev").join("team.yaml"),
            "name: dev\ndescription: user override\n",
        );
        let ctx = ctx_for(&global, &user);
        let names = discover_team_names(&ctx);
        assert_eq!(names.len(), 1);
        assert!(names.contains("dev"));
    }

    #[test]
    fn save_team_writes_to_user_layer() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        let mut spec = TeamSpec::parse("name: my-team\n").unwrap();
        spec.description = "saved via resolver".into();
        let ctx = ctx_for(&global, &user);
        let written = save_team(&spec, &ctx).unwrap();
        assert!(written.starts_with(&user));
        let reloaded = TeamSpec::load(&written).unwrap();
        assert_eq!(reloaded.description, "saved via resolver");
    }

    // V0.2.2 F40 — alias scan path. The renamed `research` team's
    // yaml lists `aliases: [product-research]`; old projects whose
    // state.json::team still points to `product-research` must
    // resolve through the second-pass alias scan when no
    // `teams/product-research/team.yaml` lives on disk anymore.

    #[test]
    fn resolve_team_falls_through_to_alias_scan_when_no_directory_match() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("research").join("team.yaml"),
            "name: research\naliases: [product-research]\n",
        );
        let ctx = ctx_for(&global, &user);
        // Direct hit on canonical name.
        let canonical = resolve_team("research", &ctx).unwrap();
        assert_eq!(canonical.name, "research");
        // Alias hit via the second-pass scan.
        let via_alias = resolve_team("product-research", &ctx).unwrap();
        assert_eq!(via_alias.name, "research");
        assert_eq!(via_alias.aliases, vec!["product-research".to_string()]);
    }

    #[test]
    fn resolve_team_alias_scan_ignores_unrelated_teams() {
        // Two teams on disk; only one declares the alias the caller
        // asks for. Make sure the scan returns the right one and
        // doesn't fall through to a sibling.
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("dev").join("team.yaml"),
            "name: dev\n",
        );
        write_yaml(
            &global.join("teams").join("research").join("team.yaml"),
            "name: research\naliases: [product-research]\n",
        );
        let ctx = ctx_for(&global, &user);
        let spec = resolve_team("product-research", &ctx).unwrap();
        assert_eq!(spec.name, "research");
    }

    #[test]
    fn resolve_team_unknown_alias_still_errors() {
        // A name that's not on disk and not in any aliases list must
        // still fail with the helpful "not found in any source" message.
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        write_yaml(
            &global.join("teams").join("research").join("team.yaml"),
            "name: research\naliases: [product-research]\n",
        );
        let ctx = ctx_for(&global, &user);
        let err = resolve_team("nonexistent-team", &ctx).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("nonexistent-team"));
        assert!(msg.contains("project / user / repo"));
    }

    #[test]
    fn save_team_refuses_to_overwrite_unreadable_yaml() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("ccteam-home");
        let user = tmp.path().join("user");
        let dst = user.join("teams").join("custom").join("team.yaml");
        write_yaml(&dst, "this: is: { broken yaml\n");

        let mut spec = TeamSpec::parse("name: custom\n").unwrap();
        spec.description = "trying to clobber".into();
        let ctx = ctx_for(&global, &user);
        let err = save_team(&spec, &ctx).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("refusing to overwrite"));
        // The pre-existing broken yaml stays intact.
        let body = std::fs::read_to_string(&dst).unwrap();
        assert!(body.contains("broken yaml"));
    }
}
