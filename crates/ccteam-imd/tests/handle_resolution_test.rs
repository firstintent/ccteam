//! Unit tests for the chat-handle resolution rules shared by
//! `build_handle_map`, the unknown-handle reply path, and the
//! `@ccteam list bots` admin keyword.
//!
//! Covers:
//! - `chat_handle` absent → handle falls back to `role`
//! - `chat_handle` set → it wins
//! - cross-slug collision → second claimant gets `<handle>@<slug>`
//! - sort order is deterministic regardless of input order
//! - `available_handles_for_chat` filters by `(channel, chat_id)`

use ccteam_core::harness::AgentVendor;
use ccteam_imd::router::{available_handles_for_chat, format_unknown_handle_reply};
use ccteam_imd::BotRegistration;

fn mk(slug: &str, role: &str, handle: Option<&str>, platform: &str, chat: &str) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: platform.into(),
        im_chat_id: chat.into(),
        chat_handle: handle.map(String::from),
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn effective_handle_falls_back_to_role_when_chat_handle_absent() {
    let bot = mk("dev-foo", "lead", None, "telegram", "@g1");
    assert_eq!(bot.effective_handle(), "lead");
}

#[test]
fn effective_handle_prefers_chat_handle_when_present() {
    let bot = mk("dev-foo", "lead", Some("curie"), "telegram", "@g1");
    assert_eq!(bot.effective_handle(), "curie");
}

#[test]
fn available_handles_for_chat_filters_by_platform_and_chat() {
    let bots = vec![
        mk("a", "lead", Some("curie"), "telegram", "@g1"),
        mk("b", "lead", Some("galileo"), "telegram", "@g2"),
        mk("c", "lead", Some("kepler"), "slack", "@g1"),
    ];
    let out = available_handles_for_chat(&bots, "telegram", "@g1");
    assert_eq!(out, vec!["curie".to_string()]);
}

#[test]
fn available_handles_for_chat_no_match_returns_empty() {
    let bots = vec![mk("a", "lead", Some("curie"), "telegram", "@g1")];
    let out = available_handles_for_chat(&bots, "telegram", "@nowhere");
    assert!(out.is_empty());
}

#[test]
fn cross_slug_collision_assigns_suffix_to_second_claimant() {
    // Two bots, same effective handle (`curie`), different slugs in
    // the same chat. The slug-sorted first one (`alpha`) keeps the
    // bare handle; `beta` gets the `@beta` suffix.
    let bots = vec![
        mk("beta", "lead", Some("curie"), "telegram", "@g1"),
        mk("alpha", "lead", Some("curie"), "telegram", "@g1"),
    ];
    let mut out = available_handles_for_chat(&bots, "telegram", "@g1");
    out.sort();
    assert_eq!(out, vec!["curie".to_string(), "curie@beta".to_string()]);
}

#[test]
fn collision_resolution_is_deterministic_across_input_order() {
    let a = mk("alpha", "lead", Some("curie"), "telegram", "@g1");
    let b = mk("beta", "lead", Some("curie"), "telegram", "@g1");

    let order1 = vec![a.clone(), b.clone()];
    let order2 = vec![b, a];

    let mut h1 = available_handles_for_chat(&order1, "telegram", "@g1");
    let mut h2 = available_handles_for_chat(&order2, "telegram", "@g1");
    h1.sort();
    h2.sort();
    assert_eq!(h1, h2);
    // `alpha` < `beta` so alpha keeps the bare `curie` regardless of
    // which Vec order the caller passes.
    assert!(h1.contains(&"curie".to_string()));
    assert!(h1.contains(&"curie@beta".to_string()));
}

#[test]
fn collision_uses_chat_handle_or_role_fallback_consistently() {
    // One bot uses chat_handle="lead", another falls back to role
    // "lead". Same effective handle → second claimant suffixed.
    let bots = vec![
        mk("alpha", "lead", None, "telegram", "@g1"), // role-fallback "lead"
        mk("beta", "other-role", Some("lead"), "telegram", "@g1"), // chat_handle "lead"
    ];
    let mut out = available_handles_for_chat(&bots, "telegram", "@g1");
    out.sort();
    assert_eq!(out, vec!["lead".to_string(), "lead@beta".to_string()]);
}

#[test]
fn format_unknown_handle_reply_includes_typo_and_available() {
    let s = format_unknown_handle_reply("ghst", &["curie".to_string(), "galileo".to_string()]);
    assert!(s.contains("@ghst"));
    assert!(s.contains("@curie"));
    assert!(s.contains("@galileo"));
    assert!(s.contains("Available bots in this chat"));
}

#[test]
fn format_unknown_handle_reply_when_no_bots_says_none() {
    let s = format_unknown_handle_reply("ghost", &[]);
    assert!(s.contains("@ghost"));
    assert!(s.contains("No bots registered in this chat"));
}
