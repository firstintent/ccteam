//! Phase template parser. Templates live as markdown files with a YAML
//! front matter header — schema in `docs/interfaces.md` §5.1.
//!
//! Schema scope:
//! - `parallelism` is one of `solo` / `agent_team` / `multi_session`.
//!   M2 lifts the gate to allow `agent_team` once `agent_team:` lists
//!   at least one role; `multi_session` still bails (M3.x).
//! - `sub_skills` is the auto-trigger schedule (M2.1+);
//! - `decision_mode` / `max_clarify_rounds` / `golden_rules` (M2.3+) are
//!   phase-internal user-decision UX + hard-quality enforcement contracts.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::Parallelism;
use crate::tool_surface::ToolsRequired;

const DEFAULT_AUTO_LOOP_MAX_ITERATIONS: u32 = 3;
const DEFAULT_MAX_CLARIFY_ROUNDS: u32 = 3;

/// V0.2 M0.18: default for `escalate_grammar_ref`. `standard` resolves
/// against the four built-in ESCALATE prefixes (`REVERT_TO_PHASE` /
/// `NEED_USER_INPUT` / `ABORT` / `INSUFFICIENT_CLARIFICATION`). A team
/// extends the surface via `team.yaml.escalate_grammar_extensions`,
/// not by inventing new `escalate_grammar_ref` values.
pub const DEFAULT_ESCALATE_GRAMMAR_REF: &str = "standard";

/// V0.2 M0.18: default for `outbox_question_protocol`. `v1` is the
/// shape documented in interfaces §3.4.3: outbox files under
/// `<project>/.ccteam/outbox/` with `event_kind: clarify` frontmatter.
pub const DEFAULT_OUTBOX_QUESTION_PROTOCOL: &str = "v1";

/// V0.2 M0.18: ordered set of inject-prompt segments the orchestrator
/// composes when dispatching a phase. Each name maps to a clause in
/// `progress::build_phase_prompt_with_attachments`. Phase YAML can
/// shrink the set via `inject_directives:` to opt out of segments
/// (escape hatch — most phases use the default).
pub const DEFAULT_INJECT_DIRECTIVES: &[&str] = &[
    "read_inputs",
    "write_outputs",
    "completion_signal",
    "escalate_grammar",
    "outbox_protocol",
    "auto_loop",
    "decision_mode",
];

/// Phase-internal multi-role agent (M2+; M0 ignores).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTeamRole {
    pub role: String,
}

/// Sub-skill trigger point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubSkillTrigger {
    PhaseStart,
    PhaseDone,
}

/// Sub-skill spec — a plugin agent / hook invoked around a phase
/// boundary (`docs/interfaces.md` §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubSkillSpec {
    pub skill: String,
    pub trigger: SubSkillTrigger,
    pub output_to: String,
}

/// Phase-level hook scripts (`docs/interfaces.md` §5.1, `hooks:` section).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseHooks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// M2.3: phase-internal user-decision UX (interfaces §5.6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    /// Block phase progress on `AskUserQuestion`. User assumed in-session.
    Sync,
    /// Write outbox `event_kind: clarify`; do not block. Pairs with
    /// `PHASE_DONE_PENDING` (M3.6) when remaining work depends on the
    /// answer. User assumed offline.
    Async,
    /// Try `AskUserQuestion` first; fall back to outbox after a short
    /// in-phase timeout. Default.
    #[default]
    Hybrid,
}

/// M2.3: hard-quality rule the orchestrator runs at phase boundary
/// (interfaces §5.1, `golden_rules`).
///
/// YAML form:
///
/// ```yaml
/// golden_rules:
///   - rule_id: tests_green
///     cmd: cargo test --workspace
///   - rule_id: no_secrets_in_repo
///     pattern: 'AWS_SECRET|sk-[a-zA-Z0-9]{32,}'
/// ```
///
/// Exactly one of `cmd` / `pattern` per rule; checked by
/// [`PhaseTemplate::validate_m0`]. The orchestrator carries no
/// hard-coded `rule_id` — phase YAML is the only source of which
/// rules to run, so plugin-style team templates can ship their own
/// quality bars without ccteam-core changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenRule {
    pub rule_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// What kind of enforcement a [`GoldenRule`] represents, after the
/// exactly-one-of-cmd-pattern invariant is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenRuleKind<'a> {
    /// Run a shell command; non-zero exit = violation.
    Cmd(&'a str),
    /// Regex match against staged diff / required-output text;
    /// any match = violation.
    Pattern(&'a str),
}

impl GoldenRule {
    /// Resolve to the validated `Cmd | Pattern` shape. Errors when the
    /// rule has zero or two of `cmd` / `pattern` set.
    pub fn kind(&self) -> Result<GoldenRuleKind<'_>> {
        match (self.cmd.as_deref(), self.pattern.as_deref()) {
            (Some(c), None) => Ok(GoldenRuleKind::Cmd(c)),
            (None, Some(p)) => Ok(GoldenRuleKind::Pattern(p)),
            (Some(_), Some(_)) => bail!(
                "golden_rule `{}` has both `cmd` and `pattern`; pick one",
                self.rule_id,
            ),
            (None, None) => bail!(
                "golden_rule `{}` missing both `cmd` and `pattern`; one is required",
                self.rule_id,
            ),
        }
    }
}

/// Parsed YAML front matter of a phase template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseTemplate {
    pub name: String,

    #[serde(default)]
    pub required_inputs: Vec<String>,

    #[serde(default)]
    pub required_outputs: Vec<String>,

    #[serde(default)]
    pub soft_cost_warn_usd: Option<f64>,

    #[serde(default)]
    pub stall_warn_minutes: Option<u64>,

    pub parallelism: Parallelism,

    #[serde(default)]
    pub agent_team: Vec<AgentTeamRole>,

    #[serde(default)]
    pub sub_skills: Vec<SubSkillSpec>,

    /// Tools the phase markdown invokes via Task / Skill / MCP. Validated
    /// against the live tool surface at orchestrator startup; any missing
    /// entry fails fast (M0.5.3). Schema in `docs/interfaces.md` §5.1.
    #[serde(default)]
    pub tools_required: ToolsRequired,

    #[serde(default)]
    pub hooks: PhaseHooks,

    /// When true, the orchestrator hands the loop to the Stop hook on
    /// dispatch (ralph-loop pattern, tech-design §3.5). The hook
    /// re-feeds `prompt` until either the assistant prints
    /// `completion_signal` or `auto_loop_max_iterations` is reached.
    /// Mechanism is phase-name-agnostic — dev's `fix` phase sets it
    /// true, research's `04-primary` data-collection phase will also
    /// set it true.
    ///
    /// V0.2 M0.19: default flipped to `true`. Every phase self-loops
    /// unless its yaml explicitly opts out with `auto_loop: false`.
    /// Pairs with the Stop-hook self-loop fallback (parse_phase_end
    /// exits 2 + stderr when neither PHASE_DONE / ESCALATE / outbox
    /// fired) so phases can never silently halt the orchestrator.
    #[serde(default = "default_auto_loop")]
    pub auto_loop: bool,

    /// Iteration cap when `auto_loop = true`. Defaults to 3 so existing
    /// dev `fix` phase markdown keeps the same semantics without
    /// declaring it.
    #[serde(default = "default_auto_loop_max_iterations")]
    pub auto_loop_max_iterations: u32,

    /// Substring the assistant must print to break out of the auto-loop.
    /// Required (non-empty) when `auto_loop = true`; ignored otherwise.
    /// Validated by `validate_m0`.
    #[serde(default)]
    pub completion_signal: String,

    /// Explicit DAG edge for the "phase passed" branch. When `None`
    /// (the YAML field is omitted), `Dag::from_templates` falls back
    /// to the next phase in topological filename order, or treats the
    /// phase as terminal when it's the last in the list. M3.2 widens
    /// this to fork on test results.
    #[serde(default)]
    pub next_on_done: Option<String>,

    /// Explicit DAG edge for the "phase escalated" branch. When
    /// `None`, the orchestrator marks the project terminally
    /// escalated (M0/M1 behavior). The M0.5.4 `REVERT_TO_PHASE`
    /// ESCALATE grammar continues to route via the event's
    /// `target_phase` field, independent of this static fallback.
    #[serde(default)]
    pub next_on_escalate: Option<String>,

    /// M2.3: how phase markdown surfaces user-decision questions
    /// (interfaces §5.6.1). Default `hybrid` keeps every legacy
    /// template working without declaring the field — `AskUserQuestion`
    /// is tried first and the phase falls back to outbox after a
    /// short in-phase timeout.
    #[serde(default)]
    pub decision_mode: DecisionMode,

    /// M2.3: hard cap on CLARIFY rounds inside one phase. Past this
    /// the phase must produce a best-effort artifact and ESCALATE
    /// `INSUFFICIENT_CLARIFICATION` (interfaces §5.6.2). Default 3 —
    /// verdict / kickoff phases bump it to 5–7 in their templates.
    #[serde(default = "default_max_clarify_rounds")]
    pub max_clarify_rounds: u32,

    /// M2.3: phase-level hard-quality rules (interfaces §5.1
    /// `golden_rules`). Empty = no enforcement; orchestrator never
    /// adds rules itself, so dev / product-research / etc. teams
    /// each declare their own.
    #[serde(default)]
    pub golden_rules: Vec<GoldenRule>,

    /// V0.2 M0.18: ESCALATE grammar dialect this phase emits. `standard`
    /// resolves to the four built-in prefixes; team-specific extensions
    /// (e.g. `MARKET_DUPLICATE`) come from `team.yaml.escalate_grammar_extensions`
    /// regardless of this field. `inject_directives` controls whether
    /// the inject prompt mentions the dialect at all.
    #[serde(default)]
    pub escalate_grammar_ref: Option<String>,

    /// V0.2 M0.18: which outbox-question protocol the inject prompt
    /// instructs the assistant to use when it needs to ask the user.
    /// Only `v1` ships in V0.2 (interfaces §3.4.3). Defaulting to
    /// `None` keeps legacy phase yamls valid and the inject prompt
    /// uses `DEFAULT_OUTBOX_QUESTION_PROTOCOL` when composing.
    #[serde(default)]
    pub outbox_question_protocol: Option<String>,

    /// V0.2 M0.18: explicit list of inject-prompt segments to compose.
    /// `None` (yaml omitted) means use `DEFAULT_INJECT_DIRECTIVES`.
    /// Phase yamls rarely override this — the field exists as an
    /// escape hatch when a phase truly needs a custom prompt shape.
    #[serde(default)]
    pub inject_directives: Option<Vec<String>>,
}

fn default_auto_loop_max_iterations() -> u32 {
    DEFAULT_AUTO_LOOP_MAX_ITERATIONS
}

/// V0.2 M0.19: phases self-loop unless explicitly opted out. The Stop
/// hook re-injects the same prompt up to `auto_loop_max_iterations`
/// times (default 3) until the assistant emits the phase's
/// `completion_signal` (default `PHASE_DONE: <name>`).
fn default_auto_loop() -> bool {
    true
}

fn default_max_clarify_rounds() -> u32 {
    DEFAULT_MAX_CLARIFY_ROUNDS
}

impl PhaseTemplate {
    /// Parse a phase template from its raw markdown source. The YAML
    /// front matter is delimited by `---` lines at the top of the file
    /// (compatible with most static site generators).
    pub fn parse(source: &str) -> Result<Self> {
        let frontmatter = extract_frontmatter(source)
            .context("phase template missing YAML front matter (expected `---` delimiters)")?;
        let template: PhaseTemplate = serde_yaml::from_str(frontmatter)
            .context("phase template front matter does not match schema")?;
        Ok(template)
    }

    /// Load + parse a template from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read phase template {}", path.display()))?;
        Self::parse(&source).with_context(|| format!("parse phase template {}", path.display()))
    }

    /// Validate phase-template invariants. Method name is preserved as
    /// `validate_m0` for callsite stability; the body widened in M2 to
    /// allow `parallelism: agent_team` (M2.2) and check the new M2.3
    /// fields (`max_clarify_rounds > 0`, `golden_rules` shape).
    pub fn validate_m0(&self) -> Result<()> {
        match self.parallelism {
            Parallelism::Solo => {}
            Parallelism::AgentTeam => {
                if self.agent_team.is_empty() {
                    bail!(
                        "phase `{}` declares parallelism `agent_team` but no `agent_team:` roles; nothing for the orchestrator to dispatch",
                        self.name,
                    );
                }
            }
            Parallelism::MultiSession => {
                bail!(
                    "phase `{}` declares parallelism `multi_session`; M2 does not support it yet (M3.x widens)",
                    self.name,
                );
            }
        }
        // V0.2 M0.18: when `completion_signal` is omitted the inject
        // prompt + auto-loop both fall back to
        // `PHASE_DONE: <phase-name>` via `effective_completion_signal()`,
        // so an empty literal field is no longer a validation error.
        // This lets phase yamls keep frontmatter free of the
        // `PHASE_DONE` literal — the protocol value is supplied by
        // ccteam-core's default when the field is absent.
        if self.auto_loop && self.auto_loop_max_iterations == 0 {
            bail!(
                "phase `{}` declares `auto_loop: true` with `auto_loop_max_iterations: 0` — that would never enter the loop",
                self.name,
            );
        }
        if self.max_clarify_rounds == 0 {
            bail!(
                "phase `{}` declares `max_clarify_rounds: 0`; phase would skip every clarify and ESCALATE on first try",
                self.name,
            );
        }
        for rule in &self.golden_rules {
            rule.kind().with_context(|| {
                format!(
                    "phase `{}` golden_rule `{}` invalid",
                    self.name, rule.rule_id
                )
            })?;
        }
        // V0.2 M0.18: an explicit empty `escalate_grammar_ref: ''` is
        // a configuration mistake — we default-fill it when missing,
        // but a present-but-empty value means the operator typed it
        // and meant something they didn't write.
        if let Some(g) = &self.escalate_grammar_ref {
            if g.trim().is_empty() {
                bail!(
                    "phase `{}` declares empty `escalate_grammar_ref` — drop the field to use `{}` default, or supply a non-empty dialect",
                    self.name,
                    DEFAULT_ESCALATE_GRAMMAR_REF,
                );
            }
        }
        if let Some(p) = &self.outbox_question_protocol {
            if p.trim().is_empty() {
                bail!(
                    "phase `{}` declares empty `outbox_question_protocol` — drop the field to use `{}` default",
                    self.name,
                    DEFAULT_OUTBOX_QUESTION_PROTOCOL,
                );
            }
        }
        if let Some(directives) = &self.inject_directives {
            for name in directives {
                if !DEFAULT_INJECT_DIRECTIVES.iter().any(|d| *d == name) {
                    bail!(
                        "phase `{}` declares unknown inject directive `{}`; valid: {:?}",
                        self.name,
                        name,
                        DEFAULT_INJECT_DIRECTIVES,
                    );
                }
            }
        }
        Ok(())
    }

    /// Cross-check `self.tools_required` against `snap`. Returns the
    /// list of unmet dependencies — empty means the phase is callable.
    pub fn missing_tools_against(
        &self,
        snap: &crate::tool_surface::ToolSurfaceSnapshot,
    ) -> Vec<crate::tool_surface::MissingTool> {
        crate::tool_surface::missing_tools(&self.tools_required, snap)
    }

    /// V0.2 M0.18: effective completion signal the inject prompt should
    /// reference. Falls back to `PHASE_DONE: <name>` when the yaml
    /// omits the field — mirrors the historic hardcoded inject string
    /// so legacy phase yamls keep behaving the same after the inject-
    /// prompt template lands.
    pub fn effective_completion_signal(&self) -> String {
        let trimmed = self.completion_signal.trim();
        if trimmed.is_empty() {
            format!("PHASE_DONE: {}", self.name)
        } else {
            trimmed.to_string()
        }
    }

    /// V0.2 M0.18: effective ESCALATE grammar dialect. Defaults to
    /// `DEFAULT_ESCALATE_GRAMMAR_REF` when omitted.
    pub fn effective_escalate_grammar_ref(&self) -> &str {
        self.escalate_grammar_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_ESCALATE_GRAMMAR_REF)
    }

    /// V0.2 M0.18: effective outbox-question protocol. Defaults to
    /// `DEFAULT_OUTBOX_QUESTION_PROTOCOL` when omitted.
    pub fn effective_outbox_question_protocol(&self) -> &str {
        self.outbox_question_protocol
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTBOX_QUESTION_PROTOCOL)
    }

    /// V0.2 M0.18: effective inject directives. Empty `Some(vec![])`
    /// is honored (a phase opts out of every conditional segment) —
    /// only `None` falls back to defaults.
    pub fn effective_inject_directives(&self) -> Vec<String> {
        match self.inject_directives.as_ref() {
            Some(list) => list.clone(),
            None => DEFAULT_INJECT_DIRECTIVES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

fn extract_frontmatter(source: &str) -> Result<&str> {
    let after_first = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("template must start with `---` line"))?;

    let end = after_first
        .find("\n---\n")
        .or_else(|| after_first.find("\n---\r\n"))
        .ok_or_else(|| anyhow!("template missing closing `---` line"))?;

    Ok(&after_first[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_solo_template() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "---\n",
            "# 任务\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.name, "implement");
        assert_eq!(t.parallelism, Parallelism::Solo);
        assert!(t.required_inputs.is_empty());
        assert!(t.sub_skills.is_empty());
        t.validate_m0().unwrap();
    }

    #[test]
    fn parses_sub_skills_and_agent_team() {
        let src = concat!(
            "---\n",
            "name: review\n",
            "parallelism: solo\n",
            "agent_team:\n",
            "  - role: reviewer\n",
            "sub_skills:\n",
            "  - skill: claude-plugins-official:pr-review-toolkit/agents/code-reviewer\n",
            "    trigger: phase_done\n",
            "    output_to: .ccteam/code-review.md\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.agent_team.len(), 1);
        assert_eq!(t.sub_skills[0].trigger, SubSkillTrigger::PhaseDone);
        assert_eq!(t.sub_skills[0].output_to, ".ccteam/code-review.md");
    }

    #[test]
    fn missing_frontmatter_errors() {
        let src = "no front matter here\n";
        let err = PhaseTemplate::parse(src).unwrap_err();
        assert!(format!("{err:#}").contains("front matter"));
    }

    #[test]
    fn parses_tools_required_with_subagents_skills_mcp() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "tools_required:\n",
            "  subagents: [code-reviewer, code-architect]\n",
            "  skills: [some-skill]\n",
            "  mcp: [playwright]\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(
            t.tools_required.subagents,
            vec!["code-reviewer", "code-architect"]
        );
        assert_eq!(t.tools_required.skills, vec!["some-skill"]);
        assert_eq!(t.tools_required.mcp, vec!["playwright"]);
    }

    #[test]
    fn tools_required_defaults_to_empty_when_omitted() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(t.tools_required.is_empty());
    }

    #[test]
    fn auto_loop_defaults_to_true_when_omitted() {
        // V0.2 M0.19: phases self-loop by default. A yaml that omits
        // the field gets `auto_loop: true` + the synthesized
        // `PHASE_DONE: <name>` completion signal so the Stop hook can
        // re-inject up to `auto_loop_max_iterations` times until the
        // assistant emits the signal.
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(t.auto_loop);
        assert_eq!(t.auto_loop_max_iterations, 3);
        assert!(t.completion_signal.is_empty());
        // effective_completion_signal() falls back to PHASE_DONE: <name>.
        assert_eq!(t.effective_completion_signal(), "PHASE_DONE: implement");
        t.validate_m0().unwrap();
    }

    #[test]
    fn auto_loop_explicit_false_opts_out() {
        // Some phase yamls (e.g. evergreen meta-agent stubs, ad-hoc
        // diagnostic phases) may want to skip the ralph-loop. Explicit
        // `auto_loop: false` continues to disable it.
        let src = concat!(
            "---\n",
            "name: kickoff\n",
            "parallelism: solo\n",
            "auto_loop: false\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(!t.auto_loop);
        t.validate_m0().unwrap();
    }

    #[test]
    fn auto_loop_explicit_fields_parse_correctly() {
        let src = concat!(
            "---\n",
            "name: fix\n",
            "parallelism: solo\n",
            "auto_loop: true\n",
            "auto_loop_max_iterations: 5\n",
            "completion_signal: TESTS_GREEN\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(t.auto_loop);
        assert_eq!(t.auto_loop_max_iterations, 5);
        assert_eq!(t.completion_signal, "TESTS_GREEN");
        t.validate_m0().unwrap();
    }

    #[test]
    fn auto_loop_true_without_completion_signal_defaults_to_phase_done_phase_name() {
        // V0.2 M0.18: a phase yaml that omits `completion_signal` is no
        // longer a validation error. The inject-prompt builder + auto-
        // loop bootstrap synthesize `PHASE_DONE: <name>` via
        // `effective_completion_signal()`. Drops a body-level reason
        // for phase markdown to repeat the protocol literal.
        let src = concat!(
            "---\n",
            "name: fix\n",
            "parallelism: solo\n",
            "auto_loop: true\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        t.validate_m0().unwrap();
        assert_eq!(t.effective_completion_signal(), "PHASE_DONE: fix");
    }

    #[test]
    fn auto_loop_true_with_zero_max_iterations_fails_validation() {
        let src = concat!(
            "---\n",
            "name: fix\n",
            "parallelism: solo\n",
            "auto_loop: true\n",
            "auto_loop_max_iterations: 0\n",
            "completion_signal: TESTS_GREEN\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(
            format!("{err:#}").contains("auto_loop_max_iterations"),
            "got: {err:#}",
        );
    }

    // ---------------- M2.2 / M2.3 ----------------

    #[test]
    fn validate_m2_accepts_agent_team_with_roles() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: agent_team\n",
            "agent_team:\n",
            "  - role: backend-dev\n",
            "  - role: reviewer\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        t.validate_m0().unwrap();
        assert_eq!(t.parallelism, Parallelism::AgentTeam);
        assert_eq!(t.agent_team.len(), 2);
    }

    #[test]
    fn validate_m2_rejects_agent_team_without_roles() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: agent_team\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("agent_team"), "got: {err:#}",);
    }

    #[test]
    fn validate_m2_still_rejects_multi_session() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: multi_session\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("multi_session"), "got: {err:#}",);
    }

    #[test]
    fn decision_mode_defaults_to_hybrid() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.decision_mode, DecisionMode::Hybrid);
        assert_eq!(t.max_clarify_rounds, 3);
        assert!(t.golden_rules.is_empty());
    }

    #[test]
    fn decision_mode_parses_explicit_sync_async_hybrid() {
        for mode in ["sync", "async", "hybrid"] {
            let src =
                format!("---\nname: x\nparallelism: solo\ndecision_mode: {mode}\n---\nbody\n",);
            let t = PhaseTemplate::parse(&src).unwrap();
            let expected = match mode {
                "sync" => DecisionMode::Sync,
                "async" => DecisionMode::Async,
                "hybrid" => DecisionMode::Hybrid,
                _ => unreachable!(),
            };
            assert_eq!(t.decision_mode, expected, "mode={mode}");
        }
    }

    #[test]
    fn max_clarify_rounds_zero_fails_validation() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "max_clarify_rounds: 0\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(
            format!("{err:#}").contains("max_clarify_rounds"),
            "got: {err:#}",
        );
    }

    #[test]
    fn golden_rules_parse_cmd_and_pattern() {
        let src = concat!(
            "---\n",
            "name: ship\n",
            "parallelism: solo\n",
            "golden_rules:\n",
            "  - rule_id: tests_green\n",
            "    cmd: cargo test --workspace\n",
            "  - rule_id: no_secrets_in_repo\n",
            "    pattern: 'AWS_SECRET|sk-[a-zA-Z0-9]{32,}'\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        t.validate_m0().unwrap();
        assert_eq!(t.golden_rules.len(), 2);
        assert!(
            matches!(t.golden_rules[0].kind().unwrap(), GoldenRuleKind::Cmd(c) if c.starts_with("cargo"))
        );
        assert!(
            matches!(t.golden_rules[1].kind().unwrap(), GoldenRuleKind::Pattern(p) if p.contains("AWS_SECRET"))
        );
    }

    #[test]
    fn golden_rule_with_both_cmd_and_pattern_fails() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "golden_rules:\n",
            "  - rule_id: confused\n",
            "    cmd: ls\n",
            "    pattern: 'foo'\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("confused"), "got: {err:#}",);
    }

    // ---------------- V0.2 M0.18 inject-prompt frontmatter ----------------

    #[test]
    fn m018_completion_signal_defaults_to_phase_done_phase_when_omitted() {
        let src = concat!("---\n", "name: implement\n", "parallelism: solo\n", "---\n",);
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.effective_completion_signal(), "PHASE_DONE: implement");
    }

    #[test]
    fn m018_completion_signal_explicit_value_takes_priority() {
        let src = concat!(
            "---\n",
            "name: fix\n",
            "parallelism: solo\n",
            "auto_loop: true\n",
            "completion_signal: TESTS_GREEN\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.effective_completion_signal(), "TESTS_GREEN");
    }

    #[test]
    fn m018_escalate_grammar_ref_defaults_to_standard() {
        let src = "---\nname: x\nparallelism: solo\n---\n";
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.effective_escalate_grammar_ref(), "standard");
    }

    #[test]
    fn m018_outbox_question_protocol_defaults_to_v1() {
        let src = "---\nname: x\nparallelism: solo\n---\n";
        let t = PhaseTemplate::parse(src).unwrap();
        assert_eq!(t.effective_outbox_question_protocol(), "v1");
    }

    #[test]
    fn m018_inject_directives_default_when_omitted() {
        let src = "---\nname: x\nparallelism: solo\n---\n";
        let t = PhaseTemplate::parse(src).unwrap();
        let d = t.effective_inject_directives();
        assert!(d.contains(&"read_inputs".to_string()));
        assert!(d.contains(&"completion_signal".to_string()));
        assert!(d.contains(&"escalate_grammar".to_string()));
    }

    #[test]
    fn m018_inject_directives_explicit_empty_opts_out_of_all_segments() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "inject_directives: []\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(t.effective_inject_directives().is_empty());
    }

    #[test]
    fn m018_inject_directives_unknown_name_fails_validation() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "inject_directives:\n",
            "  - bogus_directive\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(
            format!("{err:#}").contains("bogus_directive"),
            "got: {err:#}"
        );
    }

    #[test]
    fn m018_empty_escalate_grammar_ref_string_fails_validation() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "escalate_grammar_ref: ''\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("escalate_grammar_ref"));
    }

    #[test]
    fn m018_empty_outbox_question_protocol_fails_validation() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "outbox_question_protocol: ''\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("outbox_question_protocol"));
    }

    #[test]
    fn m018_legacy_phase_yaml_without_v02_fields_still_parses() {
        // Mirror of the M2 / M3 era yaml shape — every V0.2 inject-
        // prompt field defaulted means existing dev / product-research
        // phase markdowns load unchanged.
        let src = concat!(
            "---\n",
            "name: implement\n",
            "required_inputs: [.ccteam/plan-eng.md]\n",
            "required_outputs: [.ccteam/implement-report.md]\n",
            "parallelism: solo\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(t.escalate_grammar_ref.is_none());
        assert!(t.outbox_question_protocol.is_none());
        assert!(t.inject_directives.is_none());
        assert_eq!(t.effective_completion_signal(), "PHASE_DONE: implement");
    }

    #[test]
    fn golden_rule_with_neither_cmd_nor_pattern_fails() {
        let src = concat!(
            "---\n",
            "name: x\n",
            "parallelism: solo\n",
            "golden_rules:\n",
            "  - rule_id: empty_rule\n",
            "---\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(format!("{err:#}").contains("empty_rule"), "got: {err:#}",);
    }
}
