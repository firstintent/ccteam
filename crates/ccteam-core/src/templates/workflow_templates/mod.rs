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

/// One agent entry the `chat-squad` preset materializes under the
/// `agents:` map. Held by [`render_agents_block`] so callers can stamp
/// out an N-agent block instead of being capped at the legacy 3-slot
/// `role_a` / `role_b` / `role_c` placeholders.
#[derive(Debug, Clone)]
pub struct AgentTemplateEntry {
    /// Role name (will become the `agents.<role>` key). Must match the
    /// `[a-z0-9_-]` charset enforced by `WorkflowSpec::validate`.
    pub role: String,
}

impl AgentTemplateEntry {
    /// Convenience constructor for the common case (manual trigger,
    /// max_parallel=1 — the only shape the chat-squad preset emits).
    pub fn new(role: impl Into<String>) -> Self {
        Self { role: role.into() }
    }
}

/// Render a YAML fragment to fill the `{{agents_block}}` slot in
/// `chat-squad.yaml`. Each entry produces three lines under a 2-space
/// indent (matching the outer `agents:` key). The output always ends
/// with a trailing newline so the template's following section starts
/// on a fresh line.
///
/// Empty input is intentionally rendered as the empty string — the
/// caller is responsible for asserting at least one agent before
/// hitting `WorkflowSpec::validate`. The workflow schema enforces the
/// agents-non-empty rule for artifact-driven / human-approval modes and
/// the chat-mode allow-empty rule.
pub fn render_agents_block(agents: &[AgentTemplateEntry]) -> String {
    let mut out = String::with_capacity(agents.len() * 64);
    for entry in agents {
        out.push_str("  ");
        out.push_str(&entry.role);
        out.push_str(":\n    trigger: manual\n    max_parallel: 1\n");
    }
    out
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

/// V0.6.6 F167 — overlay `ProjectProbe`-derived sensible defaults onto a
/// `TemplateCtx`. Today it sets `scope_yaml` (a small YAML fragment the
/// preset templates embed under `agents.<role>` when present) based on
/// `probe.probable_scope`. Existing `scope_yaml` overrides from the
/// caller win — this is a *default seed*, never a clobber.
///
/// Templates that do not reference `scope_yaml` (chat-pocket /
/// chat-squad — chat bots scope is naturally the project root) leave
/// the key unused; render() is strict, so we only inject keys the
/// templates actually consume.
pub fn apply_probe_defaults(
    ctx: &mut TemplateCtx,
    preset: Preset,
    probe: &super::project_probe::ProjectProbe,
) {
    // Build a YAML fragment for the `agents.<role>.scope:` slot. We
    // emit a *single* scope for the role-bearing preset slots — for
    // monorepos with multiple member crates, we pick the first
    // probable_scope (top-LOC), matching the V0.6.2 F140 "one scope
    // per role" semantics. The skill / user can hand-edit later if
    // they want per-role splits.
    let scope_yaml = probe
        .probable_scope
        .first()
        .map(|p| format!("    scope: {}\n", p.display()))
        .unwrap_or_default();

    match preset {
        Preset::BgOvernight => {
            // Clobber only if the current value is the empty-default
            // sentinel — preserves any explicit user-supplied scope.
            let current = ctx.vars.get("scope_yaml").cloned().unwrap_or_default();
            if current.trim().is_empty() {
                ctx.vars.insert("scope_yaml".into(), scope_yaml.clone());
            }
        }
        Preset::InprocTeam | Preset::InprocSolo => {
            // agent-team mode renders agents: {} — scope lives on the
            // per-role suggested_teammates instead. Future: extend.
            // For now, leave a hint comment in the persona section
            // via persona_label_hint (skill can format).
            let _ = scope_yaml;
        }
        Preset::ChatPocket | Preset::ChatSquad => {
            // chat bots run at project root by design — no scope.
        }
    }
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
        Preset::BgOvernight => {
            // V0.6.6 F167 — `scope_yaml` is an empty placeholder by
            // default; `apply_probe_defaults` overlays a real
            // `scope: <path>` line when a project probe is available.
            ctx = ctx.with("scope_yaml", "");
        }
        Preset::ChatPocket => {
            ctx = ctx
                .with("primary_role", "tech-helper")
                .with("bot_handle", "@demo_bot")
                .with("im_platform", "telegram")
                .with("owner_chat_id", "123456789");
        }
        Preset::ChatSquad => {
            // Use 2 agents so the schema gate exercises the N-agent
            // render path (not just the legacy 3-slot shape).
            let agents = [
                AgentTemplateEntry::new("lead"),
                AgentTemplateEntry::new("critic"),
            ];
            ctx = ctx
                .with("primary_bot_handle", "@lead_bot")
                .with("bot_handles_summary", "@lead_bot, @critic_bot")
                .with("im_platform", "telegram")
                .with("group_chat_id", "-100123456789")
                .with("agents_block", render_agents_block(&agents));
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
        // V0.6.6 F167: default scope_yaml is empty (no probe overlaid).
        assert!(
            !out.contains("scope:"),
            "default render must not embed a scope until probe overlay applies"
        );
    }

    #[test]
    fn bg_overnight_applies_probe_scope_overlay() {
        use super::super::project_probe::{Language, ProjectKind, ProjectProbe};
        use std::path::PathBuf;

        let mut ctx = default_ctx(Preset::BgOvernight);
        let probe = ProjectProbe {
            kind: ProjectKind::SingleRepo,
            languages: vec![Language::Rust],
            has_tests: true,
            probable_scope: vec![PathBuf::from("src"), PathBuf::from("tests")],
        };
        apply_probe_defaults(&mut ctx, Preset::BgOvernight, &probe);
        let out = render(Preset::BgOvernight, &ctx).unwrap();
        assert!(
            out.contains("    scope: src"),
            "expected `scope: src` after probe overlay, got:\n{out}"
        );
        assert!(!out.contains("{{"));
    }

    #[test]
    fn apply_probe_defaults_is_idempotent_and_does_not_clobber() {
        use super::super::project_probe::{Language, ProjectKind, ProjectProbe};
        use std::path::PathBuf;

        let mut ctx =
            default_ctx(Preset::BgOvernight).with("scope_yaml", "    scope: custom-dir\n");
        let probe = ProjectProbe {
            kind: ProjectKind::SingleRepo,
            languages: vec![Language::Rust],
            has_tests: false,
            probable_scope: vec![PathBuf::from("src")],
        };
        apply_probe_defaults(&mut ctx, Preset::BgOvernight, &probe);
        let out = render(Preset::BgOvernight, &ctx).unwrap();
        // User-provided scope_yaml wins (entry().or_insert_with semantics).
        assert!(
            out.contains("scope: custom-dir"),
            "explicit ctx override must win over probe defaults:\n{out}"
        );
        assert!(!out.contains("scope: src"));
    }

    #[test]
    fn chat_pocket_renders_with_defaults() {
        let out = render(Preset::ChatPocket, &default_ctx(Preset::ChatPocket)).unwrap();
        assert!(out.contains("mode: chat"));
        assert!(out.contains("bot_name: \"@demo_bot\""));
        // `im_platform` lives only in the header comment now; the
        // `chat:` block has no such field (ChatSpec doesn't define one).
        assert!(out.contains("# IM platform: telegram"));
        assert!(out.contains("compact_every_turns: 20"));
        // `chat_acl` is a struct (`allow_users` / `allow_groups`), not
        // a list — verify the rendered shape matches ChatAcl.
        assert!(out.contains("allow_users:"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn chat_squad_renders_with_defaults() {
        let out = render(Preset::ChatSquad, &default_ctx(Preset::ChatSquad)).unwrap();
        assert!(out.contains("mode: chat"));
        assert!(out.contains("hop_limit: 3"));
        assert!(out.contains("@lead_bot"));
        // N-agent block: default ctx ships two agents (lead + critic).
        assert!(out.contains("  lead:\n    trigger: manual"));
        assert!(out.contains("  critic:\n    trigger: manual"));
        // `chat_acl` reshaped into the struct form.
        assert!(out.contains("allow_groups:"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn render_agents_block_emits_two_space_indent_and_trailing_newline() {
        let block = render_agents_block(&[
            AgentTemplateEntry::new("alpha"),
            AgentTemplateEntry::new("beta"),
        ]);
        assert_eq!(
            block,
            "  alpha:\n    trigger: manual\n    max_parallel: 1\n  beta:\n    trigger: manual\n    max_parallel: 1\n",
        );
    }

    #[test]
    fn render_agents_block_empty_input_returns_empty_string() {
        assert_eq!(render_agents_block(&[]), "");
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
