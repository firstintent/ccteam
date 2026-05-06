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
use serde::{Deserialize, Serialize};

use crate::phases::GoldenRule;

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

fn default_phase_dir() -> String {
    "phases".into()
}

/// M3.2: per-dimension Critic configuration (strategic doc §2.3).
/// **Data form only in M3** — M5 Critic loop will consume this; the
/// orchestrator never reads it before M5 (strategic doc §A invariant 1
/// forbids putting dimension names in `match` arms).
///
/// All fields default so an M3 team.yaml can ship without any
/// `critic_dimensions:` block — M5 will start adding them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticStrictness {
    /// At least one comment of any level passes anti-leniency.
    Lenient,
    /// At least one CONCERN or BLOCK passes anti-leniency.
    Normal,
    /// Must have at least one BLOCK to pass anti-leniency.
    Strict,
}

impl Default for CriticStrictness {
    fn default() -> Self {
        Self::Normal
    }
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

/// `team.yaml` — the team-level config. M3.1 shipped name /
/// description / retro_schema; M3.2 adds the five fields below so the
/// orchestrator can route phases / ESCALATE prefixes / golden-rule
/// enforcement per team without ccteam-core knowing the team name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Team-wide default `golden_rules`. Phase YAML's `golden_rules`
    /// takes priority — phases that declare their own rules ignore
    /// the team default entirely (no merge, see strategic doc §3.4
    /// "不预设质量评分维度"). Empty = team has no default rules.
    #[serde(default)]
    pub golden_rules: Vec<GoldenRule>,

    /// Phase template directory relative to the orchestrator root
    /// (`~/.ccteam/`). Defaults to `phases/` so legacy dev installs
    /// keep working without migration. product-research uses
    /// `phases-product-research/`. Project-local templates live in
    /// `<project>/.ccteam/phases/` and are written by `bootstrap_project`
    /// — that path is independent of `phase_dir`.
    #[serde(default = "default_phase_dir")]
    pub phase_dir: String,

    /// Phase names that produce a `verdict:` document (interfaces §5.3).
    /// Used by M3.4 product-research to flag the `verdict` phase as
    /// the one whose output drives PASS/CONCERN/REJECT/CLARIFY routing.
    /// dev currently leaves this empty.
    #[serde(default)]
    pub verdict_schema: Vec<String>,
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

        // M3.2: phase_dir non-empty + no traversal.
        if self.phase_dir.trim().is_empty() {
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
                && ext.target_phase.as_deref().map_or(true, str::is_empty)
            {
                bail!(
                    "team.yaml: escalate prefix `{}` has route revert_to_phase but no target_phase",
                    ext.prefix,
                );
            }
        }

        // M3.2: golden_rules cmd | pattern xor.
        for rule in &self.golden_rules {
            rule.kind().with_context(|| {
                format!("team.yaml: golden_rule `{}` invalid", rule.rule_id)
            })?;
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
            description: "Software dev".into(),
            retro_schema: vec![RetroFieldSpec {
                field: "tech_stack".into(),
                description: "List of techs".into(),
                kind: RetroFieldKind::List,
            }],
            critic_dimensions: Vec::new(),
            escalate_grammar_extensions: Vec::new(),
            golden_rules: Vec::new(),
            phase_dir: "phases".into(),
            verdict_schema: Vec::new(),
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
        let src = "name: product-research\nphase_dir: phases-product-research\n";
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.phase_dir, "phases-product-research");
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
        assert_eq!(ldiff.target_phase.as_deref(), Some("differentiation-analysis"));
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
    fn m32_team_wide_golden_rules_parse_and_validate() {
        let src = concat!(
            "name: dev\n",
            "golden_rules:\n",
            "  - rule_id: tests_green\n",
            "    cmd: cargo test --workspace\n",
            "  - rule_id: no_secrets\n",
            "    pattern: 'AWS_SECRET'\n",
        );
        let spec = TeamSpec::parse(src).unwrap();
        assert_eq!(spec.golden_rules.len(), 2);
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
        assert_eq!(spec.phase_dir, "phases-product-research");
        assert_eq!(spec.verdict_schema, vec!["verdict".to_string()]);
        assert_eq!(spec.escalate_grammar_extensions.len(), 3);
    }
}
