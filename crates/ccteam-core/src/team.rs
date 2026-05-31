//! `team.yaml` schema — team-level configuration that lives next to a
//! team's phase template directory.
//!
//! Two stages of contributions:
//!
//! - **M3.1** shipped name / description / retro_schema. M4.1 retro
//!   phase reads `retro_schema` so retro reports pick up team-specific
//!   fields from day one (no RAG-index rebuild later).
//! - **M3.2** adds critic_dimensions, escalate_grammar_extensions,
//!   golden_rules (team-wide default), phase_dir, verdict_schema.
//!   These let M3.3 swap phase template directories per team and let
//!   M3.4 declare team-specific ESCALATE prefixes without touching
//!   ccteam-core. M5 will read `critic_dimensions`; the field is
//!   data-form-only in M3 (strategic doc §A invariant 1).
//!
//! The orchestrator never hard-codes a team name — phase routing,
//! ESCALATE grammar, and golden-rules enforcement all consult these
//! fields, so adding a team is a configuration change.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

/// One field in a team's retro schema. M4.1 retro phase emits a
/// markdown section per entry; the cross-project memory RAG indexes
/// each field as a tagged document so future projects can pull only
/// the field types relevant to their own team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetroFieldSpec {
    /// snake_case field name. Used as the markdown subsection slug
    /// AND the RAG index tag, so keep it stable per team — renaming
    /// invalidates indexed history.
    pub field: String,
    /// Free-text description shown to the assistant in the retro
    /// phase prompt — explains what to put in this field.
    pub description: String,
    /// `list` for bulleted text, `text` for a single paragraph. M4.1
    /// retro phase formats accordingly.
    #[serde(default = "default_field_kind")]
    pub kind: RetroFieldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroFieldKind {
    /// Bulleted list of short items (tech stack, pitfalls, …).
    List,
    /// Single paragraph (overall summary, narrative).
    Text,
}

fn default_field_kind() -> RetroFieldKind {
    RetroFieldKind::List
}

fn default_phase_dir() -> String {
    "phases".into()
}

/// V0.3.1 F48 — team-level execution mode. Defaults to `workflow` so
/// V0.1 / V0.2 / V0.3 team yamls that omit `kind` parse unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamKind {
    #[default]
    Workflow,
    MultiWorkflow,
    Flex,
}

impl TeamKind {
    pub fn is_flex(self) -> bool {
        matches!(self, Self::Flex)
    }

    pub fn is_phase_driven(self) -> bool {
        matches!(self, Self::Workflow | Self::MultiWorkflow)
    }
}

/// V0.2 M0.18: how a team-level golden rule is enforced.
///
/// - `cmd_check` runs the rule's `cmd` (or matches its `pattern`) at
///   `phase_done` boundary — the historic [`crate::golden_rules`]
///   executor path. Non-zero exit / regex hit = violation.
/// - `prompt_directive` skips runtime enforcement and instead injects
///   the rule's `directive` text into the orchestrator's per-phase
///   inject prompt so the assistant sees it as a hard constraint.
///   Pairs with `domain` rules where the user's intent is "tell the
///   LLM, don't run a check".
///
/// Default `cmd_check` keeps every M3.x team.yaml that listed flat
/// `golden_rules` working — they continue to mean "run this command
/// at phase boundary".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoldenRuleEnforcement {
    /// Run `cmd` / match `pattern` at phase_done boundary; non-zero
    /// exit / regex hit blocks the transition.
    #[default]
    CmdCheck,
    /// Inject `directive` into the phase's inject prompt; no runtime
    /// enforcement — the assistant is expected to honor it.
    PromptDirective,
}

/// V0.2 M0.18: a `protocol` golden rule. Unlike the phase-level
/// [`GoldenRule`] which is strictly cmd | pattern, protocol rules
/// can also carry a `directive` string when `enforce: prompt_directive`
/// — the inject-prompt template renders the directive verbatim.
///
/// Field validation:
/// - `enforce: cmd_check` requires exactly one of `cmd` / `pattern`.
/// - `enforce: prompt_directive` requires `directive` non-empty;
///   `cmd` / `pattern` are tolerated (ignored) but trigger a warn so
///   operators notice the dead field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRule {
    pub rule_id: String,
    #[serde(default)]
    pub enforce: GoldenRuleEnforcement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive: Option<String>,
}

/// V0.2 M0.18: a `domain` golden rule — pure prompt-layer guidance
/// for the assistant ("prefer small PRs", "no SQL string interpolation").
/// Domain rules never run at phase boundary; they're surfaced via
/// the inject prompt. Phase markdown bodies may also reference domain
/// rules implicitly (the user's freedom; doctor doesn't lint bodies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainRule {
    pub rule_id: String,
    pub directive: String,
}

/// V0.2 M0.18: structured team-level golden rules with protocol /
/// domain split (docs/versions/v0-2/phase-prompt-architecture.md §6).
///
/// **Legacy compat**: a flat `Vec<GoldenRule>` in pre-V0.2 yamls is
/// deserialized as `protocol` with `enforce: cmd_check` so M3.x team
/// yamls keep loading. V0.2-shape yamls explicitly key `protocol:` /
/// `domain:`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TeamGoldenRules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol: Vec<ProtocolRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<DomainRule>,
}

impl TeamGoldenRules {
    pub fn is_empty(&self) -> bool {
        self.protocol.is_empty() && self.domain.is_empty()
    }
}

/// V0.2 M0.18 legacy compat shape — pre-V0.2 yamls listed flat
/// `golden_rules` items with `rule_id` + (`cmd` | `pattern`). Kept as
/// a private deserialization-only shape so old team.yaml files still
/// load post-F60 (the phase machinery that used to consume these is
/// gone; the `Deserialize` impl below coerces them into `protocol`
/// with `enforce: cmd_check`).
#[derive(Debug, Clone, Deserialize)]
struct LegacyGoldenRule {
    rule_id: String,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
}

/// V0.2 M0.18: deserialize either the new structured shape
/// (`protocol: [...]` / `domain: [...]`) or the legacy flat list
/// (`Vec<LegacyGoldenRule>` — coerced into `protocol` with
/// `enforce: cmd_check`).
impl<'de> Deserialize<'de> for TeamGoldenRules {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as DeError;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Structured {
            #[serde(default)]
            protocol: Vec<ProtocolRule>,
            #[serde(default)]
            domain: Vec<DomainRule>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Structured(Structured),
            Legacy(Vec<LegacyGoldenRule>),
        }

        match Either::deserialize(d).map_err(D::Error::custom)? {
            Either::Structured(Structured { protocol, domain }) => {
                Ok(TeamGoldenRules { protocol, domain })
            }
            Either::Legacy(list) => Ok(TeamGoldenRules {
                protocol: list
                    .into_iter()
                    .map(|r| ProtocolRule {
                        rule_id: r.rule_id,
                        enforce: GoldenRuleEnforcement::CmdCheck,
                        cmd: r.cmd,
                        pattern: r.pattern,
                        directive: None,
                    })
                    .collect(),
                domain: Vec::new(),
            }),
        }
    }
}

/// M3.2: per-dimension Critic configuration (strategic doc §2.3).
/// **Data form only in M3** — M5 Critic loop will consume this; the
/// orchestrator never reads it before M5 (strategic doc §A invariant 1
/// forbids putting dimension names in `match` arms).
///
/// All fields default so an M3 team.yaml can ship without any
/// `critic_dimensions:` block — M5 will start adding them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticDimensionSpec {
    /// snake_case name. Used as the dimension identifier in M5
    /// scoring output and as the RAG tag for cross-project memory.
    pub name: String,
    /// Weight in the aggregate score. Sum across dimensions does not
    /// have to be 1.0 — M5 may normalize.
    #[serde(default)]
    pub weight: f64,
    /// `≤ weak_threshold` in any one dimension forces a fix-cycle
    /// (M5; strategic doc §2.3 invariant 3 — config-driven, not const).
    #[serde(default)]
    pub weak_threshold: f64,
    /// per-dimension anti-leniency strictness (strategic doc §2.3
    /// invariant 2). dev's six dims default to `normal`; research's
    /// core dims need `strict` because LLM-only judgment is too lenient
    /// without an objective tie-breaker.
    #[serde(default)]
    pub anti_leniency_strictness: CriticStrictness,
    /// Free-text rubric shown to the M5 Critic prompt.
    #[serde(default)]
    pub rubric: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CriticStrictness {
    /// At least one comment of any level passes anti-leniency.
    Lenient,
    /// At least one CONCERN or BLOCK passes anti-leniency.
    #[default]
    Normal,
    /// Must have at least one BLOCK to pass anti-leniency.
    Strict,
}

/// M3.2: a team-specific ESCALATE prefix the Stop hook should treat as
/// a known grammar element (interfaces §4.1.1). Without registration
/// in `team.yaml.escalate_grammar_extensions`, the Stop hook still
/// degrades the prefix to `NEED_USER_INPUT` (the legacy bare-text
/// fallback), so this is a **declaration**, not an enforcement gate.
///
/// strategic doc §1.6: prefix is team semantics; the routing table is
/// mechanism. The `route` field selects which built-in `kind` the
/// orchestrator uses to handle the escalate event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateGrammarExtension {
    /// e.g. `MARKET_DUPLICATE` / `HYPOTHESIS_REJECTED`. Free-form
    /// uppercase by convention; the Stop hook matches on this exact
    /// substring after `ESCALATE:`.
    pub prefix: String,
    /// Built-in routing kind this prefix maps to. M3 supports the
    /// three pre-existing kinds (revert / need_user_input / abort);
    /// teams cannot invent new kinds without an orchestrator change.
    pub route: EscalateRoute,
    /// For `route: revert_to_phase` only — phase name to revert to.
    /// Ignored otherwise.
    #[serde(default)]
    pub target_phase: Option<String>,
    /// Human-readable description used in the orchestrator's
    /// escalation.md and surfaced to the user.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalateRoute {
    /// Mirror `ESCALATE: REVERT_TO_PHASE <target> — ...`.
    RevertToPhase,
    /// Mirror `ESCALATE: NEED_USER_INPUT — ...`.
    NeedUserInput,
    /// Mirror `ESCALATE: ABORT — ...` (project terminally failed).
    Abort,
}

/// V0.2 §6.4 candidate 5: cost-handling policy. Replaces the
/// hardcoded `if state.team == META_TEAM_NAME { skip }` branch in
/// `enforce_cost_thresholds` with a declarative per-team flag. Two
/// variants cover the cases V0.2 has consumers for:
///
/// - `None` — no cost tracking, no warnings, no kill. Used by
///   evergreen teams (meta-agent) where "cost" is the user's running
///   tab, not a per-project budget.
/// - `KillAt(usd)` — current dev / product-research behavior: soft +
///   mid warnings, hard-kill the tmux session when
///   `state.cost_used_usd > usd`. The threshold is per-team, not
///   per-project, so a team author sets a sensible default and the
///   orchestrator uses it unless `state.hard_kill_threshold_usd`
///   overrides at the project level.
///
/// PRD §6.4 originally listed a third `Track` variant (warn but
/// don't kill) for V0.3 watchdog agents. Dropped on review (PR #15
/// 2026-05-07 fixup): V0.3 will define the watchdog cost behavior
/// concretely (warn thresholds, soft-warn factor, etc.) — adding
/// `Track` then with real consumers beats shipping a marker variant
/// today.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "threshold_usd")]
pub enum CostPolicy {
    /// No tracking, no warnings, no kill. Evergreen sessions
    /// (meta-agent) — cost is the user's running tab, not a per-project
    /// budget.
    None,
    /// Soft + mid warnings + hard-kill at threshold. Default for
    /// regular phase-DAG teams. Falls back to
    /// `state.hard_kill_threshold_usd` (default $200) when this
    /// variant's threshold is `None`.
    KillAt(Option<f64>),
}

impl Default for CostPolicy {
    fn default() -> Self {
        // Phase-DAG teams default to the historical KillAt path so a
        // team.yaml that omits `cost_policy` keeps M3 behavior intact.
        // Evergreen teams (meta-agent) override to `None`.
        Self::KillAt(None)
    }
}

/// V0.3.1 F47 — which LLM harness backs a session. Mapped 1:1 to
/// `ccteam_harness::HarnessAdapter` impls; `Claude` →
/// [`ccteam_harness::ClaudeCodeAdapter`], `Codex` →
/// [`ccteam_harness::CodexAdapter`] (stub, V0.3.2 fills it). The serde
/// rename is `lowercase` so `team.yaml` stays user-readable
/// (`harness: claude` / `harness: codex`) without mentioning the
/// internal `claude-code` adapter name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    /// Anthropic's Claude Code TUI — V0.3.1 default + only fully
    /// supported harness. Backed by [`ccteam_harness::ClaudeCodeAdapter`].
    /// `#[default]` because Claude Code is the only real harness in
    /// V0.3.1; F48 PR adds the `kind: flex` team layer that consumes
    /// this default; F49 fills the master state.json::sessions wiring.
    #[default]
    Claude,
    /// OpenAI's `codex` CLI. V0.3.1 ships the trait stub
    /// ([`ccteam_harness::CodexAdapter`]); real spawn / ingest /
    /// shutdown lands in V0.3.2 (PRD §F47, see also
    /// `docs/research/ccteam-codex-integration.md`).
    Codex,
}

/// V0.3.1 F47 — one entry in `team.yaml::sessions[]`. Declares a
/// default session the team factory should bootstrap when an operator
/// runs `ccteam new --team <flex-team>`. Meaningful only for `kind:
/// flex` teams; workflow / multi_workflow teams ignore the
/// field but parse it without error so a hand-rolled flex team yaml
/// can carry its own bootstrap defaults today even before F48 lands.
///
/// Fields:
/// - `sid` — session id slug (`"claude-1"`, `"codex-1"`, …). Must be
///   unique within the team yaml (validation lands with F49 when the
///   field becomes load-bearing); F47 only checks the schema parses.
/// - `harness` — which [`HarnessKind`] backs this session. Defaults
///   to [`HarnessKind::Claude`] so a `sessions: [{sid: claude-1}]`
///   entry without an explicit `harness:` key still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultSessionSpec {
    /// Session id slug. Used by F49 to derive the tmux session name
    /// (`ccteam-<slug>-<sid>`) and the `<harness-dir>/<slug>-<sid>.json`
    /// dual-write target.
    pub sid: String,
    /// Backing harness. Defaults to [`HarnessKind::Claude`] so a
    /// minimal `{sid: claude-1}` entry round-trips.
    #[serde(default)]
    pub harness: HarnessKind,
}

/// `team.yaml` — the team-level config. M3.1 shipped name /
/// description / retro_schema; M3.2 adds the fields below so the
/// orchestrator can route phases / ESCALATE prefixes / golden-rule
/// enforcement per team without ccteam-core knowing the team name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSpec {
    /// Team identifier. Must match the `--team` arg / `state.json.team`
    /// field. snake-case lowercase — gets used as a directory name.
    pub name: String,

    /// V0.2.2 F40 — soft-rename aliases. Old team identifiers that
    /// still appear in user data (`state.json::team`, project
    /// directories like `~/projects/<old-name>-*`) resolve to this
    /// canonical spec via [`crate::team_resolver::resolve_team`] /
    /// [`crate::orchestrator::Orchestrator::team_runtime`]. Used to
    /// rename a shipped team without touching user data — old projects
    /// keep working, new projects pick up the canonical name.
    ///
    /// Each alias must be unique within the team and follow the same
    /// `[a-z0-9_-]+` charset as `name`. Defaults to empty so
    /// pre-V0.2.2 yamls keep parsing.
    #[serde(default)]
    pub aliases: Vec<String>,

    /// V0.3.1 F48 — team kind. Defaults to [`TeamKind::Workflow`].
    /// `flex` teams have no phase DAG: no auto-loop, phase prompt
    /// injection, or golden-rule check. Observability stays on.
    #[serde(default)]
    pub kind: TeamKind,

    /// Human-readable one-liner; surfaced by `ccteam ls --teams`
    /// (M3.4) and in error messages.
    #[serde(default)]
    pub description: String,

    /// Field schema for the retro phase (M4.1). Empty list = no
    /// retro phase for this team. Order is preserved — the retro
    /// markdown emits sections in this order.
    #[serde(default)]
    pub retro_schema: Vec<RetroFieldSpec>,

    // ---------------- M3.2 fields ----------------
    /// Critic dimensions consumed by M5. Data-form-only in M3 — the
    /// orchestrator doesn't read this before M5 (strategic §A inv 1).
    #[serde(default)]
    pub critic_dimensions: Vec<CriticDimensionSpec>,

    /// Team-specific ESCALATE prefixes (interfaces §4.1.1). The Stop
    /// hook learns the prefix list at orchestrator startup, so adding
    /// a prefix is a config change, not an ccteam-core change.
    #[serde(default)]
    pub escalate_grammar_extensions: Vec<EscalateGrammarExtension>,

    /// Team-wide default `golden_rules` (V0.2 M0.18: structured —
    /// `protocol` / `domain` split, docs/versions/v0-2/phase-prompt-architecture.md §6).
    /// Phase YAML's `golden_rules` takes priority — phases that
    /// declare their own rules ignore the team default entirely (no
    /// merge, see strategic doc §3.4 "不预设质量评分维度"). Empty =
    /// team has no default rules.
    ///
    /// Legacy (M3.x) yamls with a flat list of `{rule_id, cmd|pattern}`
    /// entries deserialize as `protocol` with `enforce: cmd_check` so
    /// existing team yamls keep working (see [`TeamGoldenRules`]).
    #[serde(default)]
    pub golden_rules: TeamGoldenRules,

    /// Phase template directory **relative to the team directory**
    /// (V0.2 M0.17.2). For shipped teams that's
    /// `~/.ccteam/teams/<name>/<phase_dir>/`; user-authored teams in
    /// `~/.config/ccteam/teams/<name>/<phase_dir>/` follow the same
    /// pattern. Defaults to `phases` so a fresh team yaml just works.
    /// Project-local templates live in `<project>/.ccteam/phases/` —
    /// that path is independent of `phase_dir`.
    ///
    /// **Legacy compat**: yamls from M3.x ship with values like
    /// `phases-product-research` — those refer to the old "relative
    /// to ~/.ccteam/" semantic. `TeamSpec::load` (and parse-time
    /// `validate`) detects the legacy `phases-` prefix and rewrites
    /// to `phases` with a warn so the new layout works without manual
    /// migration of user yamls.
    #[serde(default = "default_phase_dir")]
    pub phase_dir: String,

    /// Phase names that produce a `verdict:` document (interfaces §5.3).
    /// Used by M3.4 product-research to flag the `verdict` phase as
    /// the one whose output drives PASS/CONCERN/REJECT/CLARIFY routing.
    /// dev currently leaves this empty.
    #[serde(default)]
    pub verdict_schema: Vec<String>,

    // ---------------- V0.2 M0.16 fields (§6.4 candidate 5) ----------------
    /// Evergreen sessions never reach a phase-DAG terminal state and
    /// don't follow the AdvancePhase / DispatchPhase loop — they're
    /// event-loop sessions waiting for inbox messages. The orchestrator
    /// dispatches them through `process_meta_project` instead of
    /// `process_project`. Defaults to false so phase-DAG teams behave
    /// unchanged.
    ///
    /// Replaces `if state.team == META_TEAM_NAME` branches in
    /// `process_project` / `count_active_regular` / `tick`. Any
    /// user-authored team can opt in (e.g. V0.3 watchdog / reviewer
    /// agents) without ccteam-core changes.
    #[serde(default)]
    pub evergreen: bool,

    /// Cost-handling policy (§6.4 candidate 5). Defaults to
    /// `KillAt(None)` so phase-DAG teams keep current behavior;
    /// evergreen teams (meta-agent) opt to `None`. See `CostPolicy`
    /// for variant semantics.
    #[serde(default)]
    pub cost_policy: CostPolicy,

    /// Auto-managed `<project>/CLAUDE.md` body. Replaces the hardcoded
    /// `match team` in `projects::render_project_claude_md` (§6.2
    /// candidate 2). `{slug}` and `{team}` placeholders are substituted
    /// at bootstrap time; everything else lands verbatim.
    ///
    /// Empty string keeps a generic fallback body so a team.yaml
    /// without `claude_md_template` still bootstraps.
    #[serde(default)]
    pub claude_md_template: String,

    // ---------------- V0.3.1 F47 fields ----------------
    /// V0.3.1 F47/F48 — default session list for `kind: flex` teams.
    /// Each entry declares a session the team
    /// factory should bootstrap when an operator runs `ccteam new
    /// --team <flex-team>` so the project's first attach has the
    /// expected harnesses (claude / codex / mixed) already running.
    ///
    /// **Workflow teams ignore this field** — it has no effect on
    /// `dev` / `research` / `meta-agent` / `ccteam-project-creator`
    /// today, but parse-time tolerance lets a flex team yaml ship its
    /// `sessions[]` defaults right now without waiting on F48 / F49.
    ///
    /// Defaults to empty so V0.1 / V0.2 / V0.3 / pre-F48 yamls (which
    /// omit the field entirely) parse unchanged.
    #[serde(default)]
    pub sessions: Vec<DefaultSessionSpec>,
}

impl TeamSpec {
    /// Parse `team.yaml` from raw YAML source. V0.2 M0.17.2: legacy
    /// `phase_dir: phases-<team>` values are rewritten to `phases`
    /// after parse — they're a relic of the pre-M0.17 "relative to
    /// ~/.ccteam/" layout. `validate()` then runs against the
    /// rewritten spec.
    pub fn parse(source: &str) -> Result<Self> {
        let mut spec: TeamSpec =
            serde_yaml::from_str(source).context("team.yaml does not match schema")?;
        spec.normalize_legacy_phase_dir();
        spec.validate()?;
        Ok(spec)
    }

    /// Load + parse `team.yaml` from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read team.yaml at {}", path.display()))?;
        Self::parse(&source).with_context(|| format!("parse team.yaml at {}", path.display()))
    }

    /// V0.2 M0.17.2: rewrite legacy `phase_dir: phases-<team>` to
    /// `phases`. The old value comes from M3.x where `phase_dir` was
    /// "relative to ~/.ccteam/" and product-research carried
    /// `phase_dir: phases-product-research` so the two teams' phase
    /// markdowns wouldn't collide on disk. With the M0.17 layout
    /// (`~/.ccteam/teams/<name>/phases/`) the per-team prefix is
    /// redundant — every team's phase dir is `phases` under its own
    /// team dir. Rewrite-on-load keeps user yamls and shipped seeds
    /// from the M3 era working without manual migration.
    fn normalize_legacy_phase_dir(&mut self) {
        if self.phase_dir.starts_with("phases-") {
            let legacy = std::mem::replace(&mut self.phase_dir, "phases".into());
            tracing::warn!(
                team = %self.name,
                legacy = %legacy,
                "rewriting legacy phase_dir to `phases` (V0.2 M0.17.2 layout)",
            );
        }
    }

    /// Sanity checks at parse time. The orchestrator never wants to
    /// hold an "almost OK" TeamSpec — better to fail loud at load
    /// than to discover a duplicate retro field name during M4.1
    /// retro execution.
    ///
    /// Also runs the M3.2 invariants:
    /// - `phase_dir` non-empty, no path traversal (`..`).
    /// - `escalate_grammar_extensions[*].prefix` non-empty, unique,
    ///   and `route: revert_to_phase` carries a `target_phase`.
    /// - `golden_rules[*]` resolves to exactly one of cmd | pattern.
    /// - `verdict_schema[*]` non-empty.
    /// - `critic_dimensions[*].name` non-empty + unique.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("team.yaml: `name` must be non-empty");
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
        {
            bail!(
                "team.yaml: `name` must be ascii lower / digit / `-` / `_`; got `{}`",
                self.name,
            );
        }

        // V0.2.2 F40: aliases follow the same charset rules as `name`,
        // must be unique within the team, and must not collide with the
        // canonical name (an alias === itself is meaningless).
        let mut alias_seen = std::collections::HashSet::new();
        for alias in &self.aliases {
            if alias.trim().is_empty() {
                bail!("team.yaml: alias entry must be non-empty");
            }
            if alias
                .chars()
                .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
            {
                bail!("team.yaml: alias `{alias}` must be ascii lower / digit / `-` / `_`",);
            }
            if alias == &self.name {
                bail!(
                    "team.yaml: alias `{alias}` matches canonical name; \
                     drop it from the aliases list",
                );
            }
            if !alias_seen.insert(alias.as_str()) {
                return Err(anyhow!("team.yaml: duplicate alias `{alias}`"));
            }
        }

        let mut seen = std::collections::HashSet::new();
        for f in &self.retro_schema {
            if f.field.trim().is_empty() {
                bail!("team.yaml: retro_schema entry has empty `field`");
            }
            if !seen.insert(f.field.as_str()) {
                return Err(anyhow!(
                    "team.yaml: retro_schema duplicates field `{}`",
                    f.field,
                ));
            }
        }

        // M3.2: phase_dir non-empty + no traversal for phase-driven teams.
        if self.kind.is_phase_driven() && self.phase_dir.trim().is_empty() {
            bail!("team.yaml: `phase_dir` must be non-empty");
        }
        if self.phase_dir.split('/').any(|seg| seg == "..") {
            bail!(
                "team.yaml: `phase_dir` must not contain `..`; got `{}`",
                self.phase_dir,
            );
        }
        if Path::new(&self.phase_dir).is_absolute() {
            bail!(
                "team.yaml: `phase_dir` must be relative to ccteam root; got `{}`",
                self.phase_dir,
            );
        }
        if self.kind.is_flex()
            && !self.phase_dir.trim().is_empty()
            && self.phase_dir != default_phase_dir()
        {
            bail!(
                "team.yaml: kind=flex must not declare custom `phase_dir` `{}` \
                 (F48 / PRD §5.2.1: flex teams have no phase machinery)",
                self.phase_dir,
            );
        }

        if self.kind.is_flex() {
            if !self.escalate_grammar_extensions.is_empty() {
                bail!(
                    "team.yaml: kind=flex must not declare \
                     `escalate_grammar_extensions` (F48 / PRD §5.2.1: flex \
                     teams have no phase machinery)",
                );
            }
            if !self.golden_rules.is_empty() {
                bail!(
                    "team.yaml: kind=flex must not declare `golden_rules` \
                     (F48 / PRD §5.2.1: flex teams skip golden-rule checks)",
                );
            }
            if !self.retro_schema.is_empty() {
                bail!(
                    "team.yaml: kind=flex must not declare `retro_schema` \
                     (F48 / PRD §5.3: flex retro phase is deferred)",
                );
            }
            if !self.verdict_schema.is_empty() {
                bail!(
                    "team.yaml: kind=flex must not declare `verdict_schema` \
                     (F48 / PRD §5.3: flex verdict phase is deferred)",
                );
            }
        }

        // M3.2: escalate_grammar_extensions invariants.
        let mut prefix_seen = std::collections::HashSet::new();
        for ext in &self.escalate_grammar_extensions {
            if ext.prefix.trim().is_empty() {
                bail!("team.yaml: escalate_grammar_extensions entry has empty `prefix`");
            }
            if !prefix_seen.insert(ext.prefix.as_str()) {
                return Err(anyhow!(
                    "team.yaml: escalate_grammar_extensions duplicates prefix `{}`",
                    ext.prefix,
                ));
            }
            if matches!(ext.route, EscalateRoute::RevertToPhase)
                && ext.target_phase.as_deref().is_none_or(str::is_empty)
            {
                bail!(
                    "team.yaml: escalate prefix `{}` has route revert_to_phase but no target_phase",
                    ext.prefix,
                );
            }
        }

        // M3.2 / V0.2 M0.18: golden_rules.protocol cmd | pattern xor
        // (cmd_check enforcement) or directive non-empty
        // (prompt_directive enforcement).
        let mut seen_protocol_id = std::collections::HashSet::new();
        for rule in &self.golden_rules.protocol {
            if rule.rule_id.trim().is_empty() {
                bail!("team.yaml: golden_rules.protocol entry has empty rule_id");
            }
            if !seen_protocol_id.insert(rule.rule_id.as_str()) {
                return Err(anyhow!(
                    "team.yaml: golden_rules.protocol duplicates rule_id `{}`",
                    rule.rule_id,
                ));
            }
            match rule.enforce {
                GoldenRuleEnforcement::CmdCheck => {
                    // Cmd-check validity: exactly one of `cmd` / `pattern`.
                    // Pre-F60 this leaned on `phases::GoldenRule::kind()`
                    // for the message text; inlined here so team.rs no
                    // longer transitively depends on the deleted module.
                    match (rule.cmd.as_deref(), rule.pattern.as_deref()) {
                        (Some(_), None) | (None, Some(_)) => {}
                        (Some(_), Some(_)) => bail!(
                            "team.yaml: golden_rules.protocol `{}` (cmd_check) has both `cmd` and `pattern`; pick one",
                            rule.rule_id,
                        ),
                        (None, None) => bail!(
                            "team.yaml: golden_rules.protocol `{}` (cmd_check) missing both `cmd` and `pattern`; one is required",
                            rule.rule_id,
                        ),
                    }
                }
                GoldenRuleEnforcement::PromptDirective => {
                    if rule
                        .directive
                        .as_deref()
                        .is_none_or(|s| s.trim().is_empty())
                    {
                        bail!(
                            "team.yaml: golden_rules.protocol `{}` (prompt_directive) requires non-empty `directive`",
                            rule.rule_id,
                        );
                    }
                }
            }
        }
        let mut seen_domain_id = std::collections::HashSet::new();
        for rule in &self.golden_rules.domain {
            if rule.rule_id.trim().is_empty() {
                bail!("team.yaml: golden_rules.domain entry has empty rule_id");
            }
            if !seen_domain_id.insert(rule.rule_id.as_str()) {
                return Err(anyhow!(
                    "team.yaml: golden_rules.domain duplicates rule_id `{}`",
                    rule.rule_id,
                ));
            }
            if rule.directive.trim().is_empty() {
                bail!(
                    "team.yaml: golden_rules.domain `{}` requires non-empty `directive`",
                    rule.rule_id,
                );
            }
        }

        // M3.2: verdict_schema entries non-empty.
        for v in &self.verdict_schema {
            if v.trim().is_empty() {
                bail!("team.yaml: verdict_schema entry must be non-empty");
            }
        }

        // M3.2: critic_dimensions name uniqueness.
        let mut dim_seen = std::collections::HashSet::new();
        for d in &self.critic_dimensions {
            if d.name.trim().is_empty() {
                bail!("team.yaml: critic_dimensions entry has empty `name`");
            }
            if !dim_seen.insert(d.name.as_str()) {
                return Err(anyhow!(
                    "team.yaml: critic_dimensions duplicates name `{}`",
                    d.name,
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_team_yaml() {
        let src = "name: dev\n";
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.name, "dev");
        assert!(spec.description.is_empty());
        assert!(spec.retro_schema.is_empty());
    }

    #[test]
    fn parses_dev_team_retro_schema() {
        let src = concat!(
            "name: dev\n",
            "description: Software development team\n",
            "retro_schema:\n",
            "  - field: tech_stack\n",
            "    description: Languages, frameworks, key libraries\n",
            "  - field: pitfalls\n",
            "    description: Mistakes to avoid next time\n",
            "  - field: successful_designs\n",
            "    description: Design choices that paid off\n",
            "  - field: do_not_do_again\n",
            "    description: Anti-patterns observed\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.name, "dev");
        assert_eq!(spec.retro_schema.len(), 4);
        assert_eq!(spec.retro_schema[0].field, "tech_stack");
        // default kind = list
        assert_eq!(spec.retro_schema[0].kind, RetroFieldKind::List);
    }

    #[test]
    fn parses_research_team_with_text_field() {
        let src = concat!(
            "name: research\n",
            "retro_schema:\n",
            "  - field: methodology\n",
            "    description: Methods used\n",
            "  - field: summary\n",
            "    description: Narrative recap\n",
            "    kind: text\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.retro_schema[1].kind, RetroFieldKind::Text);
    }

    #[test]
    fn rejects_empty_name() {
        let err = TeamSpec::parse("name: ''\n").unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn rejects_invalid_chars_in_name() {
        let err = TeamSpec::parse("name: Dev Team\n").unwrap_err();
        assert!(format!("{err:#}").contains("ascii"));
    }

    #[test]
    fn rejects_duplicate_retro_field() {
        let src = concat!(
            "name: dev\n",
            "retro_schema:\n",
            "  - field: tech_stack\n",
            "    description: First\n",
            "  - field: tech_stack\n",
            "    description: Duplicate\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("duplicates"));
    }

    #[test]
    fn rejects_empty_field_name() {
        let src = concat!(
            "name: dev\n",
            "retro_schema:\n",
            "  - field: ''\n",
            "    description: empty\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("empty"));
    }

    #[test]
    fn round_trip_through_yaml_preserves_fields() {
        let original = TeamSpec {
            name: "dev".into(),
            aliases: Vec::new(),
            kind: TeamKind::Workflow,
            description: "Software dev".into(),
            retro_schema: vec![RetroFieldSpec {
                field: "tech_stack".into(),
                description: "List of techs".into(),
                kind: RetroFieldKind::List,
            }],
            critic_dimensions: Vec::new(),
            escalate_grammar_extensions: Vec::new(),
            golden_rules: TeamGoldenRules::default(),
            phase_dir: "phases".into(),
            verdict_schema: Vec::new(),
            evergreen: false,
            cost_policy: CostPolicy::default(),
            claude_md_template: String::new(),
            sessions: Vec::new(),
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed = TeamSpec::parse(&yaml).unwrap();
        assert_eq!(parsed, original);
    }

    // ---------------- M3.2 schema fields ----------------

    #[test]
    fn m32_phase_dir_defaults_to_phases() {
        let spec = TeamSpec::parse("name: dev\n").unwrap();
        assert_eq!(spec.phase_dir, "phases");
    }

    #[test]
    fn m32_phase_dir_explicit_value_round_trips() {
        // V0.2 M0.17.2: legacy `phases-<team>` values get rewritten to
        // `phases` on parse. A non-legacy custom value still round-trips.
        let src = "name: product-research\nphase_dir: phases\n";
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.phase_dir, "phases");
    }

    #[test]
    fn m017_legacy_phase_dir_rewrites_to_phases() {
        // V0.2 M0.17.2: M3.x yamls used phase_dir relative to
        // ~/.ccteam/, with values like `phases-product-research` to
        // avoid collision. Under the M0.17 layout each team's phase
        // dir lives inside the team's own directory, so the per-team
        // prefix is redundant — rewrite-on-load keeps stale yamls
        // working.
        let src = "name: product-research\nphase_dir: phases-product-research\n";
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.phase_dir, "phases");
    }

    #[test]
    fn m32_phase_dir_rejects_path_traversal() {
        let err = TeamSpec::parse("name: x\nphase_dir: ../escape\n").unwrap_err();
        assert!(format!("{err:#}").contains(".."));
    }

    #[test]
    fn m32_phase_dir_rejects_absolute_path() {
        let err = TeamSpec::parse("name: x\nphase_dir: /etc/phases\n").unwrap_err();
        assert!(format!("{err:#}").contains("relative"));
    }

    #[test]
    fn m32_phase_dir_rejects_empty() {
        let err = TeamSpec::parse("name: x\nphase_dir: ''\n").unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn m32_escalate_grammar_extensions_parse() {
        let src = concat!(
            "name: product-research\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: MARKET_DUPLICATE\n",
            "    route: abort\n",
            "    reason: target market saturated, idea duplicates an existing free tool\n",
            "  - prefix: INSUFFICIENT_VALIDATION\n",
            "    route: need_user_input\n",
            "    reason: cannot collect a 3rd primary source within the round budget\n",
            "  - prefix: LOW_DIFFERENTIATION\n",
            "    route: revert_to_phase\n",
            "    target_phase: differentiation-analysis\n",
            "    reason: differentiation gap too small for value-prop\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.escalate_grammar_extensions.len(), 3);
        let ldiff = &spec.escalate_grammar_extensions[2];
        assert_eq!(ldiff.prefix, "LOW_DIFFERENTIATION");
        assert_eq!(ldiff.route, EscalateRoute::RevertToPhase);
        assert_eq!(
            ldiff.target_phase.as_deref(),
            Some("differentiation-analysis")
        );
    }

    #[test]
    fn m32_escalate_grammar_extension_revert_without_target_fails() {
        let src = concat!(
            "name: x\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: MISSING_TARGET\n",
            "    route: revert_to_phase\n",
            "    reason: forgot the target phase\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("target_phase"));
    }

    #[test]
    fn m32_escalate_grammar_extension_duplicate_prefix_fails() {
        let src = concat!(
            "name: x\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: DUP\n",
            "    route: abort\n",
            "  - prefix: DUP\n",
            "    route: need_user_input\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("DUP"));
    }

    #[test]
    fn m32_escalate_grammar_extension_empty_prefix_fails() {
        let src = concat!(
            "name: x\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: ''\n",
            "    route: abort\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("empty"));
    }

    #[test]
    fn m32_team_wide_golden_rules_legacy_flat_list_still_parses() {
        // V0.2 M0.18: legacy flat list deserializes via serde alias as
        // protocol with enforce: cmd_check. M3.x team yamls keep working.
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  - rule_id: tests_green\n",
            "    cmd: cargo test --workspace\n",
            "  - rule_id: no_secrets\n",
            "    pattern: 'AWS_SECRET'\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.golden_rules.protocol.len(), 2);
        assert!(spec.golden_rules.domain.is_empty());
        assert_eq!(
            spec.golden_rules.protocol[0].enforce,
            GoldenRuleEnforcement::CmdCheck,
        );
    }

    #[test]
    fn m32_team_wide_golden_rules_with_both_cmd_and_pattern_fails() {
        let src = concat!(
            "name: x\n",
            "golden_rules:\n",
            "  - rule_id: confused\n",
            "    cmd: ls\n",
            "    pattern: 'foo'\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("confused"));
    }

    // ---------------- V0.2 M0.18 protocol / domain split ----------------

    #[test]
    fn m018_team_golden_rules_structured_form_parses() {
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  protocol:\n",
            "    - rule_id: tests_green\n",
            "      enforce: cmd_check\n",
            "      cmd: cargo test --workspace\n",
            "    - rule_id: outbox_only\n",
            "      enforce: prompt_directive\n",
            "      directive: '询问用户唯一合法出口是 outbox'\n",
            "  domain:\n",
            "    - rule_id: prefer_small_pr\n",
            "      directive: 'PR 控制在 500 行以内'\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.golden_rules.protocol.len(), 2);
        assert_eq!(spec.golden_rules.domain.len(), 1);
        assert_eq!(
            spec.golden_rules.protocol[1].enforce,
            GoldenRuleEnforcement::PromptDirective,
        );
    }

    #[test]
    fn m018_team_golden_rules_protocol_prompt_directive_requires_directive_text() {
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  protocol:\n",
            "    - rule_id: missing_text\n",
            "      enforce: prompt_directive\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("missing_text"));
    }

    #[test]
    fn m018_team_golden_rules_domain_rule_requires_non_empty_directive() {
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  domain:\n",
            "    - rule_id: empty_directive\n",
            "      directive: ''\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("empty_directive"));
    }

    #[test]
    fn m018_team_golden_rules_protocol_duplicate_rule_id_fails() {
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  protocol:\n",
            "    - rule_id: dup\n",
            "      cmd: ls\n",
            "    - rule_id: dup\n",
            "      cmd: pwd\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("dup"));
    }

    // ---------------- V0.2 M0.19.4 forbid_ask_user_question ----------------

    #[test]
    fn m019_forbid_ask_user_question_rule_loads_as_prompt_directive() {
        // Shape every shipped team.yaml uses to enforce the self-loop
        // protocol: a `protocol` rule with `enforce: prompt_directive`
        // whose `directive` text rides into every phase's inject prompt.
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  protocol:\n",
            "    - rule_id: forbid_ask_user_question\n",
            "      enforce: prompt_directive\n",
            "      directive: '禁用 AskUserQuestion / 纯文本问句。改写 .ccteam/outbox/clarify-<ts>.md。'\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.golden_rules.protocol.len(), 1);
        let rule = &spec.golden_rules.protocol[0];
        assert_eq!(rule.rule_id, "forbid_ask_user_question");
        assert_eq!(rule.enforce, GoldenRuleEnforcement::PromptDirective);
        assert!(rule
            .directive
            .as_deref()
            .unwrap()
            .contains("AskUserQuestion"));
    }

    #[test]
    fn m32_verdict_schema_round_trips() {
        let src = concat!(
            "name: product-research\n",
            "verdict_schema:\n",
            "  - verdict\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.verdict_schema, vec!["verdict".to_string()]);
    }

    #[test]
    fn m32_verdict_schema_empty_entry_fails() {
        let src = "name: x\nverdict_schema:\n  - ''\n";
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn m32_critic_dimensions_parse() {
        let src = concat!(
            "name: research\n",
            "critic_dimensions:\n",
            "  - name: source_diversity\n",
            "    weight: 0.25\n",
            "    weak_threshold: 0.4\n",
            "    anti_leniency_strictness: strict\n",
            "    rubric: 0.0 = single source; 1.0 = >=3 cross-validated\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.critic_dimensions.len(), 1);
        let d = &spec.critic_dimensions[0];
        assert_eq!(d.name, "source_diversity");
        assert!((d.weight - 0.25).abs() < 1e-9);
        assert_eq!(d.anti_leniency_strictness, CriticStrictness::Strict);
    }

    #[test]
    fn m32_critic_dimensions_default_strictness_is_normal() {
        let src = concat!(
            "name: dev\n",
            "critic_dimensions:\n",
            "  - name: functionality\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(
            spec.critic_dimensions[0].anti_leniency_strictness,
            CriticStrictness::Normal,
        );
    }

    #[test]
    fn m32_critic_dimensions_duplicate_name_fails() {
        let src = concat!(
            "name: x\n",
            "critic_dimensions:\n",
            "  - name: dup\n",
            "  - name: dup\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("dup"));
    }

    #[test]
    fn m32_dev_team_yaml_legacy_shape_still_parses() {
        // Reverse-engineering check: the M3.1 dev team.yaml example
        // (no M3.2 fields) must still parse without migration —
        // serde defaults handle every new field.
        let src = concat!(
            "name: dev\n",
            "description: Software development team\n",
            "retro_schema:\n",
            "  - field: tech_stack\n",
            "    description: Languages, frameworks, key libraries used\n",
            "  - field: pitfalls\n",
            "    description: Mistakes / surprises to avoid next time\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.name, "dev");
        assert_eq!(spec.phase_dir, "phases");
        assert!(spec.critic_dimensions.is_empty());
        assert!(spec.escalate_grammar_extensions.is_empty());
        assert!(spec.golden_rules.is_empty());
        assert!(spec.verdict_schema.is_empty());
    }

    // ---------------- V0.2 M0.16 fields (§6.4 candidate 5) ----------------

    #[test]
    fn m016_evergreen_defaults_false_and_cost_policy_kill_at_none() {
        let spec = TeamSpec::parse("name: dev\n").unwrap();
        assert!(!spec.evergreen);
        assert_eq!(spec.cost_policy, CostPolicy::KillAt(None));
    }

    #[test]
    fn m016_evergreen_round_trips() {
        let src = concat!(
            "name: meta-agent\n",
            "evergreen: true\n",
            "cost_policy:\n",
            "  kind: none\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert!(spec.evergreen);
        assert_eq!(spec.cost_policy, CostPolicy::None);
    }

    #[test]
    fn m016_cost_policy_kill_at_with_threshold_round_trips() {
        let src = concat!(
            "name: x\n",
            "cost_policy:\n",
            "  kind: kill_at\n",
            "  threshold_usd: 200.0\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.cost_policy, CostPolicy::KillAt(Some(200.0)));
    }

    #[test]
    fn m016_claude_md_template_default_is_empty_string() {
        let spec = TeamSpec::parse("name: dev\n").unwrap();
        assert!(spec.claude_md_template.is_empty());
    }

    #[test]
    fn m016_claude_md_template_round_trips() {
        let src = concat!(
            "name: dev\n",
            "claude_md_template: |\n",
            "  # CLAUDE.md\n",
            "  slug: {slug}\n",
            "  team: {team}\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert!(spec.claude_md_template.contains("{slug}"));
        assert!(spec.claude_md_template.contains("{team}"));
    }

    #[test]
    fn m32_full_product_research_team_yaml_parses() {
        // Stand-in for the real product-research team.yaml that lands
        // in M3.4 — exercises every M3.2 field at once.
        let src = concat!(
            "name: product-research\n",
            "description: Product research team — should we build this idea?\n",
            "phase_dir: phases-product-research\n",
            "verdict_schema:\n",
            "  - verdict\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: MARKET_DUPLICATE\n",
            "    route: abort\n",
            "    reason: target market saturated\n",
            "  - prefix: INSUFFICIENT_VALIDATION\n",
            "    route: need_user_input\n",
            "    reason: cannot validate within round budget\n",
            "  - prefix: LOW_DIFFERENTIATION\n",
            "    route: revert_to_phase\n",
            "    target_phase: differentiation-analysis\n",
            "    reason: differentiation gap too small\n",
            "retro_schema:\n",
            "  - field: market_signals\n",
            "    description: Top market signals collected\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        // V0.2 M0.17.2: legacy `phases-product-research` rewritten to
        // `phases` on parse (now relative to team dir, not ~/.ccteam/).
        assert_eq!(spec.phase_dir, "phases");
        assert_eq!(spec.verdict_schema, vec!["verdict".to_string()]);
        assert_eq!(spec.escalate_grammar_extensions.len(), 3);
    }

    // F31 — `deny_unknown_fields` fail-loud on typo'd / unknown keys
    // (V0.2.1). Two negatives: one at TeamSpec top level, one at a
    // nested struct (RetroFieldSpec). Other nested structs share the
    // same serde attribute path so coverage is by symmetry.

    #[test]
    fn f31_top_level_unknown_field_fails_loud() {
        // typo: `cost_polciy` instead of `cost_policy` — pre-V0.2.1
        // this silently fell back to defaults.
        let src = "name: dev\ncost_polciy: none\n";
        let err = TeamSpec::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field") && msg.contains("cost_polciy"),
            "expected unknown-field error mentioning `cost_polciy`, got: {msg}",
        );
    }

    #[test]
    fn f31_nested_unknown_field_fails_loud() {
        // typo inside a retro_schema entry — `descripton` instead of
        // `description`.
        let src = concat!(
            "name: dev\n",
            "retro_schema:\n",
            "  - field: tech_stack\n",
            "    descripton: Languages used\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field") && msg.contains("descripton"),
            "expected unknown-field error mentioning `descripton`, got: {msg}",
        );
    }

    // ---------------- V0.3.1 F48 kind schema ----------------

    #[test]
    fn f48_kind_defaults_to_workflow_when_omitted() {
        let spec = TeamSpec::parse("name: dev\n").unwrap();
        assert_eq!(spec.kind, TeamKind::Workflow);
    }

    #[test]
    fn f48_kind_parses_all_values() {
        let workflow = TeamSpec::parse("name: dev\nkind: workflow\n").unwrap();
        let multi = TeamSpec::parse("name: research\nkind: multi_workflow\n").unwrap();
        let flex = TeamSpec::parse("name: scratch\nkind: flex\n").unwrap();
        assert_eq!(workflow.kind, TeamKind::Workflow);
        assert_eq!(multi.kind, TeamKind::MultiWorkflow);
        assert_eq!(flex.kind, TeamKind::Flex);
    }

    #[test]
    fn f48_kind_round_trips_through_yaml() {
        let spec = TeamSpec::parse(concat!(
            "name: scratch\n",
            "kind: flex\n",
            "sessions:\n",
            "  - sid: claude-1\n",
            "    harness: claude\n",
        ))
        .unwrap();
        let yaml = serde_yaml::to_string(&spec).unwrap();
        assert!(yaml.contains("kind: flex"), "got:\n{yaml}");
        let parsed = TeamSpec::parse(&yaml).unwrap();
        assert_eq!(parsed, spec);
    }

    #[test]
    fn f48_flex_rejects_golden_rules() {
        let err = TeamSpec::parse(concat!(
            "name: scratch\n",
            "kind: flex\n",
            "golden_rules:\n",
            "  - rule_id: no-todo\n",
            "    pattern: TODO\n",
        ))
        .unwrap_err();
        assert!(format!("{err:#}").contains("kind=flex"));
        assert!(format!("{err:#}").contains("golden_rules"));
    }

    #[test]
    fn f48_flex_rejects_escalate_grammar_extensions() {
        let err = TeamSpec::parse(concat!(
            "name: scratch\n",
            "kind: flex\n",
            "escalate_grammar_extensions:\n",
            "  - prefix: NEED_REVIEW\n",
            "    route: need_user_input\n",
        ))
        .unwrap_err();
        assert!(format!("{err:#}").contains("kind=flex"));
        assert!(format!("{err:#}").contains("escalate_grammar_extensions"));
    }

    #[test]
    fn f48_flex_rejects_custom_phase_dir() {
        let err = TeamSpec::parse("name: scratch\nkind: flex\nphase_dir: custom\n").unwrap_err();
        assert!(format!("{err:#}").contains("custom `phase_dir`"));
    }

    #[test]
    fn f48_flex_rejects_retro_and_verdict_phase_machinery() {
        let retro = TeamSpec::parse(concat!(
            "name: scratch\n",
            "kind: flex\n",
            "retro_schema:\n",
            "  - field: findings\n",
            "    description: Findings\n",
        ))
        .unwrap_err();
        assert!(format!("{retro:#}").contains("retro_schema"));

        let verdict = TeamSpec::parse("name: scratch\nkind: flex\nverdict_schema:\n  - verdict\n")
            .unwrap_err();
        assert!(format!("{verdict:#}").contains("verdict_schema"));
    }

    // ---------------- V0.3.1 F47 sessions[] schema ----------------

    #[test]
    fn f47_sessions_field_default_empty_when_omitted() {
        // Back-compat invariant: every V0.1 / V0.2 / V0.3 yaml omits
        // `sessions:`; default `Vec::new()` keeps them parsing
        // unchanged.
        let spec = TeamSpec::parse("name: dev\n").unwrap();
        assert!(spec.sessions.is_empty());
    }

    #[test]
    fn f47_sessions_field_parses_claude_harness() {
        let src = concat!(
            "name: my-flex\n",
            "sessions:\n",
            "  - sid: claude-1\n",
            "    harness: claude\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.sessions.len(), 1);
        assert_eq!(spec.sessions[0].sid, "claude-1");
        assert_eq!(spec.sessions[0].harness, HarnessKind::Claude);
    }

    #[test]
    fn f47_sessions_field_parses_codex_harness() {
        // Schema test: `harness: codex` parsing (F47 schema + F49
        // runtime path, both shipped V0.4.x).
        let src = concat!(
            "name: my-flex\n",
            "sessions:\n",
            "  - sid: codex-1\n",
            "    harness: codex\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.sessions.len(), 1);
        assert_eq!(spec.sessions[0].harness, HarnessKind::Codex);
    }

    #[test]
    fn f47_sessions_field_parses_mixed_harnesses() {
        // Mirrors PRD §4.2.2 sample — flex teams may declare a
        // claude + codex pair as their bootstrap default.
        let src = concat!(
            "name: my-flex\n",
            "sessions:\n",
            "  - sid: claude-1\n",
            "    harness: claude\n",
            "  - sid: codex-1\n",
            "    harness: codex\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.sessions.len(), 2);
        assert_eq!(spec.sessions[0].harness, HarnessKind::Claude);
        assert_eq!(spec.sessions[1].harness, HarnessKind::Codex);
    }

    #[test]
    fn f47_sessions_field_harness_defaults_to_claude_when_omitted() {
        // `{sid: claude-1}` without an explicit `harness:` key parses
        // and resolves to HarnessKind::Claude (matches `Default` impl).
        let src = concat!("name: my-flex\n", "sessions:\n", "  - sid: claude-1\n",);
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.sessions[0].harness, HarnessKind::Claude);
    }

    #[test]
    fn f47_sessions_field_rejects_unknown_harness() {
        // `harness: anthropic` is not in the strict HarnessKind enum
        // — serde must reject so a typo doesn't silently fall back to
        // claude. Pins the strict-enum contract for V0.3.2 when the
        // codex spawn path lands.
        let src = concat!(
            "name: my-flex\n",
            "sessions:\n",
            "  - sid: x\n",
            "    harness: anthropic\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("anthropic")
                || msg.to_lowercase().contains("variant")
                || msg.to_lowercase().contains("harness"),
            "expected unknown-variant error for `harness: anthropic`, got: {msg}",
        );
    }

    #[test]
    fn f47_sessions_field_rejects_unknown_inner_field() {
        // `DefaultSessionSpec` is `deny_unknown_fields` — typo on
        // `sid` (e.g. `sd:`) must fail loud rather than silently
        // accept an empty-sid spec.
        let src = concat!(
            "name: my-flex\n",
            "sessions:\n",
            "  - sd: claude-1\n",
            "    harness: claude\n",
        );
        let err = TeamSpec::parse(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field") || msg.contains("missing field"),
            "expected unknown / missing field error, got: {msg}",
        );
    }

    #[test]
    fn f47_sessions_field_round_trip_through_yaml() {
        // Round-trip a TeamSpec carrying both harness flavors so the
        // serialize side honors the lowercase rename. The serialized
        // yaml must be re-parsable.
        let original = TeamSpec {
            name: "my-flex".into(),
            aliases: Vec::new(),
            kind: TeamKind::Flex,
            description: String::new(),
            retro_schema: Vec::new(),
            critic_dimensions: Vec::new(),
            escalate_grammar_extensions: Vec::new(),
            golden_rules: TeamGoldenRules::default(),
            phase_dir: "phases".into(),
            verdict_schema: Vec::new(),
            evergreen: false,
            cost_policy: CostPolicy::default(),
            claude_md_template: String::new(),
            sessions: vec![
                DefaultSessionSpec {
                    sid: "claude-1".into(),
                    harness: HarnessKind::Claude,
                },
                DefaultSessionSpec {
                    sid: "codex-1".into(),
                    harness: HarnessKind::Codex,
                },
            ],
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        // Lowercase rename must survive serialization.
        assert!(
            yaml.contains("harness: claude") && yaml.contains("harness: codex"),
            "expected lowercase harness values in serialized yaml, got:\n{yaml}",
        );
        let parsed = TeamSpec::parse(&yaml).unwrap();
        assert_eq!(parsed, original);
    }
}
