//! V0.6.0 F115 — spawn-brief template rendering.
//!
//! Tiny `{{token}}` replacer used by the orchestrator + agent-team lead
//! when materializing a kicker prompt. Keeps the rendering layer
//! distinct from data (`workflow::SuggestedTeammate::spawn_brief` is
//! the raw user-authored string; this module is where the
//! `{{include_prev_handoffs}}` directive gets expanded).
//!
//! Supported tokens (V0.6.0 wave 2):
//!
//! | Token | Source | Notes |
//! |---|---|---|
//! | `{{workflow_slug}}` | [`SpawnContext::workflow_slug`] | identity |
//! | `{{role}}` | [`SpawnContext::role`] | identity |
//! | `{{stage_num}}` | [`SpawnContext::stage_num`] | blank when None |
//! | `{{include_prev_handoffs}}` | [`crate::handoff::read_concat`] | last 3 by default |
//!
//! Unknown `{{tokens}}` are **left intact** so unrelated template
//! syntax (handlebars / mustache / mid-prompt examples) survives a
//! pass through this renderer. F115's contract is "expand the four
//! tokens above; don't touch anything else".
//!
//! ## Why not bring in a templating engine?
//!
//! Handlebars / tera adds 200+ KB to the binary + a runtime parse
//! step. The whole F115 token set is a static `Vec<&str>`; a manual
//! `.replace()` chain is more transparent and lets the orchestrator
//! short-circuit the expensive `read_concat` when the token isn't
//! present.

use std::path::PathBuf;

use anyhow::Result;

use crate::handoff;

/// Inputs the renderer needs to expand spawn-brief tokens.
///
/// All fields are owned strings so callers can build a context once
/// (per spawn) and pass it across `await` points without lifetimes.
#[derive(Debug, Clone)]
pub struct SpawnContext {
    /// Absolute path of the project dir — used to locate
    /// `<project>/.ccteam/handoffs/<workflow-slug>/`.
    pub project_dir: PathBuf,
    /// Workflow slug; in ccteam this is the project slug since each
    /// project owns exactly one `workflow.yaml`.
    pub workflow_slug: String,
    /// Role of the agent being spawned (for `{{role}}`).
    pub role: String,
    /// Optional 1-indexed stage number. Renders `{{stage_num}}` as
    /// `"<N>"` when `Some`, empty string when `None`.
    pub stage_num: Option<u32>,
    /// How many prior handoff docs to splice into
    /// `{{include_prev_handoffs}}`. Defaults to
    /// [`handoff::DEFAULT_INCLUDE_LAST_N`] when constructed via
    /// [`SpawnContext::new`].
    pub include_last_n_handoffs: usize,
}

impl SpawnContext {
    /// Convenience constructor with sensible defaults
    /// (`stage_num=None`, `include_last_n_handoffs=DEFAULT_INCLUDE_LAST_N`).
    pub fn new(project_dir: PathBuf, workflow_slug: String, role: String) -> Self {
        Self {
            project_dir,
            workflow_slug,
            role,
            stage_num: None,
            include_last_n_handoffs: handoff::DEFAULT_INCLUDE_LAST_N,
        }
    }
}

/// Render a spawn-brief template by expanding the four supported
/// tokens. Idempotent over inputs without tokens (returns `template`
/// unchanged on the hot path).
pub fn render_spawn_brief(template: &str, ctx: &SpawnContext) -> Result<String> {
    // Hot-path: nothing to expand. Avoids an unnecessary allocation +
    // (more importantly) the disk read inside `read_concat`.
    if !template.contains("{{") {
        return Ok(template.to_string());
    }

    let mut out = template.to_string();

    if out.contains("{{include_prev_handoffs}}") {
        let handoffs = handoff::read_concat(
            &ctx.project_dir,
            &ctx.workflow_slug,
            ctx.include_last_n_handoffs,
        )?;
        out = out.replace("{{include_prev_handoffs}}", &handoffs);
    }

    if out.contains("{{workflow_slug}}") {
        out = out.replace("{{workflow_slug}}", &ctx.workflow_slug);
    }

    if out.contains("{{role}}") {
        out = out.replace("{{role}}", &ctx.role);
    }

    if out.contains("{{stage_num}}") {
        let val = ctx.stage_num.map(|n| n.to_string()).unwrap_or_default();
        out = out.replace("{{stage_num}}", &val);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_path_returns_template_unchanged() {
        let ctx = SpawnContext::new(
            PathBuf::from("/tmp/does/not/exist"),
            "slug".into(),
            "explorer".into(),
        );
        let s = render_spawn_brief("plain prompt with no tokens", &ctx).unwrap();
        assert_eq!(s, "plain prompt with no tokens");
    }

    #[test]
    fn unknown_tokens_pass_through() {
        let ctx = SpawnContext::new(
            PathBuf::from("/tmp/does/not/exist"),
            "slug".into(),
            "explorer".into(),
        );
        let s = render_spawn_brief("{{custom_thing}} ok {{role}}", &ctx).unwrap();
        assert_eq!(s, "{{custom_thing}} ok explorer");
    }
}
