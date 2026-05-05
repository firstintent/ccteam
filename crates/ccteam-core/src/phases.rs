//! Phase template parser. Templates live as markdown files with a YAML
//! front matter header — schema in `docs/interfaces.md` §5.1.
//!
//! M0 parses the template strictly enough to support orchestrator checks:
//! - `parallelism` must be `solo` (M2/M3 widen this).
//! - `sub_skills` may be empty; orchestrator no-ops it. M2 implements
//!   actual scheduling (see development-plan §2.1 M0.6 acceptance).

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::Parallelism;
use crate::tool_surface::ToolsRequired;

const DEFAULT_AUTO_LOOP_MAX_ITERATIONS: u32 = 3;

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
}

fn default_auto_loop_max_iterations() -> u32 {
    DEFAULT_AUTO_LOOP_MAX_ITERATIONS
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

    /// Validate M0 invariants. Stricter checks (per-trigger sub_skill
    /// validity, agent_team coupling with parallelism::AgentTeam) land
    /// in M2/M3.
    pub fn validate_m0(&self) -> Result<()> {
        if self.parallelism != Parallelism::Solo {
            bail!(
                "phase `{}` declares parallelism `{:?}`; M0 only supports `solo` (development-plan §2.1)",
                self.name,
                self.parallelism,
            );
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
    fn validate_m0_rejects_agent_team_parallelism() {
        let src = concat!(
            "---\n",
            "name: implement\n",
            "parallelism: agent_team\n",
            "---\n",
            "body\n",
        );
        let t = PhaseTemplate::parse(src).unwrap();
        let err = t.validate_m0().unwrap_err();
        assert!(err.to_string().contains("solo"), "got: {err}");
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
        // M0 validation passes — auto_loop=false ignores the empty signal.
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
}
