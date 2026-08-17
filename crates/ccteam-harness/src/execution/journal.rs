//! Corruption-tolerant readers for ccteam-owned append-only JSONL journals.
//!
//! All parsing is byte-based so one torn UTF-8 sequence costs one line rather
//! than making the entire journal unreadable. Tail reads walk backwards from
//! EOF in fixed-size blocks; full scans stream one line at a time.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

const READ_BLOCK_SIZE: u64 = 8 * 1024;

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
    Ok(tail_filter_map_inner(path, 1, None, false, |line| {
        serde_json::from_slice::<Value>(line).ok()
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
    parse: F,
) -> Result<Tail<T>>
where
    F: FnMut(&[u8]) -> Option<T>,
{
    tail_filter_map_inner(path, n, before, true, parse)
}

fn tail_filter_map_inner<T, F>(
    path: &Path,
    n: usize,
    before: Option<u64>,
    probe_older: bool,
    mut parse: F,
) -> Result<Tail<T>>
where
    F: FnMut(&[u8]) -> Option<T>,
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

    while rows.len() < target {
        let Some((offset, raw)) = reader.next_line()? else {
            break;
        };
        let line = trim_ascii(&raw);
        if line.is_empty() {
            continue;
        }
        match parse(line) {
            Some(value) => rows.push((offset, value)),
            None => corrupt_count += 1,
        }
    }

    let has_more = probe_older && rows.len() > n;
    if has_more {
        rows.truncate(n);
    }
    rows.reverse();
    let first_offset = rows.first().map(|(offset, _)| *offset);
    let events = rows.into_iter().map(|(_, value)| value).collect();

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

    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
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

    Ok(delta)
}

struct ReverseLines {
    file: File,
    read_pos: u64,
    buffer_start: u64,
    buffer: Vec<u8>,
    reached_bof: bool,
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
        }))
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
