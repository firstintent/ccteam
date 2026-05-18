//! V0.6.0 Wave 2 F114 — integration tests for the scientist-nickname
//! pool used by ccteam-creator when minting bot handles.

use ccteam_core::{pick_unused_bot_name, SCIENTIST_NAMES};

#[test]
fn pool_is_at_least_fifty_names() {
    assert!(SCIENTIST_NAMES.len() >= 50, "pool too small: {}", SCIENTIST_NAMES.len());
}

#[test]
fn pick_first_unused_in_order() {
    // First three pool entries are Euclid, Archimedes, Ptolemy.
    let taken = vec!["Euclid".into(), "Archimedes".into()];
    let pick = pick_unused_bot_name(&taken);
    assert_eq!(pick, "Ptolemy");
}

#[test]
fn pick_case_insensitive_collision_detection() {
    let taken: Vec<String> = SCIENTIST_NAMES
        .iter()
        .take(5)
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let pick = pick_unused_bot_name(&taken);
    assert_eq!(pick, SCIENTIST_NAMES[5]);
}

#[test]
fn pool_has_no_two_token_names() {
    for &name in SCIENTIST_NAMES {
        assert!(
            !name.contains(' '),
            "two-token name in pool would break IM @-mention parsing: {name:?}"
        );
    }
}
