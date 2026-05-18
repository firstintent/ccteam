//! V0.6.0 Wave 2 F114 — natural-language intent → [`CreatorMode`]
//! rule-based inferrer used by the `ccteam-creator` skill.
//!
//! The skill's LLM body extracts three orthogonal axes from the user's
//! free-text request:
//!
//! - `task_type`  — coding / writing / chat-assistant / monitoring / …
//! - `presence`   — full-attended / partial / hands-off / im-dm / im-group
//! - `timeline`   — one-shot / hours / long-running
//!
//! and feeds them to [`infer_mode`] which collapses the cross-product
//! into one of the four [`CreatorMode`] variants (or
//! [`InferenceResult::Ambiguous`] / [`InferenceResult::NeedsClarification`]
//! when the signal is too weak).
//!
//! Keep this rule-based: the LLM has already done the hard NL parsing
//! upstream — this layer just enforces the V0.6.0 PRD decision table so
//! the skill's branching is reproducible across sessions (no hidden
//! prompt drift between Phase 2 invocations).
//!
//! See `docs/v0-6-0/prd.md` F114 §"Phase 2: Mode inference" for the
//! authoritative decision table.
//!
//! ## Naming note
//!
//! This module's [`CreatorMode`] is a **preset-axis** discriminator
//! distinct from `harness::ExecutionMode` (the runtime adapter axis).
//! Harness's `Chat` collapses DM + Group; the creator skill needs to
//! split them because the chosen workflow template is different
//! (`chat-pocket.yaml` vs `chat-squad.yaml`). Mapping:
//!
//! | [`CreatorMode`] | `harness::ExecutionMode` |
//! |-----------------|--------------------------|
//! | InProc          | InProc                   |
//! | Bg              | Bg                       |
//! | ChatDm          | Chat                     |
//! | ChatGroup       | Chat                     |

use serde::{Deserialize, Serialize};

/// User intent extracted by the skill's NL parser. All three fields
/// hold normalized lowercase tokens — see [`Presence`] /
/// [`Timeline`] constants below for the accepted vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// Domain of the work: `coding`, `writing`, `research`, `support`,
    /// `chat-assistant`, `multi-bot-team`, `qa-loop`, `scheduling`,
    /// `monitoring`, `other`. Free-form so future skill extensions can
    /// add categories without touching the enum.
    pub task_type: String,
    /// Where the user expects to be while the work runs. One of
    /// [`Presence::FULL_ATTENDED`], `PARTIAL`, `HANDS_OFF`, `IM_DM`,
    /// `IM_GROUP`. Unknown values fall through to
    /// [`InferenceResult::NeedsClarification`].
    pub presence: String,
    /// How long the work is expected to run. One of
    /// [`Timeline::ONE_SHOT`], `HOURS`, `LONG_RUNNING`. Unknown values
    /// fall through to [`InferenceResult::NeedsClarification`].
    pub timeline: String,
}

/// Accepted `presence` axis tokens. Match these case-insensitively at
/// the call site.
pub struct Presence;
impl Presence {
    pub const FULL_ATTENDED: &'static str = "full-attended";
    pub const PARTIAL: &'static str = "partial";
    pub const HANDS_OFF: &'static str = "hands-off";
    pub const IM_DM: &'static str = "im-dm";
    pub const IM_GROUP: &'static str = "im-group";
}

/// Accepted `timeline` axis tokens.
pub struct Timeline;
impl Timeline {
    pub const ONE_SHOT: &'static str = "one-shot";
    pub const HOURS: &'static str = "hours";
    pub const LONG_RUNNING: &'static str = "long-running";
}

/// V0.6.0 PRD F114 — coarse execution-mode discriminator the
/// `ccteam-creator` skill picks before fanning out to the
/// preset-specific `workflow.yaml` template.
///
/// Mapped 1:1 to the `WorkflowMode` variant the rendered yaml will
/// carry once tui-impl lands the `mode: chat` schema extension (Wave 2
/// task #2). Until then, `ChatDm` / `ChatGroup` render `mode: chat`
/// scalar text — full parse round-trip is gated on the schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CreatorMode {
    /// In-process Anthropic Agent Team: user runs `claude` locally and
    /// the team-lead spawns short-lived teammates inside the same
    /// session. Preset: Solo Sidekick (1 teammate) / Team Sprint (N).
    InProc,
    /// Background `claude --bg` jobs driven by the daemon /
    /// `ArtifactWatcher`. Preset: Overnight Builder.
    Bg,
    /// Long-running chat bot, single-user DM transport. Preset:
    /// Pocket Assistant.
    ChatDm,
    /// Long-running chat bot, multi-user group transport with
    /// bot-to-bot @ addressing. Preset: IM Squad.
    ChatGroup,
}

/// Outcome of [`infer_mode`]. Most well-formed `Intent`s return
/// [`InferenceResult::Confident`]; weak signal triggers
/// [`InferenceResult::Ambiguous`] (multi-candidate ranked by score) or
/// [`InferenceResult::NeedsClarification`] (single clarifying question
/// the skill should bounce back to the user).
#[derive(Debug, Clone, PartialEq)]
pub enum InferenceResult {
    /// One mode dominates — proceed to Phase 3 persona match.
    Confident(CreatorMode),
    /// Two-plus modes tied or close; surface ranked options to user.
    Ambiguous(Vec<(CreatorMode, f32)>),
    /// Insufficient signal — string is the question to ask user.
    NeedsClarification(String),
}

/// Collapse an [`Intent`] into an [`InferenceResult`] using the V0.6.0
/// PRD F114 decision table:
///
/// | presence       | timeline      | mode        |
/// |----------------|---------------|-------------|
/// | full-attended  | one-shot      | InProc      |
/// | full-attended  | hours         | InProc      |
/// | partial        | one-shot      | InProc      |
/// | partial        | hours         | Bg          |
/// | partial        | long-running  | Bg          |
/// | hands-off      | (any)         | Bg          |
/// | im-dm          | (any)         | ChatDm      |
/// | im-group       | (any)         | ChatGroup   |
///
/// Unknown presence/timeline tokens return
/// [`InferenceResult::NeedsClarification`]. The
/// `task_type` axis biases tie-breaks but never overrides the table:
/// `chat-assistant` boosts ChatDm score by 0.1 in
/// [`InferenceResult::Ambiguous`] output, `multi-bot-team` boosts
/// ChatGroup, `qa-loop` boosts Bg.
pub fn infer_mode(intent: &Intent) -> InferenceResult {
    let presence = intent.presence.to_ascii_lowercase();
    let timeline = intent.timeline.to_ascii_lowercase();
    let task_type = intent.task_type.to_ascii_lowercase();

    // IM presence pinned to chat modes regardless of timeline (a chat
    // bot is always long-running by nature).
    match presence.as_str() {
        Presence::IM_DM => return InferenceResult::Confident(CreatorMode::ChatDm),
        Presence::IM_GROUP => return InferenceResult::Confident(CreatorMode::ChatGroup),
        _ => {}
    }

    // Hands-off → background, no timeline ambiguity worth re-asking.
    if presence == Presence::HANDS_OFF {
        return InferenceResult::Confident(CreatorMode::Bg);
    }

    // Validate axis tokens; clarify if unknown.
    let known_presence = matches!(
        presence.as_str(),
        Presence::FULL_ATTENDED | Presence::PARTIAL
    );
    let known_timeline = matches!(
        timeline.as_str(),
        Timeline::ONE_SHOT | Timeline::HOURS | Timeline::LONG_RUNNING
    );
    if !known_presence {
        return InferenceResult::NeedsClarification(format!(
            "你打算全程盯着跑(`full-attended`)还是偶尔回来看(`partial`)还是关机让它跑(`hands-off`)?(收到:{presence:?})"
        ));
    }
    if !known_timeline {
        return InferenceResult::NeedsClarification(format!(
            "这活儿是一次性(`one-shot`)、跑几小时(`hours`)、还是 24/7 长跑(`long-running`)?(收到:{timeline:?})"
        ));
    }

    // Core table — full-attended / partial × one-shot / hours / long.
    let mode = match (presence.as_str(), timeline.as_str()) {
        (Presence::FULL_ATTENDED, Timeline::ONE_SHOT) => CreatorMode::InProc,
        (Presence::FULL_ATTENDED, Timeline::HOURS) => CreatorMode::InProc,
        (Presence::FULL_ATTENDED, Timeline::LONG_RUNNING) => {
            // Long-running + user staying full-attended is unusual —
            // surface ambiguity rather than silently picking one.
            let mut scores = vec![
                (CreatorMode::Bg, 0.6),
                (CreatorMode::InProc, 0.4),
            ];
            if task_type == "qa-loop" {
                scores[0].1 += 0.1;
            }
            return InferenceResult::Ambiguous(scores);
        }
        (Presence::PARTIAL, Timeline::ONE_SHOT) => CreatorMode::InProc,
        (Presence::PARTIAL, Timeline::HOURS) => CreatorMode::Bg,
        (Presence::PARTIAL, Timeline::LONG_RUNNING) => CreatorMode::Bg,
        _ => unreachable!("validated above"),
    };
    InferenceResult::Confident(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(task: &str, presence: &str, timeline: &str) -> Intent {
        Intent {
            task_type: task.into(),
            presence: presence.into(),
            timeline: timeline.into(),
        }
    }

    #[test]
    fn full_attended_one_shot_is_inproc() {
        let r = infer_mode(&intent("coding", "full-attended", "one-shot"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::InProc));
    }

    #[test]
    fn partial_hours_is_bg() {
        let r = infer_mode(&intent("qa-loop", "partial", "hours"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::Bg));
    }

    #[test]
    fn hands_off_long_is_bg() {
        let r = infer_mode(&intent("monitoring", "hands-off", "long-running"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::Bg));
    }

    #[test]
    fn im_dm_is_chat_dm_regardless_of_timeline() {
        let r = infer_mode(&intent("chat-assistant", "im-dm", "one-shot"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::ChatDm));
        let r = infer_mode(&intent("chat-assistant", "im-dm", "long-running"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::ChatDm));
    }

    #[test]
    fn im_group_is_chat_group() {
        let r = infer_mode(&intent("multi-bot-team", "im-group", "long-running"));
        assert_eq!(r, InferenceResult::Confident(CreatorMode::ChatGroup));
    }

    #[test]
    fn full_attended_long_running_is_ambiguous() {
        let r = infer_mode(&intent("coding", "full-attended", "long-running"));
        match r {
            InferenceResult::Ambiguous(scores) => {
                assert!(scores.iter().any(|(m, _)| *m == CreatorMode::Bg));
                assert!(scores.iter().any(|(m, _)| *m == CreatorMode::InProc));
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn unknown_presence_asks_clarification() {
        let r = infer_mode(&intent("coding", "maybe", "one-shot"));
        assert!(matches!(r, InferenceResult::NeedsClarification(_)));
    }

    #[test]
    fn unknown_timeline_asks_clarification() {
        let r = infer_mode(&intent("coding", "partial", "forever"));
        assert!(matches!(r, InferenceResult::NeedsClarification(_)));
    }
}
