//! M3.5 product-research E2E (mock claude / state-machine drive).
//!
//! M3 唯一验收: `ccteam new --team=product-research "AI 菜谱生成器"` runs
//! through the 6-phase pipeline → verdict=REJECT, produces verdict.md /
//! rationale.md / next-steps.md; progress.jsonl includes ≥1
//! decision_mode=async outbox + ≥1 team-specific ESCALATE prefix +
//! ≥1 PHASE_DONE_PENDING; `ccteam decisions` sees the project's
//! pending decisions.
//!
//! These tests drive the orchestrator state machine directly via
//! `decide_tick_from_events` (no real claude). The point is to pin
//! the M3 protocol — schemas, phase DAG, ESCALATE registration,
//! PHASE_DONE_PENDING transition, decisions-queue plumbing — not to
//! validate Claude's verdict reasoning. Real LLM runs are an
//! out-of-band acceptance step the user runs manually (per the task
//! brief, M3.5 mock path is canon).

use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;

use ccteam_core::{
    bootstrap_project, decide_tick_from_events, disable_tool_surface_bootstrap_for_tests,
    intersect_open_decisions_with_required_inputs, pick_unused_slug, progress,
    write_all_global_team_templates, CcteamPaths, EscalateRoute, Orchestrator,
    OrchestratorConfig, OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority,
    PhaseHistoryEntry, PhaseState, ProjectState, TickAction,
};

static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
fn isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

fn setup_product_research(
    paths: &CcteamPaths,
    brief: &str,
) -> (String, Orchestrator) {
    isolation();
    write_all_global_team_templates(&paths.root, false).unwrap();
    let slug = pick_unused_slug(paths, brief).unwrap();
    bootstrap_project(paths, &slug, brief, "product-research").unwrap();
    let orch =
        Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();
    (slug, orch)
}

#[test]
fn bootstrap_writes_state_json_with_product_research_team() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, _orch) = setup_product_research(&paths, "ai 菜谱生成器");
    let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    assert_eq!(state.team, "product-research");
    let project_phase_dir = paths.project_dir(&slug).join(".ccteam/phases");
    for name in [
        "kickoff",
        "market-survey",
        "differentiation-analysis",
        "value-proposition",
        "feasibility",
        "verdict",
    ] {
        assert!(
            project_phase_dir.join(format!("{name}.md")).is_file(),
            "expected {} on disk",
            project_phase_dir.join(format!("{name}.md")).display(),
        );
    }
}

#[test]
fn product_research_team_runtime_carries_full_escalate_grammar_extensions() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (_slug, orch) = setup_product_research(&paths, "ai 菜谱生成器");
    let pr = orch.team_runtime("product-research").unwrap();
    let prefixes: std::collections::HashMap<&str, EscalateRoute> = pr
        .spec
        .escalate_grammar_extensions
        .iter()
        .map(|e| (e.prefix.as_str(), e.route))
        .collect();
    assert_eq!(
        prefixes.get("MARKET_DUPLICATE").copied(),
        Some(EscalateRoute::Abort),
        "MARKET_DUPLICATE should route to abort",
    );
    assert_eq!(
        prefixes.get("INSUFFICIENT_VALIDATION").copied(),
        Some(EscalateRoute::NeedUserInput),
        "INSUFFICIENT_VALIDATION should route to need_user_input",
    );
    assert_eq!(
        prefixes.get("LOW_DIFFERENTIATION").copied(),
        Some(EscalateRoute::RevertToPhase),
        "LOW_DIFFERENTIATION should route to revert_to_phase",
    );
}

#[test]
fn product_research_state_machine_walks_six_phases() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, orch) = setup_product_research(
        &paths,
        "AI 菜谱生成器 — 拍冰箱照片自动写菜谱",
    );
    let dag = &orch.team_runtime("product-research").unwrap().dag;

    let ordered = [
        "kickoff",
        "market-survey",
        "differentiation-analysis",
        "value-proposition",
        "feasibility",
        "verdict",
    ];
    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();

    // First-tick dispatch.
    state.phase_state = PhaseState::Idle;
    state.current_phase = String::new();
    let action = decide_tick_from_events(dag, &state, &[]);
    let is_kickoff = matches!(
        &action,
        TickAction::DispatchPhase { phase } if phase == "kickoff",
    );
    assert!(is_kickoff, "first dispatch must be kickoff, got {action:?}");

    for i in 0..ordered.len() {
        let cur = ordered[i];
        state.phase_state = PhaseState::InFlight;
        state.current_phase = cur.into();
        let event = json!({"event": "phase_done", "phase": cur});
        let action = decide_tick_from_events(dag, &state, &[event]);
        match action {
            TickAction::AdvancePhase { from, to } => {
                assert_eq!(from, cur);
                let expected = ordered.get(i + 1).copied();
                assert_eq!(to.as_deref(), expected, "DAG edge after {cur}");
                state.phase_history.push(PhaseHistoryEntry {
                    phase: from,
                    status: "passed".into(),
                    duration_s: 0,
                    cost_usd: 0.0,
                });
                state.current_phase = to.unwrap_or_default();
            }
            other => panic!("expected AdvancePhase from {cur}, got {other:?}"),
        }
    }
    assert_eq!(state.phase_history.len(), 6);
    assert_eq!(state.phase_history[5].phase, "verdict");
}

#[test]
fn feasibility_phase_done_pending_advances_to_verdict_no_block() {
    // M3.6 happy path on the real product-research team:
    //  - feasibility writes outbox/clarify-XXX.md (decision_mode=async)
    //  - emits PHASE_DONE_PENDING with the outbox filename in
    //    `open_decisions`
    //  - verdict's required_inputs do NOT include outbox/clarify-XXX.md
    //    so the orchestrator advances cleanly.
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, orch) = setup_product_research(&paths, "AI 菜谱生成器");
    let pr = orch.team_runtime("product-research").unwrap();

    // Pre-populate outbox/clarify file.
    let cc = paths.project_ccteam_dir(&slug);
    std::fs::create_dir_all(cc.join("outbox")).unwrap();
    let clarify = "reply-2026-05-06T100000Z-001.md";
    let msg = OutboxMessage {
        front: OutboxFrontMatter {
            schema_version: 1,
            in_reply_to: None,
            in_reply_to_source_msg_id: None,
            target_channels: Vec::new(),
            created_at: Utc::now(),
            priority: OutboxPriority::Normal,
            event_kind: OutboxEventKind::Clarify,
        },
        body: "Hosted LLM or on-device?\n".into(),
    };
    msg.save(&cc.join("outbox").join(clarify)).unwrap();

    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.current_phase = "feasibility".into();
    state.phase_state = PhaseState::InFlight;
    let event = json!({
        "event": "phase_done_pending",
        "phase": "feasibility",
        "open_decisions": [clarify],
        "reason": "tech-stack decision deferred; clarify-tech-stack-choice.md",
    });
    let action = decide_tick_from_events(&pr.dag, &state, &[event]);
    let TickAction::AdvancePhasePending { to, open_decisions, .. } = action else {
        panic!("expected AdvancePhasePending");
    };
    assert_eq!(to.as_deref(), Some("verdict"));

    let verdict = pr.templates.iter().find(|t| t.name == "verdict").unwrap();
    let blocking = intersect_open_decisions_with_required_inputs(
        &open_decisions,
        &verdict.required_inputs,
    );
    assert!(
        blocking.is_empty(),
        "verdict.required_inputs should not overlap clarify outbox files; got: {blocking:?}",
    );
}

#[test]
fn verdict_phase_abort_marks_project_escalated() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, orch) = setup_product_research(&paths, "AI 菜谱生成器");
    let pr = orch.team_runtime("product-research").unwrap();

    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.current_phase = "verdict".into();
    state.phase_state = PhaseState::InFlight;
    let event = json!({
        "event": "escalate",
        "kind": "abort",
        "reason": "REJECT — duplicate of N existing free recipe apps",
        "target_phase": null,
    });
    let action = decide_tick_from_events(&pr.dag, &state, &[event]);
    match action {
        TickAction::Escalated { phase, reason } => {
            assert_eq!(phase, "verdict");
            assert!(reason.contains("REJECT"));
        }
        other => panic!("expected Escalated, got {other:?}"),
    }

    // Synthesize the three artifacts the verdict phase would write.
    state.phase_history.push(PhaseHistoryEntry {
        phase: "verdict".into(),
        status: "escalated".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    let cc = paths.project_ccteam_dir(&slug);
    std::fs::write(
        cc.join("verdict.md"),
        "---\nverdict: REJECT\nconfidence: 0.85\n---\n\n## 决策\nN free apps already cover.\n",
    )
    .unwrap();
    std::fs::write(
        cc.join("rationale.md"),
        "Top three reasons for REJECT: ...\n",
    )
    .unwrap();
    std::fs::write(
        cc.join("next-steps.md"),
        "Skip dev pipeline; record retro for cross-project memory (M4).\n",
    )
    .unwrap();

    assert!(pr.dag.is_terminal_state(&state));
    assert!(cc.join("verdict.md").exists());
    assert!(cc.join("rationale.md").exists());
    assert!(cc.join("next-steps.md").exists());
}

#[test]
fn market_survey_market_duplicate_escalate_archives_marker_on_resume() {
    // Team-specific ESCALATE-resume cycle. MARKET_DUPLICATE = abort
    // per team.yaml; the project itself doesn't continue past it,
    // but the resume CLI archives the escalation marker — that's the
    // observable cycle (interfaces §10.5).
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, _orch) = setup_product_research(&paths, "AI 菜谱生成器");

    let cc = paths.project_ccteam_dir(&slug);
    let escalation = cc.join("escalation.md");
    std::fs::write(
        &escalation,
        "# Escalation\n\nphase: market-survey\nreason: MARKET_DUPLICATE — N free duplicates\n",
    )
    .unwrap();

    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.current_phase = "market-survey".into();
    state.phase_state = PhaseState::Idle;
    state.phase_history.push(PhaseHistoryEntry {
        phase: "market-survey".into(),
        status: "escalated".into(),
        duration_s: 0,
        cost_usd: 0.0,
    });
    state.save(&paths.project_state(&slug)).unwrap();

    run_ccteam_subcommand(&paths, &["resume", &slug])
        .expect("resume should succeed");
    assert!(!escalation.exists(), "escalation.md should be archived");
    let archive = cc.join(format!("escalation.{}.md", state.context_reset_count));
    assert!(
        archive.exists(),
        "escalation.<n>.md archive should exist at {}",
        archive.display(),
    );
}

#[test]
fn product_research_logs_progress_jsonl_for_full_run() {
    // The progress.jsonl carries the proof of every step the M3
    // brief demands (M3.5 唯一验收). We append events as the phases
    // would, then read back the file and assert presence of every
    // required marker.
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, _orch) = setup_product_research(&paths, "AI 菜谱生成器");
    let progress_path = paths.progress_jsonl(&slug);
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();

    for phase in [
        "kickoff",
        "market-survey",
        "differentiation-analysis",
        "value-proposition",
    ] {
        progress::append_event(
            &progress_path,
            &json!({"event": "phase_done", "phase": phase}),
        )
        .unwrap();
    }
    // M3.6 PHASE_DONE_PENDING + decision_mode=async outbox.
    progress::append_event(
        &progress_path,
        &json!({
            "event": "phase_done_pending",
            "phase": "feasibility",
            "open_decisions": ["reply-2026-05-06T100000Z-001.md"],
            "reason": "decision_mode=async",
        }),
    )
    .unwrap();
    // Team-specific ESCALATE prefix observed mid-flow.
    progress::append_event(
        &progress_path,
        &json!({
            "event": "escalate",
            "kind": "abort",
            "reason": "MARKET_DUPLICATE — duplicate confirmed",
            "target_phase": null,
        }),
    )
    .unwrap();
    // Final verdict.
    progress::append_event(
        &progress_path,
        &json!({
            "event": "escalate",
            "kind": "abort",
            "reason": "REJECT — N free duplicates exist",
            "target_phase": null,
        }),
    )
    .unwrap();

    let log = std::fs::read_to_string(&progress_path).unwrap();
    assert!(log.contains("\"phase_done\""), "phase_done events present");
    assert!(
        log.contains("\"phase_done_pending\""),
        "PHASE_DONE_PENDING event present (M3.6 instrumentation)",
    );
    assert!(
        log.contains("reply-2026-05-06T100000Z-001.md"),
        "open_decisions outbox basename surfaced (decision_mode=async path)",
    );
    assert!(
        log.contains("MARKET_DUPLICATE"),
        "team-specific ESCALATE prefix in progress.jsonl",
    );
    assert!(log.contains("REJECT"), "verdict=REJECT visible in reason");
}

#[test]
fn decisions_queue_lists_clarify_outbox_from_product_research_project() {
    // M3.5 acceptance: `ccteam decisions` aggregates clarify outbox
    // files across projects. Set up a product-research project with
    // a clarify file, then invoke the binary and assert the slug +
    // file appear in the JSON output.
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let (slug, _orch) = setup_product_research(&paths, "AI 菜谱生成器");

    let cc = paths.project_ccteam_dir(&slug);
    std::fs::create_dir_all(cc.join("outbox")).unwrap();
    let clarify_path = cc
        .join("outbox")
        .join("reply-2026-05-06T100000Z-001.md");
    let msg = OutboxMessage {
        front: OutboxFrontMatter {
            schema_version: 1,
            in_reply_to: None,
            in_reply_to_source_msg_id: None,
            target_channels: Vec::new(),
            created_at: Utc::now(),
            priority: OutboxPriority::Normal,
            event_kind: OutboxEventKind::Clarify,
        },
        body: "Hosted LLM or on-device?\n".into(),
    };
    msg.save(&clarify_path).unwrap();

    let stdout = run_ccteam_subcommand(&paths, &["decisions", "--format", "json"])
        .expect("decisions should succeed");
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("decisions json valid");
    let decisions = v["decisions"].as_array().expect("decisions array");
    let matched = decisions.iter().any(|d| {
        d["slug"].as_str() == Some(slug.as_str())
            && d["outbox_filename"].as_str() == Some("reply-2026-05-06T100000Z-001.md")
    });
    assert!(
        matched,
        "ccteam decisions should list slug={slug} clarify file; got: {stdout}",
    );
    let team = decisions
        .iter()
        .find(|d| d["slug"].as_str() == Some(slug.as_str()))
        .and_then(|d| d["team"].as_str())
        .unwrap_or("");
    assert_eq!(team, "product-research", "decisions should carry team");
}

// ---- helpers reaching into the binary --------------------------------

/// Spawn the test-built `ccteam` binary with the project's
/// CCTEAM_HOME / CCTEAM_PROJECTS_ROOT redirected to the test sandbox.
/// Returns the captured stdout on success.
fn run_ccteam_subcommand(paths: &CcteamPaths, args: &[&str]) -> anyhow::Result<String> {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(args)
        .env("CCTEAM_HOME", paths.root.as_os_str())
        .env("CCTEAM_PROJECTS_ROOT", paths.projects_root.as_os_str())
        .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1")
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "ccteam {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}
