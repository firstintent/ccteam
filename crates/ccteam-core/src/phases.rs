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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    Hybrid,
}

impl Default for DecisionMode {
    fn default() -> Self {
        Self::Hybrid
    }
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
    #[serde(default)]
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
}

fn default_auto_loop_max_iterations() -> u32 {
    DEFAULT_AUTO_LOOP_MAX_ITERATIONS
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
        Self::parse(&source)
            .with_context(|| format!("parse phase template {}", path.display()))
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
        if self.auto_loop && self.completion_signal.trim().is_empty() {
            bail!(
                "phase `{}` declares `auto_loop: true` but no `completion_signal` — orchestrator can't tell when the loop is done",
                self.name,
            );
        }
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
                format!("phase `{}` golden_rule `{}` invalid", self.name, rule.rule_id)
            })?;
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
        assert_eq!(t.tools_required.subagents, vec!["code-reviewer", "code-architect"]);
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
    fn auto_loop_defaults_to_false_when_omitted() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        assert!(!t.auto_loop);
        assert_eq!(t.auto_loop_max_iterations, 3);
        assert!(t.completion_signal.is_empty());
        // Validation passes — auto_loop=false ignores the empty signal.
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
    fn auto_loop_true_without_completion_signal_fails_validation() {
        let src = concat!(
            "---\n",
            "name: fix\n",
            "parallelism: solo\n",
            "auto_loop: true\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(
            format!("{err:#}").contains("completion_signal"),
            "got: {err:#}",
        );
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
        assert!(
            format!("{err:#}").contains("agent_team"),
            "got: {err:#}",
        );
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
        assert!(
            format!("{err:#}").contains("multi_session"),
            "got: {err:#}",
        );
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
            let src = format!(
                "---\nname: x\nparallelism: solo\ndecision_mode: {mode}\n---\nbody\n",
            );
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
        assert!(matches!(t.golden_rules[0].kind().unwrap(), GoldenRuleKind::Cmd(c) if c.starts_with("cargo")));
        assert!(matches!(t.golden_rules[1].kind().unwrap(), GoldenRuleKind::Pattern(p) if p.contains("AWS_SECRET")));
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
        assert!(
            format!("{err:#}").contains("confused"),
            "got: {err:#}",
        );
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
        assert!(
            format!("{err:#}").contains("empty_rule"),
            "got: {err:#}",
        );
    }
}
