//! `ccteam doctor --verify-mcp` end-to-end tests.
//!
//! `--verify-mcp` introspects the live MCP tool surface registered by
//! `mcp_serve::tool_definitions()` and cross-checks the names against
//! `mcp_tool_groups::STUB_TOOLS`.
//!
//! Tests cover:
//!   - active tool count matches the v0.9 T1 spec (8)
//!   - `STUB_TOOLS` is empty (asserted via `stub_count: 0`)
//!   - human-readable output schema (header / per-group / verdict)
//!   - JSON output mode (`--json`)
//!   - exit code 0 on clean tree (no STUBs) — both modes
//!   - per-group breakdown carries every shipped group with active
//!     + stub keys

use serde_json::Value;
use std::process::Command;

fn run_doctor_verify_mcp(extra_args: &[&str]) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let mut cmd = Command::new(bin);
    cmd.arg("doctor").arg("--verify-mcp");
    for a in extra_args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("spawn ccteam");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn active_count_is_8_and_stub_count_is_0_on_clean_tree() {
    // status 1 + beacon alias 1 + chat 1 + session 5 = 8. STUB_TOOLS empty.
    let (stdout, stderr, code) = run_doctor_verify_mcp(&["--json"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(v["ok"], Value::Bool(true), "{v}");
    assert_eq!(v["total_tools"], Value::Number(8.into()), "{v}");
    assert_eq!(v["active_count"], Value::Number(8.into()), "{v}");
    assert_eq!(v["stub_count"], Value::Number(0.into()), "{v}");
    assert!(v["unexpected_stubs"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn json_output_schema_includes_per_group_and_tool_list() {
    let (stdout, _stderr, code) = run_doctor_verify_mcp(&["--json"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    // Required top-level keys.
    for key in [
        "ok",
        "total_tools",
        "active_count",
        "stub_count",
        "tool_list",
        "per_group",
        "unexpected_stubs",
    ] {
        assert!(v.get(key).is_some(), "missing top-level key `{key}` in {v}");
    }
    // Every shipped group with ≥1 tool is represented with `active` +
    // `stub` keys. Culled `advise` and retired `workflow` groups do not
    // appear (per_group is built from live tools only).
    for group in ["admin", "chat", "session"] {
        let g = v["per_group"].get(group).unwrap_or_else(|| {
            panic!("per_group missing `{group}` in {v}");
        });
        assert!(g.get("active").is_some(), "{g}");
        assert!(g.get("stub").is_some(), "{g}");
    }
    assert!(
        v["per_group"].get("workflow").is_none(),
        "retired workflow group must not appear in per_group: {v}"
    );
    assert!(
        v["per_group"].get("advise").is_none(),
        "culled advise group must not appear in per_group: {v}"
    );
    // Tool list length matches total_tools.
    let list = v["tool_list"].as_array().unwrap();
    assert_eq!(list.len(), v["total_tools"].as_u64().unwrap() as usize);
    // List is sorted.
    let names: Vec<&str> = list.iter().map(|v| v.as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "tool_list must be sorted for stable output");
    // Spot-check a known tool from each surviving group is present.
    assert!(names.contains(&"status"));
    assert!(names.contains(&"grok_claude_codex_kimi"));
    assert!(!names.contains(&concat!("claude_codex_grok_kimi_", "opencode_status")));
    assert!(names.contains(&"chat_send_file"));
    assert!(names.contains(&"session_spawn"));
    // Culled / retired tools are gone from the live surface.
    assert!(!names.contains(&"ccteam__admin_ls"));
    assert!(!names.contains(&"ccteam__advise_vote"));
    assert!(!names.contains(&"ccteam__chat_register_bot"));
    assert!(!names.contains(&"ccteam__workflow_show"));
    // 2026-07-26 cull: tmux-era pane screenshot tool is gone.
    assert!(!names.contains(&"screenshot"));
}

#[test]
fn human_readable_output_contains_verdict_pass_and_breakdown() {
    let (stdout, _stderr, code) = run_doctor_verify_mcp(&[]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("MCP tool surface verification"),
        "missing header: {stdout}"
    );
    assert!(
        stdout.contains("V0.6.6 F171"),
        "header must carry F171 marker for traceability: {stdout}",
    );
    assert!(stdout.contains("total tools:    8"), "got: {stdout}");
    assert!(stdout.contains("active:         8"), "got: {stdout}");
    assert!(stdout.contains("stubs:          0"), "got: {stdout}");
    assert!(stdout.contains("per-group breakdown:"), "got: {stdout}");
    // Verdict line on clean tree.
    assert!(
        stdout.contains("verdict: PASS"),
        "must print PASS verdict on clean tree: {stdout}",
    );
    // No JSON braces in human mode — guards against double-printing
    // when `--json` is unset.
    assert!(
        !stdout.trim_start().starts_with('{'),
        "human mode must not emit JSON: {stdout}",
    );
}

#[test]
fn exit_code_is_zero_when_no_unexpected_stubs() {
    // STUB_TOOLS is empty, so the gate must exit 0 both with and without
    // `--json`. Re-runs are idempotent.
    let (_so, _se, code_text) = run_doctor_verify_mcp(&[]);
    assert_eq!(code_text, 0, "human mode should exit 0");
    let (_so, _se, code_json) = run_doctor_verify_mcp(&["--json"]);
    assert_eq!(code_json, 0, "json mode should exit 0");
}

#[test]
fn json_mode_emits_pretty_printed_single_object() {
    // `--json` must be a parseable single JSON object (not JSONL). The
    // pretty-printed form makes the output diff-friendly when shipped
    // in CI logs.
    let (stdout, _stderr, code) = run_doctor_verify_mcp(&["--json"]);
    assert_eq!(code, 0);
    // First non-whitespace char must be `{` (object, not array / line-
    // delimited stream).
    assert_eq!(
        stdout.trim_start().chars().next().unwrap(),
        '{',
        "json mode must emit one JSON object, got: {stdout}",
    );
    // serde must parse the full body in one shot.
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON object");
    assert!(v.is_object(), "top-level value must be an object");
    // Pretty-printed (multi-line) — single-line output would suggest
    // we accidentally called `to_string` instead of `to_string_pretty`.
    let line_count = stdout.lines().count();
    assert!(
        line_count > 5,
        "expected pretty-printed JSON (multi-line), got {line_count} line(s): {stdout}",
    );
}

#[test]
fn human_mode_lists_every_shipped_group_with_active_count() {
    // Per-group breakdown is the load-bearing diff users will spot
    // when a group's tool count regresses. Verify every shipped group
    // appears with the right active count and `0 stub` suffix.
    let (stdout, _stderr, code) = run_doctor_verify_mcp(&[]);
    assert_eq!(code, 0);
    for (group, active) in [("admin:", 2), ("chat:", 1), ("session:", 5)] {
        let needle = format!("{group}    {active} active / 0 stub");
        // Allow extra padding on either side — exact spacing depends
        // on the longest group name. Use a relaxed contains check.
        assert!(
            stdout
                .lines()
                .any(|l| l.contains(group) && l.contains(&format!("{active} active / 0 stub"))),
            "missing per-group line for `{needle}` in:\n{stdout}",
        );
    }
}
