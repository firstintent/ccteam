//! Tests for the `CCTEAM_DISABLE_TOOLS` group filter.
//!
//! Group enum: `admin` / `workflow` / `chat` / `session`. The `workflow` token
//! stays a valid enum value but gates an empty set. The `advise` group was
//! dropped in v0.9 T1 and `screenshot` was culled 2026-07-26 — both tokens are
//! now silently ignored like any unknown. Unknown tokens are dropped rather
//! than rejected: the filter is best-effort UX, not a security boundary.
//!
//! Each case used to spawn a `ccteam internal mcp-serve` child with a
//! different env value. That transport is deleted, and the filter takes the
//! parsed spec as an argument, so the same cases run as direct calls — no
//! subprocess, no process-global env mutation to serialize.

use std::collections::HashSet;

use ccteam_im::mcp::{filter_by_disabled, group_for_tool, parse_disable_env, tool_definitions};

/// Tool names surviving the filter for a given `CCTEAM_DISABLE_TOOLS` value.
fn names_with_disable(disable: Option<&str>) -> Vec<String> {
    let disabled = parse_disable_env(disable);
    filter_by_disabled(tool_definitions(), &disabled)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The set of group names the surviving tools belong to.
fn group_set(names: &[String]) -> HashSet<String> {
    names
        .iter()
        .filter_map(|n| group_for_tool(n))
        .map(|g| g.as_str().to_string())
        .collect()
}

#[test]
fn disable_unset_returns_all_visible_groups() {
    // workflow gates an empty set (never appears). advise + screenshot were
    // culled. Visible surface: admin + chat + session.
    let names = names_with_disable(None);
    let groups = group_set(&names);
    for g in ["admin", "chat", "session"] {
        assert!(
            groups.contains(g),
            "default tools/list should contain group `{g}`; got groups {groups:?}"
        );
    }
    assert!(
        !groups.contains("workflow"),
        "workflow group has no tools; got groups {groups:?}"
    );
    assert!(
        !groups.contains("advise"),
        "advise group was culled in v0.9 T1; got groups {groups:?}"
    );
    assert!(
        !names.contains(&"screenshot".to_string()),
        "screenshot was culled 2026-07-26; got {names:?}"
    );
    assert_eq!(names.len(), 8);
}

#[test]
fn disable_chat_hides_chat_keeps_others() {
    let names = names_with_disable(Some("chat"));
    let groups = group_set(&names);
    assert!(!groups.contains("chat"), "chat group should be hidden");
    for g in ["admin", "session"] {
        assert!(groups.contains(g), "group `{g}` should still be present");
    }
    assert!(!names.iter().any(|n| n.starts_with("chat_")));
    assert!(names.contains(&"status".to_string()));
}

#[test]
fn disable_chat_with_stale_screenshot_token_still_works() {
    // A stale `screenshot` token (group culled 2026-07-26) parses as unknown
    // and is ignored; the rest of the list still applies.
    let names = names_with_disable(Some("chat,screenshot"));
    let groups = group_set(&names);
    assert!(!groups.contains("chat"));
    assert!(groups.contains("admin"));
    assert!(groups.contains("session"));
}

#[test]
fn disable_each_group_individually() {
    // Confirms the enum parser covers every documented value.
    for g in ["admin", "workflow", "chat", "session"] {
        let names = names_with_disable(Some(g));
        let groups = group_set(&names);
        assert!(
            !groups.contains(g),
            "disable `{g}` should hide that group; got groups {groups:?}"
        );
    }
}

#[test]
fn disable_unknown_token_is_silently_ignored() {
    // Unknown tokens (including retired `advise`) are dropped silently so a
    // typo cannot take the tool surface down.
    let baseline = names_with_disable(None);
    let with_typo = names_with_disable(Some("not-a-real-group,also-fake,advise"));
    assert_eq!(
        baseline, with_typo,
        "unknown disable tokens must be a no-op"
    );
}

#[test]
fn disable_all_groups_returns_empty_list() {
    let names = names_with_disable(Some("admin,workflow,chat,session"));
    assert!(
        names.is_empty(),
        "disabling every group should hide the entire surface; got {names:?}"
    );
}

#[test]
fn disable_workflow_preserves_other_groups() {
    // Sanity: workflow gates only its own (empty) set — disabling it must not
    // collaterally hide any live group.
    let names = names_with_disable(Some("workflow"));
    let groups = group_set(&names);
    assert!(!groups.contains("workflow"));
    for g in ["admin", "chat", "session"] {
        assert!(groups.contains(g), "group `{g}` should still be present");
    }
    assert_eq!(names.len(), 8);
}
