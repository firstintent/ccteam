use std::sync::Arc;

use ccteam_harness::execution::{journal, progress_bridge};
use serde_json::json;

#[test]
fn concurrent_appenders_lose_no_rows_during_rotation() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "4096");
    let temp = tempfile::tempdir().unwrap();
    let active = Arc::new(temp.path().join("parallel.jsonl"));
    let threads = 8_u64;
    let per_thread = 8_u64;

    let handles = (0..threads)
        .map(|worker| {
            let active = Arc::clone(&active);
            std::thread::spawn(move || {
                for sequence in 0..per_thread {
                    progress_bridge::append_event(
                        &active,
                        &json!({
                            "event": "parallel_rotation_fixture",
                            "worker": worker,
                            "seq": sequence,
                            "padding": "x".repeat(32),
                        }),
                    )
                    .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let archive = progress_bridge::progress_archive_path(&active);
    assert!(archive.exists(), "fixture must exercise a rollover");
    let active_scan = journal::scan_stream_detailed(&active, |_, _| {}).unwrap();
    let archive_scan = journal::scan_stream_detailed(&archive, |_, _| {}).unwrap();
    assert_eq!(
        active_scan.valid_count + archive_scan.valid_count,
        threads * per_thread
    );
    assert_eq!(active_scan.corrupt_count + archive_scan.corrupt_count, 0);
}
