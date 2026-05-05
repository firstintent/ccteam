//! `team.yaml` schema — team-level configuration that lives next to a
//! team's phase template directory (M3.4 lays out the on-disk layout).
//!
//! M3.1 scope: **data form + parsing only.** No call sites yet — M4.1
//! retro phase implementation will read `retro_schema` so retro reports
//! pick up the right fields per team from day one.
//!
//! Why this lands in M3.1 (and not M4.1): cross-project memory was
//! reordered after team-abstraction in development-plan §5. Without
//! `retro_schema` shipped first, M4.1 retro would freeze dev-only fields
//! (tech stack / pitfalls / …) into the RAG index, forcing a rebuild
//! when research team retro lands later. F20 was upgraded P1→P0 for
//! exactly this reason — see `dev-coupling-audit.md §F20` for context.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// One field in a team's retro schema. M4.1 retro phase emits a
/// markdown section per entry; the cross-project memory RAG indexes
/// each field as a tagged document so future projects can pull only
/// the field types relevant to their own team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// `team.yaml` — the team-level config. M3.1 ships only the fields
/// loadable by the orchestrator at startup; M3.4 adds artifact lists,
/// danger-command patterns, claude_md_template, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSpec {
    /// Team identifier. Must match the `--team` arg / `state.json.team`
    /// field. snake-case lowercase — gets used as a directory name.
    pub name: String,

    /// Human-readable one-liner; surfaced by `ccteam ls --teams`
    /// (M3.4) and in error messages.
    #[serde(default)]
    pub description: String,

    /// Field schema for the retro phase (M4.1). Empty list = no
    /// retro phase for this team. Order is preserved — the retro
    /// markdown emits sections in this order.
    #[serde(default)]
    pub retro_schema: Vec<RetroFieldSpec>,
}

impl TeamSpec {
    /// Parse `team.yaml` from raw YAML source.
    pub fn parse(source: &str) -> Result<Self> {
        let spec: TeamSpec = serde_yaml::from_str(source)
            .context("team.yaml does not match schema")?;
        spec.validate()?;
        Ok(spec)
    }

    /// Load + parse `team.yaml` from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read team.yaml at {}", path.display()))?;
        Self::parse(&source)
            .with_context(|| format!("parse team.yaml at {}", path.display()))
    }

    /// Sanity checks at parse time. The orchestrator never wants to
    /// hold an "almost OK" TeamSpec — better to fail loud at load
    /// than to discover a duplicate retro field name during M4.1
    /// retro execution.
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
            description: "Software dev".into(),
            retro_schema: vec![RetroFieldSpec {
                field: "tech_stack".into(),
                description: "List of techs".into(),
                kind: RetroFieldKind::List,
            }],
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed = TeamSpec::parse(&yaml).unwrap();
        assert_eq!(parsed, original);
    }
}
