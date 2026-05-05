//! Integration tests for `Orchestrator` — startup validation, the
//! shutdown contract, and that the loop ticks on a short interval.

use std::path::PathBuf;
use std::time::Duration;

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};
use tempfile::TempDir;

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

fn write_template(dir: &PathBuf, file: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file), body).unwrap();
}

#[test]
fn orchestrator_constructs_when_phases_dir_is_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert!(orch.templates().is_empty());
}

#[test]
fn orchestrator_loads_valid_solo_template() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "sub_skills: []\n",
            "---\n",
            "body\n",
        ),
    );

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert_eq!(orch.templates().len(), 1);
    assert_eq!(orch.templates()[0].name, "implement");
}

#[test]
fn orchestrator_fails_fast_on_agent_team_template() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: agent_team\n",
            "---\n",
            "body\n",
        ),
    );

    let err = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("solo"),
        "expected M0 validation failure, got: {msg}",
    );
}

#[test]
fn orchestrator_accepts_empty_sub_skills_no_op() {
    // M0 acceptance: empty sub_skills must NOT error (real scheduling
    // is M2). The phase parser already enforces this; the orchestrator
    // contract: load + validate without failing on empty lists.
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "sub_skills: []\n",
            "agent_team: []\n",
            "---\n",
        ),
    );
    Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
}

#[tokio::test]
async fn run_returns_when_shutdown_future_resolves() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = Orchestrator::new(
        paths,
        OrchestratorConfig {
            tick_interval: Duration::from_millis(50),
        },
    )
    .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), async {
        orch.run(async {
            tokio::time::sleep(Duration::from_millis(150)).await;
        })
        .await
    })
    .await;

    let inner = result.expect("orchestrator did not honor the shutdown future");
    inner.expect("orchestrator returned an error during a clean shutdown");
}

#[tokio::test]
async fn run_creates_progress_dir_when_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let progress = paths.root.join("progress");
    assert!(!progress.exists());

    let orch =
        Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        orch.run(async {
            // give the watcher setup a beat then shut down
            tokio::time::sleep(Duration::from_millis(80)).await;
        })
        .await
    })
    .await
    .unwrap()
    .unwrap();

    assert!(progress.is_dir(), "run() must create the progress dir");
}
