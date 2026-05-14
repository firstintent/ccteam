//! Fix-loop state file (`<project>/.ccteam/auto-loop.state.md`) and the
//! pure decision function the Stop hook uses to drive the ralph-loop
//! retry pattern (tech-design §3.5).
//!
//! Format: a YAML front matter (counters + completion signal +
//! timestamps) followed by the fix prompt body. The Stop hook re-feeds
//! the body verbatim until either the assistant prints
//! `completion_signal` or `iteration` reaches `max_iterations`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoLoopFrontMatter {
    pub slug: String,
    pub iteration: u32,
    pub max_iterations: u32,
    pub completion_signal: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Full auto-loop state: front matter + the prompt body that the Stop
/// hook re-feeds on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoLoopState {
    pub front: AutoLoopFrontMatter,
    pub prompt: String,
}

impl AutoLoopState {
    pub fn new(
        slug: String,
        prompt: String,
        max_iterations: u32,
        completion_signal: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            front: AutoLoopFrontMatter {
                slug,
                iteration: 1,
                max_iterations,
                completion_signal,
                created_at: now,
                updated_at: now,
            },
            prompt,
        }
    }
}

/// `<project>/.ccteam/auto-loop.state.md`.
pub fn path_in(project_dir: &Path) -> PathBuf {
    project_dir.join(".ccteam").join("auto-loop.state.md")
}

pub fn read(path: &Path) -> Result<Option<AutoLoopState>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let after = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("auto-loop state missing leading ---"))?;
    let end = after
        .find("\n---\n")
        .or_else(|| after.find("\n---\r\n"))
        .ok_or_else(|| anyhow!("auto-loop state missing closing ---"))?;
    let front_str = &after[..end];
    let front: AutoLoopFrontMatter = serde_yaml::from_str(front_str)
        .with_context(|| format!("parse auto-loop front matter at {}", path.display()))?;
    let prompt = after[end + "\n---\n".len()..].trim().to_string();
    Ok(Some(AutoLoopState { front, prompt }))
}

pub fn write(path: &Path, state: &AutoLoopState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let front_yaml =
        serde_yaml::to_string(&state.front).context("serialize auto-loop front matter")?;
    let body = format!("---\n{front_yaml}---\n\n{}\n", state.prompt);
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

pub fn delete(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
}

/// Pure ralph-loop decision: should the Stop hook block the exit and
/// re-feed the prompt, or allow the exit (signalling success or
/// max-iterations exhaustion)?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoLoopDecision {
    /// Re-feed the prompt; bump `iteration` to `next_iteration` and
    /// rewrite the state file before the hook returns.
    Reinject { prompt: String, next_iteration: u32 },
    /// Allow exit. `succeeded = true` when the completion signal was
    /// observed, `false` when iteration reached the cap without it
    /// (caller emits `escalate`).
    AllowExit { succeeded: bool },
}

pub fn decide(state: &AutoLoopState, last_assistant_text: &str) -> AutoLoopDecision {
    if last_assistant_text.contains(&state.front.completion_signal) {
        return AutoLoopDecision::AllowExit { succeeded: true };
    }
    if state.front.iteration >= state.front.max_iterations {
        return AutoLoopDecision::AllowExit { succeeded: false };
    }
    AutoLoopDecision::Reinject {
        prompt: state.prompt.clone(),
        next_iteration: state.front.iteration + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(iter: u32) -> AutoLoopState {
        let mut s = AutoLoopState::new(
            "demo".into(),
            "fix the broken tests in src/db.rs".into(),
            3,
            "TESTS_GREEN".into(),
        );
        s.front.iteration = iter;
        s
    }

    #[test]
    fn roundtrip_write_read() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("auto-loop.state.md");
        let original = sample(1);
        write(&p, &original).unwrap();
        let loaded = read(&p).unwrap().unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("nope.md");
        assert!(read(&p).unwrap().is_none());
    }

    #[test]
    fn decide_reinjects_when_below_cap_and_signal_absent() {
        let s = sample(1);
        match decide(&s, "I'm working on it...") {
            AutoLoopDecision::Reinject {
                prompt,
                next_iteration,
            } => {
                assert_eq!(next_iteration, 2);
                assert!(prompt.contains("fix the broken tests"));
            }
            other => panic!("expected Reinject, got {other:?}"),
        }
    }

    #[test]
    fn decide_allows_exit_on_completion_signal() {
        let s = sample(2);
        let txt = "Re-ran cargo test, everything green. TESTS_GREEN";
        assert_eq!(
            decide(&s, txt),
            AutoLoopDecision::AllowExit { succeeded: true },
        );
    }

    #[test]
    fn decide_allows_exit_unsuccessful_at_iteration_cap() {
        let s = sample(3); // max_iterations is 3
        assert_eq!(
            decide(&s, "still failing"),
            AutoLoopDecision::AllowExit { succeeded: false },
        );
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("absent.md");
        delete(&p).unwrap();
        delete(&p).unwrap();
    }
}
