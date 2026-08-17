use ccteam_harness::execution::journal;

#[test]
fn scan_updates_process_metrics_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    let bytes = b"{\"n\":1}\nnot-json\n{\"n\":2}\n";
    std::fs::write(&path, bytes).unwrap();

    let before = journal::metrics();
    let summary = journal::scan_stream(&path, |_| {}).unwrap();
    let after = journal::metrics();

    assert_eq!(summary.corrupt_count, 1);
    assert_eq!(after.bytes_read - before.bytes_read, bytes.len() as u64);
    assert_eq!(after.records_parsed - before.records_parsed, 2);
    assert_eq!(after.invalid_lines - before.invalid_lines, 1);
}
