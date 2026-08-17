use ccteam_harness::execution::{journal, progress_bridge};
use serde_json::json;

#[test]
fn append_rolls_once_and_keeps_a_fresh_active_file() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "0");
    assert_eq!(
        progress_bridge::progress_rotate_bytes(),
        progress_bridge::DEFAULT_PROGRESS_ROTATE_BYTES
    );
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "invalid");
    assert_eq!(
        progress_bridge::progress_rotate_bytes(),
        progress_bridge::DEFAULT_PROGRESS_ROTATE_BYTES
    );
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "4096");
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("demo.jsonl");
    let archive = progress_bridge::progress_archive_path(&active);

    let mut appended = 0_u64;
    while !archive.exists() {
        progress_bridge::append_event(
            &active,
            &json!({
                "event": "rotation_fixture",
                "seq": appended,
                "padding": "x".repeat(240),
            }),
        )
        .unwrap();
        appended += 1;
        assert!(appended < 100, "tiny threshold should rotate promptly");
    }
    assert_eq!(std::fs::metadata(&active).unwrap().len(), 0);

    for sequence in 0..3 {
        progress_bridge::append_event(
            &active,
            &json!({"event": "rotation_fixture", "seq": appended + sequence}),
        )
        .unwrap();
    }
    appended += 3;

    let active_scan = journal::scan_stream_detailed(&active, |_, _| {}).unwrap();
    let archive_scan = journal::scan_stream_detailed(&archive, |_, _| {}).unwrap();
    assert_eq!(active_scan.valid_count + archive_scan.valid_count, appended);
    assert_eq!(active_scan.corrupt_count + archive_scan.corrupt_count, 0);
    assert_eq!(
        std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".1.jsonl"))
            .count(),
        1
    );

    let checkpoint = progress_bridge::read_progress_checkpoint(&active)
        .unwrap()
        .unwrap();
    let coverage = progress_bridge::progress_archive_coverage(&active).unwrap();
    assert!(progress_bridge::checkpoint_covers_archive(
        &checkpoint,
        coverage.as_ref()
    ));
}
