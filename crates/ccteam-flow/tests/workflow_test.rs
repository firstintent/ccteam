//! V0.4.0 F63 — `workflow.yaml` schema + parser tests.
//!
//! Coverage matrix (15 cases) — see `docs/versions/v0-4-0/dev-plan.md` §5.1 #4.6.

use std::path::{Path, PathBuf};

use ccteam_flow::workflow::{Executor, Trigger, WorkflowError, WorkflowSpec};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

// ---------------------------------------------------------------------
// t01 — Load the full ui-quality-loop fixture (4 agents + mixed triggers).
// ---------------------------------------------------------------------
#[test]
fn t01_load_valid_yaml() {
    let path = fixture_dir().join("workflow-ui-quality-loop.yaml");
    let spec = WorkflowSpec::load(&path).expect("ui-quality-loop should parse");

    assert_eq!(spec.name, "ui-quality-loop");
    assert!(spec.description.as_deref().unwrap_or("").contains("UI"));
    assert_eq!(spec.agents.len(), 4);

    // Order preserved by IndexMap.
    let roles: Vec<&str> = spec.agents.keys().map(String::as_str).collect();
    assert_eq!(roles, vec!["explorer", "fixer", "reviewer", "shipper"]);

    let explorer = &spec.agents["explorer"];
    assert_eq!(explorer.executor, Executor::Claude);
    assert_eq!(explorer.trigger, Trigger::Manual);
    assert_eq!(
        explorer.output.as_deref(),
        Some(Path::new(".ccteam/issues/"))
    );

    let fixer = &spec.agents["fixer"];
    assert_eq!(fixer.executor, Executor::Claude);
    assert_eq!(
        fixer.trigger,
        Trigger::Watch(PathBuf::from(".ccteam/issues/"))
    );
    assert_eq!(fixer.parallelism, Some(10));

    let reviewer = &spec.agents["reviewer"];
    assert_eq!(reviewer.executor, Executor::Codex);
    assert_eq!(
        reviewer.trigger,
        Trigger::Watch(PathBuf::from(".ccteam/fixes/"))
    );

    let shipper = &spec.agents["shipper"];
    assert_eq!(shipper.trigger, Trigger::Gate);
    assert_eq!(
        shipper.input.as_deref(),
        Some(Path::new(".ccteam/verdicts/"))
    );
}

// ---------------------------------------------------------------------
// t02 — Second fixture (research-loop, 2 agents).
// ---------------------------------------------------------------------
#[test]
fn t02_load_research_loop() {
    let path = fixture_dir().join("workflow-research-loop.yaml");
    let spec = WorkflowSpec::load(&path).expect("research-loop should parse");

    assert_eq!(spec.name, "research-loop");
    assert_eq!(spec.agents.len(), 2);

    let claw = &spec.agents["claw"];
    assert_eq!(claw.trigger, Trigger::Manual);
    assert_eq!(claw.executor, Executor::Claude);

    let evaluator = &spec.agents["evaluator"];
    assert_eq!(
        evaluator.trigger,
        Trigger::Watch(PathBuf::from(".ccteam/raw-data/"))
    );
    assert_eq!(evaluator.parallelism, Some(5));
}

// ---------------------------------------------------------------------
// t03 — `watch:` with empty path fails validate (parses successfully
// at YAML layer; rejected at semantic validation).
// ---------------------------------------------------------------------
#[test]
fn t03_validate_empty_watch_path() {
    let yaml = r#"
name: bad-watch
agents:
  explorer:
    executor: claude
    trigger: "watch:"
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();

    let err = WorkflowSpec::load(&path).expect_err("empty watch path must fail");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(msg.contains("explorer"), "got: {msg}");
            assert!(msg.contains("non-empty path"), "got: {msg}");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t04 — `gate` trigger without `input` fails.
// ---------------------------------------------------------------------
#[test]
fn t04_validate_gate_without_input() {
    let yaml = r#"
name: bad-gate
agents:
  shipper:
    executor: claude
    trigger: gate
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();

    let err = WorkflowSpec::load(&path).expect_err("gate without input must fail");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(msg.contains("shipper"), "got: {msg}");
            assert!(msg.contains("input"), "got: {msg}");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t05 — Agent role name with illegal char (space) fails.
// ---------------------------------------------------------------------
#[test]
fn t05_invalid_agent_name() {
    // YAML maps allow quoted keys with spaces; parser must reject at validate.
    let yaml = r#"
name: bad-name
agents:
  "my agent":
    executor: claude
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();

    let err = WorkflowSpec::load(&path).expect_err("illegal name must fail");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(msg.contains("my agent"), "got: {msg}");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t06 — Project-level discovery finds `<dir>/workflow.yaml`.
// ---------------------------------------------------------------------
#[test]
fn t06_load_for_project_finds_workflow_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let yaml = r#"
name: discoverable
agents:
  explorer:
    trigger: manual
"#;
    std::fs::write(tmp.path().join("workflow.yaml"), yaml).unwrap();

    let spec = WorkflowSpec::load_for_project(tmp.path()).expect("must find workflow.yaml");
    assert_eq!(spec.name, "discoverable");
    assert_eq!(spec.agents.len(), 1);
    assert!(spec.agents.contains_key("explorer"));
    // executor defaults to claude when omitted.
    assert_eq!(spec.agents["explorer"].executor, Executor::Claude);
}

// ---------------------------------------------------------------------
// t06b — Discovery falls back to `<dir>/.ccteam/workflow.yaml`.
// (Bonus coverage; same numeric slot as t06 in the matrix.)
// ---------------------------------------------------------------------
#[test]
fn t06b_load_for_project_finds_nested_workflow_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join(".ccteam");
    std::fs::create_dir_all(&nested).unwrap();
    let yaml = r#"
name: nested-discovery
agents:
  explorer:
    trigger: manual
"#;
    std::fs::write(nested.join("workflow.yaml"), yaml).unwrap();

    let spec = WorkflowSpec::load_for_project(tmp.path())
        .expect("must find .ccteam/workflow.yaml fallback");
    assert_eq!(spec.name, "nested-discovery");
}

// ---------------------------------------------------------------------
// t07 — Empty project dir → NotFound.
// ---------------------------------------------------------------------
#[test]
fn t07_load_for_project_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let err = WorkflowSpec::load_for_project(tmp.path()).expect_err("must error");
    match err {
        WorkflowError::NotFound(path) => {
            assert_eq!(path, tmp.path().to_path_buf());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t08 — `executor` omitted defaults to claude.
// ---------------------------------------------------------------------
#[test]
fn t08_executor_default_claude() {
    let yaml = r#"
name: default-exec
agents:
  explorer:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).unwrap();
    assert_eq!(spec.agents["explorer"].executor, Executor::Claude);
}

// ---------------------------------------------------------------------
// t09 — `parallelism` omitted is `None` (caller treats `None` as 1;
// documented in workflow.rs AgentSpec::parallelism doc-comment).
// ---------------------------------------------------------------------
#[test]
fn t09_parallelism_default_one() {
    let yaml = r#"
name: default-parallel
agents:
  explorer:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).unwrap();
    assert_eq!(spec.agents["explorer"].parallelism, None);
}

// ---------------------------------------------------------------------
// t10 — `manual` trigger does NOT require `input`.
// ---------------------------------------------------------------------
#[test]
fn t10_manual_trigger_no_input_required() {
    let yaml = r#"
name: manual-only
agents:
  explorer:
    executor: claude
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("manual without input must succeed");
    assert_eq!(spec.agents["explorer"].trigger, Trigger::Manual);
    assert!(spec.agents["explorer"].input.is_none());
}

// ---------------------------------------------------------------------
// t11 — `watch:relative/path` parses to relative `PathBuf`.
// ---------------------------------------------------------------------
#[test]
fn t11_watch_trigger_path_relative() {
    let yaml = r#"
name: relative-watch
agents:
  fixer:
    trigger: watch:relative/path
    input: relative/path
    parallelism: 3
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).unwrap();
    let watch_path = match &spec.agents["fixer"].trigger {
        Trigger::Watch(p) => p.clone(),
        other => panic!("expected Watch, got {other:?}"),
    };
    assert_eq!(watch_path, PathBuf::from("relative/path"));
    assert!(watch_path.is_relative());
}

// ---------------------------------------------------------------------
// t12 — `gate` with `input` validates fine.
// ---------------------------------------------------------------------
#[test]
fn t12_gate_trigger_needs_input_dir() {
    let yaml = r#"
name: gated
agents:
  shipper:
    executor: claude
    trigger: gate
    input: .ccteam/verdicts/
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("gate + input must succeed");
    assert_eq!(spec.agents["shipper"].trigger, Trigger::Gate);
    assert_eq!(
        spec.agents["shipper"].input.as_deref(),
        Some(Path::new(".ccteam/verdicts/"))
    );
}

// ---------------------------------------------------------------------
// t13 — Duplicate agent key: YAML / serde behavior is "last wins";
// document this as the contract. (Map keys are not unique-checked by
// serde_yaml; the second mapping value overrides the first.)
// ---------------------------------------------------------------------
#[test]
fn t13_duplicate_agent_name() {
    let yaml = r#"
name: dup
agents:
  explorer:
    trigger: manual
    output: first
  explorer:
    trigger: manual
    output: second
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("duplicate key is allowed by serde_yaml");
    assert_eq!(spec.agents.len(), 1);
    assert_eq!(
        spec.agents["explorer"].output.as_deref(),
        Some(Path::new("second")),
        "last-wins per serde_yaml semantics"
    );
}

// ---------------------------------------------------------------------
// t14 — Round-trip: load → serialize → re-load yields equal spec.
// ---------------------------------------------------------------------
#[test]
fn t14_serialization_roundtrip() {
    let path = fixture_dir().join("workflow-ui-quality-loop.yaml");
    let original = WorkflowSpec::load(&path).unwrap();

    // Serialize back to YAML.
    let rendered = serde_yaml::to_string(&original).expect("serialize");

    // Re-load from rendered YAML via a tmpfile (load() does validate too).
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("workflow.yaml");
    std::fs::write(&dest, &rendered).unwrap();
    let reparsed = WorkflowSpec::load(&dest).expect("re-parse round-trip");

    // PartialEq on WorkflowSpec covers every public field including the
    // IndexMap (which compares by key order + value).
    assert_eq!(original, reparsed);
}

// ---------------------------------------------------------------------
// t15 — Unknown executor enum variant → ParseFailed.
// ---------------------------------------------------------------------
#[test]
fn t15_unknown_executor_fails() {
    let yaml = r#"
name: bad-exec
agents:
  explorer:
    executor: unknown
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).expect_err("unknown executor must fail");
    assert!(
        matches!(err, WorkflowError::ParseFailed(_)),
        "expected ParseFailed, got {err:?}"
    );
}

// ---------------------------------------------------------------------
// t16 — Extra coverage: parallelism > 1 with non-watch trigger rejected.
// (Validates rule 4 in PRD §6.1.)
// ---------------------------------------------------------------------
#[test]
fn t16_parallelism_gt1_requires_watch() {
    let yaml = r#"
name: bad-parallel
agents:
  explorer:
    trigger: manual
    parallelism: 4
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).expect_err("parallelism > 1 on manual must fail");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(msg.contains("parallelism"), "got: {msg}");
            assert!(msg.contains("watch"), "got: {msg}");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t17 — Extra coverage: empty agents map rejected.
// ---------------------------------------------------------------------
#[test]
fn t17_empty_agents_rejected() {
    let yaml = r#"
name: empty
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path).expect_err("empty agents map must fail");
    assert!(matches!(err, WorkflowError::ValidationFailed(_)));
}

// ---------------------------------------------------------------------
// t18 — V0.6.1 F124 narrow scope: `mode: human-approval` parses,
// validates, and round-trips. The orchestrator-side HITL gate
// (pending-drain skip + `plan_decision_required` emission) is
// covered by orchestrator integration tests; this test pins the
// schema contract.
// ---------------------------------------------------------------------
#[test]
fn t18_human_approval_mode_round_trip() {
    use ccteam_flow::workflow::WorkflowMode;
    let yaml = r#"
name: critical-migration
mode: human-approval
agents:
  migrator:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let spec = WorkflowSpec::load(&path).expect("human-approval mode must parse");
    assert_eq!(spec.mode, WorkflowMode::HumanApproval);
    assert_eq!(spec.agents.len(), 1);

    // Round-trip: serialize → re-parse, mode must survive.
    let rendered = serde_yaml::to_string(&spec).expect("serialize");
    assert!(
        rendered.contains("mode: human-approval"),
        "expected kebab-case `human-approval` in rendered yaml; got:\n{rendered}"
    );
    let dest = tmp.path().join("round-trip.yaml");
    std::fs::write(&dest, &rendered).unwrap();
    let reparsed = WorkflowSpec::load(&dest).expect("re-parse round-trip");
    assert_eq!(spec, reparsed);
}

// ---------------------------------------------------------------------
// t19 — V0.6.1 F124: `mode: human-approval` requires at least one
// agent (same structural rule as artifact-driven; the HITL gate
// runs over the existing roster).
// ---------------------------------------------------------------------
#[test]
fn t19_human_approval_requires_agents() {
    let yaml = r#"
name: empty-hitl
mode: human-approval
agents: {}
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path)
        .expect_err("mode: human-approval with empty agents must be rejected");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(msg.contains("human-approval"), "got: {msg}");
            assert!(msg.contains("at least one agent"), "got: {msg}");
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// t20 — V0.6.1 F124: `mode: human-approval` + `agent_team:` block
// is a schema bug (agent_team is exclusive to mode: agent-team).
// ---------------------------------------------------------------------
#[test]
fn t20_human_approval_rejects_agent_team_block() {
    let yaml = r#"
name: bad-hitl
mode: human-approval
agent_team:
  team_name: nope
  lead_seed: |
    irrelevant
agents:
  migrator:
    trigger: manual
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(&path, yaml).unwrap();
    let err = WorkflowSpec::load(&path)
        .expect_err("agent_team block under mode: human-approval must be rejected");
    match err {
        WorkflowError::ValidationFailed(msg) => {
            assert!(
                msg.contains("agent_team") && msg.contains("agent-team"),
                "got: {msg}"
            );
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}
