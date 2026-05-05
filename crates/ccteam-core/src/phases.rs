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

    #[serde(default)]
    pub hooks: PhaseHooks,
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
        Ok(())
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
}
