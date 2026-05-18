//! V0.6.0 Wave 2 F114 — pre-baked `workflow.yaml` templates the
//! `ccteam-creator` skill renders during Phase 5 execute (one template
//! per preset; preset chosen by [`mode_inferrer`](crate::mode_inferrer)
//! + persona pick).
//!
//! Five presets, mirroring the V0.6.0 PRD F114 table:
//!
//! - `inproc-solo`    — Solo Sidekick (1 in-process teammate)
//! - `inproc-team`    — Team Sprint (N in-process teammates)
//! - `bg-overnight`   — Overnight Builder (artifact-driven, daemon)
//! - `chat-pocket`    — Pocket Assistant (single bot DM)
//! - `chat-squad`     — IM Squad (N bots in group room)
//!
//! The render layer is intentionally **dependency-free** — we use plain
//! `{{var}}` substitution rather than pulling in tera / handlebars. The
//! templates are short (≤ 60 lines each) and only a handful of
//! placeholders matter; pulling a full template engine into ccteam-core
//! for this would be over-engineered. If complex conditionals become
//! necessary in V0.7+, swap to handlebars at that point.
//!
//! ## Placeholder convention
//!
//! - `{{workflow_slug}}` — kebab-case identifier (matches workflow.yaml `name`)
//! - `{{persona_label}}` — human-readable persona name (from manifest.toml)
//! - `{{user_brief}}`    — verbatim user request, used as lead_seed
//! - `{{primary_role}}`  — the canonical role name (e.g. `tech-helper`)
//! - `{{bot_handle}}`    — `@`-prefixed IM handle (chat presets only)
//! - `{{im_platform}}`   — `telegram` / `slack` / `discord` (chat only)
//!
//! Unknown placeholders left untouched are a [`RenderError::MissingPlaceholder`]
//! when [`render`] sees a `{{...}}` token that isn't in `ctx.vars`. This
//! is strict-by-default so callers don't silently ship `workflow.yaml`
//! files containing literal `{{x}}` tokens to disk.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One of the five V0.6.0 `ccteam-creator` presets. Maps 1:1 to a
/// `*.yaml` template file in this directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Solo Sidekick — single in-process teammate.
    InprocSolo,
    /// Team Sprint — N in-process teammates, orchestrator-worker.
    InprocTeam,
    /// Overnight Builder — artifact-driven, runs unattended.
    BgOvernight,
    /// Pocket Assistant — single chat bot DM.
    ChatPocket,
    /// IM Squad — multi-bot chat in a group room.
    ChatSquad,
}

impl Preset {
    /// Kebab-case wire name (matches the YAML filename).
    pub fn as_str(self) -> &'static str {
        match self {
            Preset::InprocSolo => "inproc-solo",
            Preset::InprocTeam => "inproc-team",
            Preset::BgOvernight => "bg-overnight",
            Preset::ChatPocket => "chat-pocket",
            Preset::ChatSquad => "chat-squad",
        }
    }

    /// Iterate every preset variant in declaration order. Used by
    /// tests that exercise the full render-set + by the skill UI when
    /// listing options to the user.
    pub fn all() -> &'static [Preset] {
        &[
            Preset::InprocSolo,
            Preset::InprocTeam,
            Preset::BgOvernight,
            Preset::ChatPocket,
            Preset::ChatSquad,
        ]
    }
}

/// Per-preset embedded YAML body. Kept in `include_str!` so a fresh
/// install (no repo checkout) still finds the templates compiled into
/// the binary.
fn template_body(p: Preset) -> &'static str {
    match p {
        Preset::InprocSolo => include_str!("inproc-solo.yaml"),
        Preset::InprocTeam => include_str!("inproc-team.yaml"),
        Preset::BgOvernight => include_str!("bg-overnight.yaml"),
        Preset::ChatPocket => include_str!("chat-pocket.yaml"),
        Preset::ChatSquad => include_str!("chat-squad.yaml"),
    }
}

/// Render context — bag of `{{var}}` substitutions. The keys expected
/// per preset are documented at the module level.
#[derive(Debug, Clone, Default)]
pub struct TemplateCtx {
    pub vars: HashMap<String, String>,
}

impl TemplateCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convenience builder so call sites read more like a struct
    /// literal than a chain of `insert`s.
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }
}

/// Errors returned by [`render`].
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("template references unknown placeholder `{{{{{0}}}}}` (preset={1})")]
    MissingPlaceholder(String, &'static str),
}

/// Render `preset` against `ctx`. Strict — any `{{...}}` token whose
/// key is not in `ctx.vars` produces [`RenderError::MissingPlaceholder`].
///
/// Two-pass scan:
/// 1. Find every `{{key}}` token in the template body.
/// 2. Verify each is present in `ctx.vars`; bail on first miss.
/// 3. Substitute everything in one pass.
pub fn render(preset: Preset, ctx: &TemplateCtx) -> Result<String, RenderError> {
    let body = template_body(preset);
    let mut out = String::with_capacity(body.len() + 256);
    let mut cursor = 0usize;
    let bytes = body.as_bytes();

    while cursor < bytes.len() {
        // Find next `{{`.
        let rem = &body[cursor..];
        if let Some(open) = rem.find("{{") {
            // Append everything before the opener verbatim.
            out.push_str(&rem[..open]);
            let after_open = cursor + open + 2;
            // Find matching `}}`.
            let after_rem = &body[after_open..];
            let close = after_rem.find("}}").ok_or_else(|| {
                // Unbalanced `{{` — treat as a missing-placeholder error
                // with the trailing fragment as the offender so the
                // caller has *something* to grep for.
                RenderError::MissingPlaceholder(after_rem.to_string(), preset.as_str())
            })?;
            let key = after_rem[..close].trim();
            let value = ctx
                .vars
                .get(key)
                .ok_or_else(|| RenderError::MissingPlaceholder(key.to_string(), preset.as_str()))?;
            out.push_str(value);
            cursor = after_open + close + 2;
        } else {
            out.push_str(rem);
            break;
        }
    }
    Ok(out)
}

/// Convenience: build a [`TemplateCtx`] populated with the minimal
/// required keys for each preset, then let the caller layer extra
/// overrides on top via [`TemplateCtx::with`]. Useful for the skill +
/// for tests.
pub fn default_ctx(preset: Preset) -> TemplateCtx {
    let mut ctx = TemplateCtx::new()
        .with("workflow_slug", "demo-workflow")
        .with("persona_label", "demo-persona")
        .with("user_brief", "<describe the task here>");
    match preset {
        Preset::InprocSolo => {
            ctx = ctx
                .with("primary_role", "executor")
                .with("primary_brief", "<per-task instructions>");
        }
        Preset::InprocTeam => {
            ctx = ctx.with("worker_count", "3");
        }
        Preset::BgOvernight => {}
        Preset::ChatPocket => {
            ctx = ctx
                .with("primary_role", "tech-helper")
                .with("bot_handle", "@demo_bot")
                .with("im_platform", "telegram")
                .with("owner_chat_id", "123456789");
        }
        Preset::ChatSquad => {
            ctx = ctx
                .with("primary_bot_handle", "@lead_bot")
                .with("bot_handles_summary", "@lead_bot, @critic_bot, @scribe_bot")
                .with("im_platform", "telegram")
                .with("group_chat_id", "-100123456789")
                .with("role_a", "lead")
                .with("role_b", "critic")
                .with("role_c", "scribe");
        }
    }
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inproc_solo_renders_with_defaults() {
        let out = render(Preset::InprocSolo, &default_ctx(Preset::InprocSolo)).unwrap();
        assert!(out.contains("mode: agent-team"));
        assert!(out.contains("name: demo-workflow"));
        assert!(out.contains("teammate_mode: in-process"));
        assert!(!out.contains("{{"), "unsubstituted placeholder: {out}");
    }

    #[test]
    fn inproc_team_renders_with_defaults() {
        let out = render(Preset::InprocTeam, &default_ctx(Preset::InprocTeam)).unwrap();
        assert!(out.contains("Decompose the task into 3 parallel"));
        assert!(out.contains("role: executor"));
        assert!(out.contains("role: critic"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn bg_overnight_renders_with_defaults() {
        let out = render(Preset::BgOvernight, &default_ctx(Preset::BgOvernight)).unwrap();
        assert!(out.contains("mode: artifact-driven"));
        assert!(out.contains("watch:.ccteam/inbox/executor"));
        assert!(out.contains("max_cost_usd_per_24h"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn chat_pocket_renders_with_defaults() {
        let out = render(Preset::ChatPocket, &default_ctx(Preset::ChatPocket)).unwrap();
        assert!(out.contains("mode: chat"));
        assert!(out.contains("bot_name: @demo_bot"));
        assert!(out.contains("im_platform: telegram"));
        assert!(out.contains("compact_every_turns: 20"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn chat_squad_renders_with_defaults() {
        let out = render(Preset::ChatSquad, &default_ctx(Preset::ChatSquad)).unwrap();
        assert!(out.contains("mode: chat"));
        assert!(out.contains("hop_limit: 3"));
        assert!(out.contains("@lead_bot"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn missing_placeholder_errors() {
        let ctx = TemplateCtx::new().with("workflow_slug", "x");
        let err = render(Preset::InprocSolo, &ctx).unwrap_err();
        assert!(matches!(err, RenderError::MissingPlaceholder(_, _)));
    }

    #[test]
    fn preset_as_str_matches_filename() {
        for &p in Preset::all() {
            // Body lookup should succeed for every variant.
            let body = template_body(p);
            assert!(!body.is_empty(), "preset {} has empty body", p.as_str());
        }
    }
}
