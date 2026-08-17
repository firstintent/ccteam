use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use ccteam_core::CcteamPaths;
use ccteam_harness::execution::progress_bridge::{self, AGENT_DONE};
use ccteam_im::progress_projection::ProgressProjection;
use serde_json::{json, Value};

fn paths(root: &Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join("ccteam-home"),
        projects_root: root.join("projects"),
    }
}

fn event(sequence: u64, cost: f64) -> Value {
    json!({
        "event": AGENT_DONE,
        "sid": format!("s{sequence}"),
        "vendor": if sequence % 2 == 0 { "claude" } else { "codex" },
        "cost_usd": cost,
        "padding": "x".repeat(180),
        "ts": "2026-08-17T00:00:00Z",
    })
}

fn write_rows(path: &Path, rows: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(path).unwrap();
    for row in rows {
        serde_json::to_writer(&mut file, row).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

#[test]
fn checkpoint_hydration_recovers_rotation_crash_and_live_shrink() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "4096");

    // Fresh hydration from checkpoint + active must match a never-rotated
    // control journal containing the same events.
    let rotated_temp = tempfile::tempdir().unwrap();
    let rotated_paths = paths(rotated_temp.path());
    let rotated_active = rotated_paths.progress_jsonl("rotated");
    let rows = (0..48)
        .map(|sequence| event(sequence, (sequence + 1) as f64 / 10.0))
        .collect::<Vec<_>>();
    for row in &rows {
        progress_bridge::append_event(&rotated_active, row).unwrap();
    }
    let rotated_projection = ProgressProjection::new(rotated_paths.clone());
    rotated_projection
        .hydrate_now(&["rotated".to_string()])
        .unwrap();

    let control_temp = tempfile::tempdir().unwrap();
    let control_paths = paths(control_temp.path());
    write_rows(&control_paths.progress_jsonl("control"), &rows);
    let control_projection = ProgressProjection::new(control_paths);
    control_projection
        .hydrate_now(&["control".to_string()])
        .unwrap();
    let rotated_cost = rotated_projection
        .project_snapshot("rotated")
        .cost
        .cost_total_usd;
    let rotated_snapshot = rotated_projection.project_snapshot("rotated");
    let control_snapshot = control_projection.project_snapshot("control");
    let control_cost = control_snapshot.cost.cost_total_usd;
    assert!((rotated_cost - control_cost).abs() < 1e-9);
    assert_eq!(
        rotated_snapshot.cost.cost_total_by_vendor,
        control_snapshot.cost.cost_total_by_vendor
    );
    let rotated_checkpoint = progress_bridge::read_progress_checkpoint(&rotated_active)
        .unwrap()
        .unwrap();
    assert!(!rotated_checkpoint.cost_total_by_sid.is_empty());

    // Simulate SIGKILL after mv(active, .1) but before checkpoint publish.
    let crash_temp = tempfile::tempdir().unwrap();
    let crash_paths = paths(crash_temp.path());
    let crash_active = crash_paths.progress_jsonl("crash");
    let crash_rows = vec![event(101, 1.25), event(102, 2.5), event(103, 5.0)];
    write_rows(&crash_active, &crash_rows);
    let crash_archive = progress_bridge::progress_archive_path(&crash_active);
    std::fs::rename(&crash_active, &crash_archive).unwrap();
    File::create(&crash_active).unwrap();
    assert!(!progress_bridge::progress_checkpoint_path(&crash_active).exists());

    let crash_projection = ProgressProjection::new(crash_paths);
    crash_projection
        .hydrate_now(&["crash".to_string()])
        .unwrap();
    assert!(
        (crash_projection
            .project_snapshot("crash")
            .cost
            .cost_total_usd
            - 8.75)
            .abs()
            < 1e-9
    );
    let recovered = progress_bridge::read_progress_checkpoint(&crash_active)
        .unwrap()
        .unwrap();
    let coverage = progress_bridge::progress_archive_coverage(&crash_active).unwrap();
    assert!(progress_bridge::checkpoint_covers_archive(
        &recovered,
        coverage.as_ref()
    ));
    assert_eq!(recovered.event_count, 3);

    // A projection that was already live gets the explicit rotation notice;
    // it rehydrates through the same checkpoint path as the shrink guard.
    let live_temp = tempfile::tempdir().unwrap();
    let live_paths = paths(live_temp.path());
    let live_active = live_paths.progress_jsonl("live");
    progress_bridge::append_event(&live_active, &event(200, 3.0)).unwrap();
    let live_projection = ProgressProjection::new(live_paths);
    live_projection.hydrate_now(&["live".to_string()]).unwrap();
    let mut expected = 3.0;
    let mut sequence = 201_u64;
    while !progress_bridge::progress_archive_path(&live_active).exists() {
        progress_bridge::append_event(&live_active, &event(sequence, 0.5)).unwrap();
        expected += 0.5;
        sequence += 1;
        assert!(sequence < 300);
    }
    progress_bridge::append_event(&live_active, &event(sequence, 1.75)).unwrap();
    expected += 1.75;
    let live_cost = live_projection.project_snapshot("live").cost.cost_total_usd;
    assert!((live_cost - expected).abs() < 1e-9);
    assert!(live_projection.metrics().rotations >= 1);
}
