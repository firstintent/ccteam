//! V0.6.0 Wave 2 F114 — integration tests for the NL → CreatorMode
//! inferrer. Mirrors the cells of the decision table in
//! `docs/versions/v0-6-0/prd.md` F114 §"Phase 2: Mode inference" so a future
//! table edit forces a corresponding test update.

use ccteam_core::{infer_mode, CreatorMode, InferenceResult, Intent, Presence, Timeline};

fn intent(task: &str, presence: &str, timeline: &str) -> Intent {
    Intent {
        task_type: task.into(),
        presence: presence.into(),
        timeline: timeline.into(),
    }
}

#[test]
fn inproc_solo_full_attended_one_shot() {
    let r = infer_mode(&intent("coding", Presence::FULL_ATTENDED, Timeline::ONE_SHOT));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::InProc));
}

#[test]
fn inproc_team_partial_one_shot() {
    let r = infer_mode(&intent("coding", Presence::PARTIAL, Timeline::ONE_SHOT));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::InProc));
}

#[test]
fn bg_overnight_hands_off() {
    let r = infer_mode(&intent(
        "monitoring",
        Presence::HANDS_OFF,
        Timeline::LONG_RUNNING,
    ));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::Bg));
}

#[test]
fn bg_partial_long_running() {
    let r = infer_mode(&intent("qa-loop", Presence::PARTIAL, Timeline::LONG_RUNNING));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::Bg));
}

#[test]
fn chat_dm_im_dm() {
    let r = infer_mode(&intent(
        "chat-assistant",
        Presence::IM_DM,
        Timeline::LONG_RUNNING,
    ));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::ChatDm));
}

#[test]
fn chat_group_im_group() {
    let r = infer_mode(&intent(
        "multi-bot-team",
        Presence::IM_GROUP,
        Timeline::LONG_RUNNING,
    ));
    assert_eq!(r, InferenceResult::Confident(CreatorMode::ChatGroup));
}

#[test]
fn ambiguous_full_attended_long_running() {
    let r = infer_mode(&intent("coding", Presence::FULL_ATTENDED, Timeline::LONG_RUNNING));
    match r {
        InferenceResult::Ambiguous(candidates) => {
            assert!(candidates.len() >= 2);
            assert!(candidates.iter().any(|(m, _)| *m == CreatorMode::Bg));
        }
        other => panic!("expected ambiguous, got {other:?}"),
    }
}

#[test]
fn unknown_presence_needs_clarification() {
    let r = infer_mode(&intent("coding", "elsewhere", Timeline::ONE_SHOT));
    assert!(matches!(r, InferenceResult::NeedsClarification(_)));
}
