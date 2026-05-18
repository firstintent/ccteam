//! Integration coverage of sanitize layer (lib unit tests cover the
//! basics — this file exercises edge cases that should hold across
//! the whole crate boundary).

use ccteam_imd::sanitize::{sanitize_for_tmux, sanitize_reply_input, truncate_to_max, MAX_TURN_LEN};

#[test]
fn round_trip_with_all_dangerous_chars() {
    let raw = "before\x00 `whoami` $(rm) ${HOME} \u{202e}reverse\u{2069} after";
    let out = sanitize_reply_input(raw);
    for needle in [
        "\x00", "\u{202e}", "\u{2069}",
    ] {
        assert!(!out.contains(needle), "{needle:?} should have been stripped");
    }
    for esc in ["\\`whoami\\`", "\\$(", "\\${"] {
        assert!(out.contains(esc), "missing escape {esc}");
    }
}

#[test]
fn tmux_variant_idempotent_on_clean_input() {
    let clean = "hello world";
    assert_eq!(sanitize_for_tmux(clean), clean);
}

#[test]
fn truncate_respects_max_const() {
    let long = "x".repeat(MAX_TURN_LEN * 2);
    let cut = truncate_to_max(&long, MAX_TURN_LEN);
    assert_eq!(cut.chars().count(), MAX_TURN_LEN);
}

#[test]
fn empty_input_stays_empty() {
    assert_eq!(sanitize_reply_input(""), "");
    assert_eq!(sanitize_for_tmux(""), "");
}
