//! Corruption-tolerant readers for ccteam-owned append-only JSONL journals.
//!
//! All parsing is byte-based so one torn UTF-8 sequence costs one line rather
//! than making the entire journal unreadable. Tail reads walk backwards from
//! EOF in fixed-size blocks; full scans stream one line at a time.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::Value;

const READ_BLOCK_SIZE: u64 = 8 * 1024;

static BYTES_READ: AtomicU64 = AtomicU64::new(0);
static RECORDS_PARSED: AtomicU64 = AtomicU64::new(0);
static INVALID_LINES: AtomicU64 = AtomicU64::new(0);

/// Process-wide aggregate of journal-facade work.
///
/// Counters are monotonic and relaxed: they are diagnostics, not a
/// synchronization primitive. Callers can subtract two snapshots to measure
/// one operation without adding per-request allocations or locks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JournalMetrics {
    pub bytes_read: u64,
    pub records_parsed: u64,
    pub invalid_lines: u64,
}

/// Snapshot process-wide journal read counters.
pub fn metrics() -> JournalMetrics {
    JournalMetrics {
        bytes_read: BYTES_READ.load(Ordering::Relaxed),
        records_parsed: RECORDS_PARSED.load(Ordering::Relaxed),
        invalid_lines: INVALID_LINES.load(Ordering::Relaxed),
    }
}

fn record_metrics(bytes_read: u64, records_parsed: u64, invalid_lines: usize) {
    BYTES_READ.fetch_add(bytes_read, Ordering::Relaxed);
    RECORDS_PARSED.fetch_add(records_parsed, Ordering::Relaxed);
    INVALID_LINES.fetch_add(invalid_lines as u64, Ordering::Relaxed);
}

/// A corruption-tolerant tail, ordered oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Tail<T = Value> {
    pub events: Vec<T>,
    pub corrupt_count: usize,
    /// Byte offset of the first returned line, suitable as a `before` cursor.
    pub first_offset: Option<u64>,
    /// Whether at least one older parseable row exists before `first_offset`.
    pub has_more: bool,
}

impl<T> Default for Tail<T> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            corrupt_count: 0,
            first_offset: None,
            has_more: false,
        }
    }
}

/// What a [`tail_select`] closure says about one raw row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick<T> {
    /// Keep this row; it counts toward the requested `n`.
    Keep(T),
    /// A well-formed row the caller does not want. Passed over silently: it
    /// counts as neither kept nor corrupt, so a sparse selection over a busy
    /// journal does not inflate the corruption counters.
    Skip,
    /// Not a row of the caller's schema; counted in `corrupt_count`.
    Corrupt,
}

/// Result metadata from a streaming forward scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub corrupt_count: usize,
    pub next_offset: u64,
}

/// Detailed metadata from a streaming forward scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DetailedScanSummary {
    /// Number of parseable, non-blank JSON rows.
    pub valid_count: u64,
    /// Number of non-blank rows that were not valid JSON.
    pub corrupt_count: usize,
    /// Byte offset of the first corrupt row, when one exists.
    pub first_corrupt_offset: Option<u64>,
    /// Byte position immediately after the last row read.
    pub next_offset: u64,
}

/// Complete rows read after a durable byte checkpoint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Delta {
    pub events: Vec<Value>,
    /// Exact byte position after the last complete line consumed.
    pub next_offset: u64,
    pub corrupt_count: usize,
}

/// Return the final parseable JSON row, skipping corrupt trailing rows.
pub fn last_valid(path: &Path) -> Result<Option<Value>> {
    Ok(tail_pick_inner(path, 1, None, false, u64::MAX, |line| {
        parse_or_corrupt(serde_json::from_slice::<Value>(line).ok())
    })?
    .events
    .pop())
}

/// Return the newest `n` parseable JSON rows in chronological order.
pub fn tail_valid(path: &Path, n: usize) -> Result<Tail> {
    tail_valid_before(path, n, None)
}

/// Return the newest `n` parseable JSON rows before an optional byte cursor.
///
/// `before` is exclusive and is normally a prior result's `first_offset`.
pub fn tail_valid_before(path: &Path, n: usize, before: Option<u64>) -> Result<Tail> {
    tail_filter_map(path, n, before, |line| {
        serde_json::from_slice::<Value>(line).ok()
    })
}

/// Shared typed-tail primitive for schema owners such as `turns_mirror`.
///
/// Returning `None` marks a non-blank row corrupt for the caller's schema.
/// This keeps the backwards I/O implementation inside the journal facade
/// without forcing it to depend on every journal record type.
pub fn tail_filter_map<T, F>(
    path: &Path,
    n: usize,
    before: Option<u64>,
    mut parse: F,
) -> Result<Tail<T>>
where
    F: FnMut(&[u8]) -> Option<T>,
{
    tail_pick_inner(path, n, before, true, u64::MAX, |line| {
        parse_or_corrupt(parse(line))
    })
}

/// Return the newest `n` rows the caller keeps, walking backwards from
/// `before` (exclusive) or EOF and reading at most `max_bytes` from disk.
///
/// This is the tail for SPARSE rows. [`tail_filter_map`] bounds its walk in
/// rows of the file, which is the wrong unit when the rows of interest are a
/// handful among thousands of unrelated ones: the window fills with noise and
/// tells the caller nothing about the rows it asked for. Here `n` counts kept
/// rows only — the bound is in the caller's unit — and the byte budget is what
/// keeps one request's cost finite on a large journal.
///
/// `has_more` is true when an (n+1)th kept row exists, OR when the byte budget
/// ran out first: a walk that stopped early cannot promise the rest of the
/// file holds nothing the caller wanted, so it must not read as complete.
pub fn tail_select<T, F>(
    path: &Path,
    n: usize,
    before: Option<u64>,
    max_bytes: u64,
    pick: F,
) -> Result<Tail<T>>
where
    F: FnMut(&[u8]) -> Pick<T>,
{
    tail_pick_inner(path, n, before, true, max_bytes, pick)
}

/// The two-way parse contract of [`tail_filter_map`] / [`last_valid`], spelled
/// in [`Pick`]: those callers never skip, so `None` is corruption.
fn parse_or_corrupt<T>(parsed: Option<T>) -> Pick<T> {
    match parsed {
        Some(value) => Pick::Keep(value),
        None => Pick::Corrupt,
    }
}

fn tail_pick_inner<T, F>(
    path: &Path,
    n: usize,
    before: Option<u64>,
    probe_older: bool,
    max_bytes: u64,
    mut pick: F,
) -> Result<Tail<T>>
where
    F: FnMut(&[u8]) -> Pick<T>,
{
    if n == 0 {
        return Ok(Tail::default());
    }

    let mut reader = match ReverseLines::open(path, before)? {
        Some(reader) => reader,
        None => return Ok(Tail::default()),
    };
    let target = if probe_older { n.saturating_add(1) } else { n };
    let mut rows = Vec::with_capacity(target.min(1024));
    let mut corrupt_count = 0;
    let mut budget_exhausted = false;

    while rows.len() < target {
        // Lines already buffered are free; the budget gates the next disk
        // block only, so an exhausted budget never drops a row it has read.
        if reader.bytes_read >= max_bytes && reader.needs_read() {
            budget_exhausted = true;
            break;
        }
        let Some((offset, raw)) = reader.next_line()? else {
            break;
        };
        let line = trim_ascii(&raw);
        if line.is_empty() {
            continue;
        }
        match pick(line) {
            Pick::Keep(value) => rows.push((offset, value)),
            Pick::Skip => {}
            Pick::Corrupt => corrupt_count += 1,
        }
    }

    let records_parsed = rows.len();
    let has_more = (probe_older && records_parsed > n) || budget_exhausted;
    if has_more {
        rows.truncate(n);
    }
    rows.reverse();
    let first_offset = rows.first().map(|(offset, _)| *offset);
    let events = rows.into_iter().map(|(_, value)| value).collect();

    record_metrics(reader.bytes_read, records_parsed as u64, corrupt_count);

    Ok(Tail {
        events,
        corrupt_count,
        first_offset,
        has_more,
    })
}

/// Stream every parseable row through `reduce`, without collecting values.
pub fn scan_stream<F>(path: &Path, mut reduce: F) -> Result<ScanSummary>
where
    F: FnMut(Value),
{
    let detailed = scan_stream_detailed(path, |value, _bytes| reduce(value))?;
    Ok(ScanSummary {
        corrupt_count: detailed.corrupt_count,
        next_offset: detailed.next_offset,
    })
}

/// Stream every parseable row with its exact on-disk byte length.
///
/// The callback receives the row length including its trailing newline when
/// present. Invalid UTF-8 and invalid JSON are isolated to their byte line;
/// their first offset is reported without materializing the file.
pub fn scan_stream_detailed<F>(path: &Path, mut reduce: F) -> Result<DetailedScanSummary>
where
    F: FnMut(Value, u64),
{
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DetailedScanSummary::default());
        }
        Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
    };
    let mut reader = BufReader::new(file);
    let mut raw = Vec::new();
    let mut summary = DetailedScanSummary::default();

    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        let row_offset = summary.next_offset;
        let row_bytes = u64::try_from(read).unwrap_or(u64::MAX);
        summary.next_offset = summary.next_offset.saturating_add(row_bytes);
        let line = trim_ascii(&raw);
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(value) => {
                summary.valid_count = summary.valid_count.saturating_add(1);
                reduce(value, row_bytes);
            }
            Err(_) => {
                summary.corrupt_count += 1;
                summary.first_corrupt_offset.get_or_insert(row_offset);
            }
        }
    }

    record_metrics(
        summary.next_offset,
        summary.valid_count,
        summary.corrupt_count,
    );

    Ok(summary)
}

/// Read complete rows starting at `from_offset`.
///
/// A final line without `\n` is left untouched even when its current bytes
/// happen to form valid JSON. Appending writers may still be extending it, so
/// the returned checkpoint remains at the start of that partial line.
pub fn read_delta(path: &Path, from_offset: u64) -> Result<Delta> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Delta {
                next_offset: from_offset,
                ..Delta::default()
            });
        }
        Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
    };
    let len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if from_offset > len {
        bail!(
            "journal offset {from_offset} is beyond {} byte length {len}",
            path.display()
        );
    }
    file.seek(SeekFrom::Start(from_offset))
        .with_context(|| format!("seek {} to {from_offset}", path.display()))?;

    let mut reader = BufReader::new(file);
    let mut delta = Delta {
        next_offset: from_offset,
        ..Delta::default()
    };
    let mut raw = Vec::new();
    let mut bytes_read = 0u64;

    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if raw.last() != Some(&b'\n') {
            break;
        }
        delta.next_offset += read as u64;
        let line = trim_ascii(&raw);
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(value) => delta.events.push(value),
            Err(_) => delta.corrupt_count += 1,
        }
    }

    record_metrics(bytes_read, delta.events.len() as u64, delta.corrupt_count);

    Ok(delta)
}

struct ReverseLines {
    file: File,
    read_pos: u64,
    buffer_start: u64,
    buffer: Vec<u8>,
    reached_bof: bool,
    bytes_read: u64,
}

impl ReverseLines {
    fn open(path: &Path, before: Option<u64>) -> Result<Option<Self>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
        };
        let len = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        let end = before.unwrap_or(len).min(len);
        Ok(Some(Self {
            file,
            read_pos: end,
            buffer_start: end,
            buffer: Vec::new(),
            reached_bof: false,
            bytes_read: 0,
        }))
    }

    /// True when the next [`next_line`] would have to read another block:
    /// nothing complete is buffered and the file start has not been reached.
    fn needs_read(&self) -> bool {
        !self.reached_bof && !self.buffer.contains(&b'\n')
    }

    fn next_line(&mut self) -> Result<Option<(u64, Vec<u8>)>> {
        loop {
            if let Some(newline) = self.buffer.iter().rposition(|byte| *byte == b'\n') {
                let offset = self.buffer_start + newline as u64 + 1;
                let line = self.buffer[newline + 1..].to_vec();
                self.buffer.truncate(newline);
                return Ok(Some((offset, line)));
            }

            if self.reached_bof {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let offset = self.buffer_start;
                return Ok(Some((offset, std::mem::take(&mut self.buffer))));
            }

            let step = self.read_pos.min(READ_BLOCK_SIZE);
            let next_pos = self.read_pos - step;
            self.file
                .seek(SeekFrom::Start(next_pos))
                .context("seek journal tail block")?;
            let mut prefix = vec![0; step as usize];
            self.file
                .read_exact(&mut prefix)
                .context("read journal tail block")?;
            self.bytes_read = self.bytes_read.saturating_add(step);
            prefix.extend_from_slice(&self.buffer);
            self.buffer = prefix;
            self.buffer_start = next_pos;
            self.read_pos = next_pos;
            self.reached_bof = next_pos == 0;
        }
    }
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const READER_ALLOWLIST: &[&str] = &[
        // The byte-based facade is the one runtime implementation allowed to
        // open progress/turns journals for reads.
        "crates/ccteam-harness/src/execution/journal.rs",
        // Retained for doctor/import and legacy harness-internal full reads;
        // core/web/im runtime progress readers must use the facade instead.
        "crates/ccteam-harness/src/execution/fs_atomic.rs",
    ];

    fn write_torn_fixture(path: &Path, corrupt_tail: bool) {
        let mut raw = Vec::new();
        raw.extend_from_slice(br#"{"n":1}"#);
        raw.push(b'\n');
        raw.extend_from_slice("{\"note\":\"配置：".as_bytes());
        raw.truncate(raw.len() - 2);
        raw.extend_from_slice(br#"{"n":2}"#);
        raw.push(b'\n');
        raw.extend_from_slice(br#"{"n":3}"#);
        raw.push(b'\n');
        if corrupt_tail {
            raw.extend_from_slice(b"{not-json");
        }
        std::fs::write(path, raw).unwrap();
    }

    #[test]
    fn tail_and_last_skip_torn_utf8_and_corrupt_trailing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        write_torn_fixture(&path, true);

        assert_eq!(last_valid(&path).unwrap().unwrap()["n"], 3);
        let tail = tail_valid(&path, 2).unwrap();
        assert_eq!(
            tail.events
                .iter()
                .map(|event| event["n"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(tail.corrupt_count >= 1);
        assert!(!tail.has_more);
    }

    #[test]
    fn tail_cursor_pages_without_rereading_newer_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turns.jsonl");
        std::fs::write(&path, b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();

        let newest = tail_valid(&path, 2).unwrap();
        assert_eq!(newest.events[0]["n"], 2);
        assert_eq!(newest.events[1]["n"], 3);
        assert!(newest.has_more);

        let older = tail_valid_before(&path, 2, newest.first_offset).unwrap();
        assert_eq!(older.events.len(), 1);
        assert_eq!(older.events[0]["n"], 1);
        assert!(!older.has_more);
    }

    /// A row selector: keep `{"k":"want",...}`, skip every other valid row.
    fn want_only(line: &[u8]) -> Pick<Value> {
        match serde_json::from_slice::<Value>(line) {
            Ok(value) if value["k"] == "want" => Pick::Keep(value),
            Ok(_) => Pick::Skip,
            Err(_) => Pick::Corrupt,
        }
    }

    #[test]
    fn tail_select_bounds_by_kept_rows_and_never_counts_skips_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        // Three wanted rows, each buried under a hundred rows of noise — the
        // shape of a run envelope on a busy project journal.
        let mut raw = String::new();
        for wanted in 1..=3 {
            for noise in 0..100 {
                raw.push_str(&format!("{{\"k\":\"noise\",\"n\":{noise}}}\n"));
            }
            raw.push_str(&format!("{{\"k\":\"want\",\"n\":{wanted}}}\n"));
        }
        raw.push_str("{not-json\n");
        std::fs::write(&path, raw).unwrap();

        // Two kept rows asked for: the newest two, in file order, with the
        // third's existence reported — and the 300 skipped rows nowhere.
        let two = tail_select(&path, 2, None, u64::MAX, want_only).unwrap();
        let ns = |tail: &Tail| {
            tail.events
                .iter()
                .map(|event| event["n"].as_u64().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(ns(&two), vec![2, 3]);
        assert!(two.has_more);
        assert_eq!(two.corrupt_count, 1, "only the torn row is corrupt");

        // Room for all of them: complete, and honestly so.
        let all = tail_select(&path, 3, None, u64::MAX, want_only).unwrap();
        assert_eq!(ns(&all), vec![1, 2, 3]);
        assert!(!all.has_more);

        // The row-bounded tail over the same file sees only the newest wanted
        // row — the other two are buried past its window, unreported.
        let by_rows = tail_valid(&path, 50).unwrap();
        let wanted_by_rows: Vec<u64> = by_rows
            .events
            .iter()
            .filter(|event| event["k"] == "want")
            .map(|event| event["n"].as_u64().unwrap())
            .collect();
        assert_eq!(wanted_by_rows, vec![3]);
    }

    #[test]
    fn tail_select_reports_has_more_when_the_byte_budget_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        // One wanted row at the very top, then well over a budget's worth of
        // noise below it.
        let mut raw = String::from("{\"k\":\"want\",\"n\":1}\n");
        for noise in 0..2_000 {
            raw.push_str(&format!("{{\"k\":\"noise\",\"n\":{noise}}}\n"));
        }
        std::fs::write(&path, raw).unwrap();

        // Budget smaller than the file: the walk stops short of the wanted
        // row, and says so — an empty answer here is "unknown", not "none".
        let short = tail_select(&path, 10, None, READ_BLOCK_SIZE, want_only).unwrap();
        assert!(short.events.is_empty());
        assert!(
            short.has_more,
            "an exhausted budget must not read as complete"
        );

        // Budget covering the file: the row is found and nothing is pending.
        let full = tail_select(&path, 10, None, u64::MAX, want_only).unwrap();
        assert_eq!(full.events.len(), 1);
        assert!(!full.has_more);
    }

    #[test]
    fn scan_stream_keeps_good_rows_around_torn_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        write_torn_fixture(&path, false);
        let mut seen = Vec::new();

        let summary = scan_stream(&path, |event| seen.push(event["n"].clone())).unwrap();
        assert_eq!(seen, vec![Value::from(1), Value::from(3)]);
        assert_eq!(summary.corrupt_count, 1);
        assert_eq!(summary.next_offset, std::fs::metadata(path).unwrap().len());
    }

    #[test]
    fn delta_stops_at_partial_line_and_resumes_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.jsonl");
        std::fs::write(&path, b"{\"n\":1}\nnot-json\n{\"n\":2").unwrap();

        let first = read_delta(&path, 0).unwrap();
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0]["n"], 1);
        assert_eq!(first.corrupt_count, 1);
        assert_eq!(first.next_offset, b"{\"n\":1}\nnot-json\n".len() as u64);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"}\n").unwrap();
        let resumed = read_delta(&path, first.next_offset).unwrap();
        assert_eq!(resumed.events.len(), 1);
        assert_eq!(resumed.events[0]["n"], 2);
        assert_eq!(resumed.next_offset, std::fs::metadata(path).unwrap().len());
    }

    #[test]
    fn empty_and_missing_journals_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.jsonl");
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, []).unwrap();

        for path in [&missing, &empty] {
            assert!(last_valid(path).unwrap().is_none());
            assert!(tail_valid(path, 10).unwrap().events.is_empty());
            assert_eq!(scan_stream(path, |_| {}).unwrap(), ScanSummary::default());
            assert!(read_delta(path, 0).unwrap().events.is_empty());
        }
    }

    #[test]
    fn runtime_journal_readers_cannot_bypass_the_facade() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let roots = [
            "crates/ccteam-core/src",
            "crates/ccteam-im/src",
            "crates/ccteam-web/src",
        ];
        let mut files = Vec::new();
        for root in roots {
            collect_rust_sources(&workspace.join(root), &mut files);
        }

        let mut violations = Vec::new();
        for path in files {
            let relative = path
                .strip_prefix(&workspace)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if READER_ALLOWLIST.contains(&relative.as_str()) {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            violations.extend(source_violations(&relative, &source));
        }

        assert!(
            violations.is_empty(),
            "raw progress/turns journal readers must use ccteam_core::journal:\n{}",
            violations.join("\n")
        );
    }

    fn collect_rust_sources(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                collect_rust_sources(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    fn source_violations(relative: &str, source: &str) -> Vec<String> {
        let lines = source.lines().collect::<Vec<_>>();
        let production_end = lines
            .windows(2)
            .position(|pair| pair[0].trim() == "#[cfg(test)]" && pair[1].contains("mod tests"))
            .unwrap_or(lines.len());
        let raw_markers = [
            "read_to_string(",
            "std::fs::read(",
            "fs::read(",
            "read_jsonl(",
            "read_jsonl::<",
            "BufReader::new(",
        ];
        let journal_markers = [
            "progress.jsonl",
            "turns.jsonl",
            "progress_jsonl",
            "turns_jsonl",
            "progress_path",
            "turns_path",
        ];
        let mut violations = Vec::new();
        let owns_journal_reads = relative.ends_with("/progress.rs");

        for index in 0..production_end {
            let code = lines[index].trim();
            if code.starts_with("//") || !raw_markers.iter().any(|marker| code.contains(marker)) {
                continue;
            }
            let start = index.saturating_sub(10);
            let end = (index + 11).min(production_end);
            let context = lines[start..end]
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            if owns_journal_reads
                || journal_markers
                    .iter()
                    .any(|marker| context.contains(marker))
            {
                violations.push(format!("{relative}:{}: {code}", index + 1));
            }
        }
        violations
    }
}
