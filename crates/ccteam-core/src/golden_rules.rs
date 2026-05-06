//! Phase-level `golden_rules` enforcement (M2.3 schema → M2.3 follow-up
//! executor, this module).
//!
//! `interfaces.md` §5.1 documents the YAML schema. Each rule is exactly
//! one of `cmd` or `pattern`:
//!
//! ```yaml
//! golden_rules:
//!   - rule_id: tests_green
//!     cmd: cargo test --workspace      # exit code != 0 = violation
//!   - rule_id: no_secrets_in_repo
//!     pattern: 'AWS_SECRET|sk-[a-zA-Z0-9]{32,}'   # any regex match = violation
//! ```
//!
//! Resolution path (decision §4.3 in the M2 prompt, option (c)
//! ack'd 2026-05-06): the orchestrator runs [`enforce`] **after**
//! sub-skill `phase_done` triggers but **before** declaring the phase
//! advanced — see `orchestrator::process_project`'s `AdvancePhase`
//! arm. A non-empty violation list blocks the transition: the
//! orchestrator writes `escalation.md`, marks the phase entry
//! `blocked` instead of `passed`, and routes through the normal
//! escalation flow (user resumes after fixing).
//!
//! Why a separate module (rather than inlined into orchestrator):
//! - Pure data in / structured report out keeps the executor unit-
//!   testable without spinning up a real `Orchestrator`.
//! - The Pattern path needs filesystem reads + regex compile; isolating
//!   them keeps `orchestrator.rs`'s tick loop readable.
//! - A future extension (Pattern globbing, stdin-based shell rules,
//!   git-diff-aware scanning) lands here without touching the orchestrator.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::phases::{GoldenRule, GoldenRuleKind, PhaseTemplate};

/// One violation = one `golden_rules[]` entry that failed enforcement.
///
/// `detail` is a one-line human-readable summary the escalation writer
/// puts in `escalation.md` (e.g. the failing command's stderr first
/// line, or "matched 'AWS_SECRET' in .ccteam/implement-report.md").
/// Keep it short — the file path + rule_id together are usually enough
/// for the user to find the issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenRuleViolation {
    pub rule_id: String,
    pub kind: GoldenRuleKindLabel,
    pub detail: String,
}

/// Schema-stable label for [`GoldenRuleKind`] (the latter is borrowed,
/// so it can't go in a serializable struct that travels through
/// `escalation.md` / `progress.jsonl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoldenRuleKindLabel {
    Cmd,
    Pattern,
}


/// Result of running every rule in a phase template's `golden_rules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenRulesReport {
    /// Rule IDs that ran without finding a violation.
    pub passed: Vec<String>,
    /// One entry per rule that violated. Empty = phase advances.
    pub violations: Vec<GoldenRuleViolation>,
    /// Rule IDs we couldn't evaluate (e.g. malformed regex; pattern
    /// rule pointing at a glob `required_outputs`). Reported but does
    /// **not** block the phase — surfaced as a warning so phase author
    /// can fix the rule definition.
    pub skipped: Vec<GoldenRuleSkipped>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenRuleSkipped {
    pub rule_id: String,
    pub reason: String,
}

impl GoldenRulesReport {
    pub fn is_pass(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Run every rule in `template.golden_rules` against `project_dir` and
/// return a structured report. Empty `golden_rules` → empty PASS report
/// (no-op for templates that don't opt in).
///
/// `project_dir` is the project's git working copy (one level above
/// `.ccteam/`) — `Cmd` rules run with `current_dir = project_dir` and
/// `Pattern` rules resolve `required_outputs` paths relative to it.
pub fn enforce(template: &PhaseTemplate, project_dir: &Path) -> Result<GoldenRulesReport> {
    let mut report = GoldenRulesReport {
        passed: Vec::new(),
        violations: Vec::new(),
        skipped: Vec::new(),
    };

    for rule in &template.golden_rules {
        let kind = match rule.kind() {
            Ok(k) => k,
            Err(err) => {
                // Schema violation that escaped phase YAML validation —
                // shouldn't happen in practice (validate_m0 enforces
                // exactly one of cmd/pattern), but fail safe.
                report.skipped.push(GoldenRuleSkipped {
                    rule_id: rule.rule_id.clone(),
                    reason: format!("invalid rule: {err}"),
                });
                continue;
            }
        };
        match kind {
            GoldenRuleKind::Cmd(cmd) => match run_cmd_rule(rule, cmd, project_dir) {
                Ok(Some(violation)) => report.violations.push(violation),
                Ok(None) => report.passed.push(rule.rule_id.clone()),
                Err(err) => report.skipped.push(GoldenRuleSkipped {
                    rule_id: rule.rule_id.clone(),
                    reason: format!("could not spawn cmd: {err:#}"),
                }),
            },
            GoldenRuleKind::Pattern(pattern) => {
                match run_pattern_rule(rule, pattern, &template.required_outputs, project_dir) {
                    PatternOutcome::Pass => report.passed.push(rule.rule_id.clone()),
                    PatternOutcome::Violation(v) => report.violations.push(v),
                    PatternOutcome::Skipped(reason) => {
                        report.skipped.push(GoldenRuleSkipped {
                            rule_id: rule.rule_id.clone(),
                            reason,
                        })
                    }
                }
            }
        }
    }

    Ok(report)
}

/// Spawn `cmd` via `sh -c` so users can write
/// `cargo test --workspace && cargo clippy -- -D warnings` without
/// learning a custom argv splitter. Returns `Ok(Some(violation))` on
/// non-zero exit and `Ok(None)` on success. `Err(_)` is reserved for
/// "couldn't even spawn" — those land in `skipped`, not `violations`.
fn run_cmd_rule(
    rule: &GoldenRule,
    cmd: &str,
    project_dir: &Path,
) -> Result<Option<GoldenRuleViolation>> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(project_dir)
        .output()
        .with_context(|| format!("spawn `{cmd}`"))?;
    if output.status.success() {
        return Ok(None);
    }
    let stderr_first = String::from_utf8_lossy(&output.stderr)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no stderr)")
        .to_string();
    let exit = output
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".into());
    Ok(Some(GoldenRuleViolation {
        rule_id: rule.rule_id.clone(),
        kind: GoldenRuleKindLabel::Cmd,
        detail: format!("exit {exit}: {stderr_first}"),
    }))
}

enum PatternOutcome {
    Pass,
    Violation(GoldenRuleViolation),
    Skipped(String),
}

/// Compile `pattern` once, then scan every `required_outputs` entry as
/// a literal file path under `project_dir`. First match wins. Glob
/// outputs (`*` / `?` / `**`) are skipped with a clear reason — pattern
/// scanning across glob expansions is a future extension.
fn run_pattern_rule(
    rule: &GoldenRule,
    pattern: &str,
    required_outputs: &[String],
    project_dir: &Path,
) -> PatternOutcome {
    let regex = match Regex::new(pattern) {
        Ok(r) => r,
        Err(err) => {
            return PatternOutcome::Skipped(format!("invalid regex `{pattern}`: {err}"));
        }
    };

    if required_outputs.is_empty() {
        return PatternOutcome::Skipped(
            "no required_outputs to scan — pattern rules need a target file list".into(),
        );
    }

    let mut scanned = 0usize;
    for entry in required_outputs {
        if entry.contains('*') || entry.contains('?') {
            // Glob — skip in this iteration. Report in skipped only if
            // we can't find any non-glob path to scan.
            continue;
        }
        let path: PathBuf = if Path::new(entry).is_absolute() {
            PathBuf::from(entry)
        } else {
            project_dir.join(entry)
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(err) => {
                tracing::debug!(
                    rule = %rule.rule_id,
                    path = %path.display(),
                    error = %err,
                    "pattern rule could not read required_output; skipping this file",
                );
                continue;
            }
        };
        scanned += 1;
        if let Some(m) = regex.find(&body) {
            // 1-based line number of the first match. We count
            // newlines in the prefix and add one — `.lines().count()`
            // is wrong when the prefix doesn't end on a newline, since
            // the match still belongs to that partial line.
            let line_no = body[..m.start()].matches('\n').count() + 1;
            return PatternOutcome::Violation(GoldenRuleViolation {
                rule_id: rule.rule_id.clone(),
                kind: GoldenRuleKindLabel::Pattern,
                detail: format!(
                    "matched `{}` at {}:{}",
                    truncate_match(m.as_str()),
                    entry,
                    line_no,
                ),
            });
        }
    }

    if scanned == 0 {
        PatternOutcome::Skipped(format!(
            "all {} required_outputs entries were globs or unreadable; no files scanned",
            required_outputs.len(),
        ))
    } else {
        PatternOutcome::Pass
    }
}

fn truncate_match(s: &str) -> String {
    const MAX: usize = 40;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::{GoldenRule, PhaseTemplate};
    use tempfile::TempDir;

    fn template_with(rules: Vec<GoldenRule>, required_outputs: Vec<String>) -> PhaseTemplate {
        // Fields not relevant to these tests get default-ish values.
        // PhaseTemplate fields are `pub`, but its constructor is
        // private — populate via the test-friendly path: parse a YAML
        // template that pins only the fields we need.
        let yaml = build_phase_yaml(&rules, &required_outputs);
        PhaseTemplate::parse(&yaml).unwrap()
    }

    fn build_phase_yaml(rules: &[GoldenRule], required_outputs: &[String]) -> String {
        let mut s = String::from("---\nname: enforce-test\nparallelism: solo\n");
        if !required_outputs.is_empty() {
            s.push_str("required_outputs:\n");
            for o in required_outputs {
                s.push_str(&format!("  - {o}\n"));
            }
        }
        if !rules.is_empty() {
            s.push_str("golden_rules:\n");
            for r in rules {
                s.push_str(&format!("  - rule_id: {}\n", r.rule_id));
                if let Some(c) = &r.cmd {
                    // YAML safe-quote the cmd so shell metacharacters
                    // don't blow up the parse.
                    s.push_str(&format!("    cmd: {}\n", yaml_quote(c)));
                }
                if let Some(p) = &r.pattern {
                    s.push_str(&format!("    pattern: {}\n", yaml_quote(p)));
                }
            }
        }
        s.push_str("---\n\n# task\n\nbody.\n");
        s
    }

    fn yaml_quote(v: &str) -> String {
        // Single-quote unconditionally so leading `[`, `{`, `*`, `&`,
        // `!`, `:`, etc. don't get interpreted as YAML structure
        // markers. These are test fixtures — quoting cost is zero.
        format!("'{}'", v.replace('\'', "''"))
    }

    fn rule_cmd(id: &str, cmd: &str) -> GoldenRule {
        GoldenRule {
            rule_id: id.to_string(),
            cmd: Some(cmd.to_string()),
            pattern: None,
        }
    }

    fn rule_pattern(id: &str, pattern: &str) -> GoldenRule {
        GoldenRule {
            rule_id: id.to_string(),
            cmd: None,
            pattern: Some(pattern.to_string()),
        }
    }

    #[test]
    fn enforce_returns_pass_for_empty_golden_rules() {
        let tmp = TempDir::new().unwrap();
        let template = template_with(vec![], vec![]);
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(report.is_pass());
        assert!(report.passed.is_empty());
        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn cmd_rule_passes_when_command_exits_zero() {
        let tmp = TempDir::new().unwrap();
        let template = template_with(vec![rule_cmd("zero_exit", "true")], vec![]);
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(report.is_pass(), "got {report:?}");
        assert_eq!(report.passed, vec!["zero_exit"]);
    }

    #[test]
    fn cmd_rule_violates_when_command_exits_nonzero() {
        let tmp = TempDir::new().unwrap();
        let template = template_with(
            vec![rule_cmd("expected_failure", "false")],
            vec![],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(!report.is_pass());
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule_id, "expected_failure");
        assert!(matches!(
            report.violations[0].kind,
            GoldenRuleKindLabel::Cmd
        ));
        assert!(report.violations[0].detail.starts_with("exit "));
    }

    #[test]
    fn cmd_rule_runs_in_project_dir() {
        // Verifies the working directory is project_dir, not whatever
        // CWD the orchestrator happens to have.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("marker.txt"), "hi").unwrap();
        let template = template_with(
            vec![rule_cmd("expects_marker", "test -f marker.txt")],
            vec![],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(report.is_pass(), "expects current_dir = project_dir; got {report:?}");
    }

    #[test]
    fn pattern_rule_violates_on_match_in_required_output() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ccteam")).unwrap();
        std::fs::write(
            tmp.path().join(".ccteam").join("implement-report.md"),
            "## Notes\n\nWe accidentally pasted AWS_SECRET=foo somewhere.\n",
        )
        .unwrap();
        let template = template_with(
            vec![rule_pattern("no_secrets", "AWS_SECRET")],
            vec![".ccteam/implement-report.md".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(!report.is_pass());
        assert_eq!(report.violations.len(), 1);
        let v = &report.violations[0];
        assert_eq!(v.rule_id, "no_secrets");
        assert!(matches!(v.kind, GoldenRuleKindLabel::Pattern));
        assert!(v.detail.contains("AWS_SECRET"));
        assert!(v.detail.contains(".ccteam/implement-report.md"));
        assert!(v.detail.contains(":3"));
    }

    #[test]
    fn pattern_rule_passes_when_no_match_in_required_outputs() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ccteam")).unwrap();
        std::fs::write(
            tmp.path().join(".ccteam").join("implement-report.md"),
            "## Notes\n\nNothing risky here.\n",
        )
        .unwrap();
        let template = template_with(
            vec![rule_pattern("no_secrets", "AWS_SECRET")],
            vec![".ccteam/implement-report.md".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(report.is_pass(), "got {report:?}");
        assert_eq!(report.passed, vec!["no_secrets"]);
    }

    #[test]
    fn pattern_rule_skips_glob_required_outputs_with_clear_reason() {
        // M2.3 docs allow glob in required_outputs (e.g. "src/**/*").
        // Pattern rules don't glob in this iteration — they should
        // report skipped, not crash.
        let tmp = TempDir::new().unwrap();
        let template = template_with(
            vec![rule_pattern("no_secrets", "AWS_SECRET")],
            vec!["src/**/*".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(report.is_pass(), "globs are skipped, not violated");
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("glob"));
    }

    #[test]
    fn pattern_rule_skips_when_required_outputs_empty() {
        let tmp = TempDir::new().unwrap();
        let template = template_with(
            vec![rule_pattern("no_secrets", "AWS_SECRET")],
            vec![],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("no required_outputs"));
    }

    #[test]
    fn pattern_rule_skips_invalid_regex() {
        // Don't propagate a regex compile error as a phase block —
        // surface as skipped so phase author sees the bad rule.
        let tmp = TempDir::new().unwrap();
        let template = template_with(
            vec![rule_pattern("bad_regex", "[unclosed")],
            vec!["whatever.md".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].rule_id, "bad_regex");
        assert!(report.skipped[0].reason.contains("invalid regex"));
    }

    #[test]
    fn pattern_rule_handles_missing_files_gracefully() {
        // If a required_output is listed but doesn't exist on disk,
        // the pattern rule shouldn't crash — at most skip with the
        // "all files unreadable" message if everything's missing.
        let tmp = TempDir::new().unwrap();
        let template = template_with(
            vec![rule_pattern("no_secrets", "AWS_SECRET")],
            vec!["does-not-exist.md".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        // 1 file, unreadable, scanned = 0 -> skipped per the
        // "no files scanned" branch.
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("no files scanned"));
    }

    #[test]
    fn mixed_rules_one_violation_blocks_phase_overall() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ccteam")).unwrap();
        std::fs::write(
            tmp.path().join(".ccteam").join("report.md"),
            "ok content",
        )
        .unwrap();
        let template = template_with(
            vec![
                rule_cmd("tests_pass", "true"),
                rule_cmd("typecheck", "false"),
                rule_pattern("no_secrets", "AWS_SECRET"),
            ],
            vec![".ccteam/report.md".into()],
        );
        let report = enforce(&template, tmp.path()).unwrap();
        assert!(!report.is_pass());
        assert_eq!(report.passed.len(), 2);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule_id, "typecheck");
    }

    #[test]
    fn report_serializes_to_stable_json() {
        // The report ends up in progress.jsonl — schema must be
        // serde-Serialize-stable across versions. Smoke check that the
        // tag fields land where downstream consumers expect.
        let v = GoldenRuleViolation {
            rule_id: "x".into(),
            kind: GoldenRuleKindLabel::Cmd,
            detail: "exit 1: nope".into(),
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"rule_id\":\"x\""));
        assert!(s.contains("\"kind\":\"cmd\""));
        assert!(s.contains("\"detail\":\"exit 1: nope\""));
    }

    /// Smoke test: full report shape is round-trippable so consumers
    /// can deserialize from progress.jsonl line entries.
    #[test]
    fn report_round_trip() {
        let r = GoldenRulesReport {
            passed: vec!["a".into()],
            violations: vec![GoldenRuleViolation {
                rule_id: "b".into(),
                kind: GoldenRuleKindLabel::Pattern,
                detail: "matched".into(),
            }],
            skipped: vec![GoldenRuleSkipped {
                rule_id: "c".into(),
                reason: "bad regex".into(),
            }],
        };
        let s = serde_json::to_string(&r).unwrap();
        let parsed: GoldenRulesReport = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, r);
    }

}
