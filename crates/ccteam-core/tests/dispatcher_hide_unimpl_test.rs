//! V0.6.5 F159 — `/ccteam` dispatcher must directly hide unimplemented
//! intents rather than surface them with placeholder / "coming soon" /
//! `NotImplemented` fallback text.
//!
//! Regression guard on `skills/ccteam/SKILL.md`:
//!
//! - The skill body must not carry stale "Wave 1/2/3 fallback" /
//!   `STUB` / `NotImplemented` / 占位 / 准备中 / 待落地 phrasing.
//! - The skill body must declare the F159 red line — any new intent
//!   added to the dispatcher in the future must ship with its sub-skill
//!   body + MCP dispatch already real, never with placeholder fallback.
//! - Simulating "an intent backed only by an MCP stub" (the stub-status
//!   pattern below) verifies the SKILL.md text does not anywhere
//!   document that intent as a routable option behind a placeholder.
//!
//! The dispatcher itself is LLM-driven (no Rust dispatch function), so
//! this regression-guard test operates on the SKILL.md text body — the
//! only authoritative artifact for `/ccteam` routing behavior.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn dispatcher_skill_body() -> String {
    let path = repo_root().join("skills/ccteam/SKILL.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

#[test]
fn dispatcher_skill_carries_no_wave_or_stub_placeholder_phrasing() {
    // F159 + F161: the /ccteam dispatcher SKILL.md must not carry any
    // stale "Wave 1/2/3 fallback" or "STUB" / "NotImplemented" /
    // 占位 / 准备中 / 待落地 phrasing. Each of these phrases is a
    // placeholder pattern that would either route a user into a dead-end
    // or document one — both violate F159's red line.
    //
    // Match-list mirrors the F161 doc-sweep grep:
    //   grep -E "Wave [123]|STUB|NotImplemented|占位|准备中|待落地"
    let body = dispatcher_skill_body();
    let forbidden = [
        "Wave 1",
        "Wave 2",
        "Wave 3",
        "STUB",
        "NotImplemented",
        "占位",
        "准备中",
        "待落地",
        "wave2-not-ready",
    ];
    for pat in forbidden {
        assert!(
            !body.contains(pat),
            "skills/ccteam/SKILL.md must not contain placeholder/wave phrasing {pat:?} \
             (F159 red line: hide unimplemented intents, do not document fallbacks)",
        );
    }
}

#[test]
fn dispatcher_skill_declares_hide_unimpl_red_line() {
    // F159 ship gate: the red line "未实现 intent 直接隐藏不渲染"
    // must be present in SKILL.md so future maintainers see the
    // contract before adding a new intent.
    let body = dispatcher_skill_body();
    assert!(
        body.contains("未实现 intent"),
        "skills/ccteam/SKILL.md must declare F159 red line (未实现 intent)",
    );
    assert!(
        body.contains("直接隐藏不渲染"),
        "skills/ccteam/SKILL.md F159 red line must use 直接隐藏不渲染 phrasing",
    );
    assert!(
        body.contains("Ship gate"),
        "skills/ccteam/SKILL.md must define a Ship gate for new intents (F159)",
    );
}

#[test]
fn dispatcher_unimpl_intent_does_not_appear_in_routing_table() {
    // Simulate the scenario: "an intent label is backed only by a STUB
    // MCP dispatch on the daemon side". The F159 contract says such an
    // intent must NEVER appear in the dispatcher's Step 1 / Step 2 /
    // Step 3 surface.
    //
    // We pick a plausible-but-not-yet-shipped intent label
    // (`voice-input`, called out as V0.7+ in §"What this skill cannot
    // do") and assert it does NOT appear in any of the dispatcher's
    // intent enumerations.
    let body = dispatcher_skill_body();
    let unimpl_intent_labels = ["voice-input", "image-input", "multimodal"];
    for label in unimpl_intent_labels {
        // Surface-level routing table membership: the label must not
        // appear in the bold-intent column. We check the simpler
        // condition that the label is not used as an intent name in
        // routing tables (it can appear in the §"What this skill cannot
        // do" prose).
        let lower = body.to_lowercase();
        let routing_section_marker = "step 2: 路由到 sub-skill";
        if let Some(idx) = lower.find(routing_section_marker) {
            // Take the next 400 chars as the routing table region.
            let end = (idx + 400).min(body.len());
            let region = &body[idx..end];
            assert!(
                !region.to_lowercase().contains(label),
                "unimplemented intent {label:?} must not appear in dispatcher \
                 routing table (F159: hide unimpl intents directly)",
            );
        }
    }
}
