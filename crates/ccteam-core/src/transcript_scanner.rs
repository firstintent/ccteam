//! V0.5.0 F92 — Sum cumulative cost from a Claude Code session transcript.
//!
//! Why this exists: `~/.claude/jobs/<id>/state.json::cost_usd_total`
//! reads `0` on the host even after a session has burned real dollars
//! (V0.4.6 dex-ui probe, 2026-05-16). The authoritative numbers live in
//! the transcript JSONL that `state.json::linkScanPath` points at; each
//! assistant turn writes a `message.usage` block with the four token
//! counters Anthropic's pricing API charges on.
//!
//! This module:
//!
//! 1. Resolves the transcript path from a parsed `state.json` Value —
//!    prefers `linkScanPath`, falls back to
//!    `~/.claude/projects/<cwd-with-/-as-->/<sessionId>.jsonl` (matches
//!    `queries::session_jsonl_path` and Claude Code's on-disk layout).
//! 2. Streams every `message.usage` block in the file, sums the four
//!    token counters across **all** turns, then converts to dollars via
//!    [`ccteam_cost::estimate_cost`].
//! 3. Memoizes the result keyed by `(path, mtime, len)`. The second
//!    call on an unchanged file returns the cached sum without
//!    re-reading. When the file has grown, we read **only the appended
//!    bytes** (seek to old length + parse new lines) and add to the
//!    cached total. Test-visible reset is exposed via
//!    [`reset_cache_for_tests`] so the integration tests run hermetic.
//!
//! ## Red lines (CLAUDE.md §三 + F92 PRD)
//!
//! - No new state files. The memo cache is `Mutex<HashMap<_,_>>` in
//!   process memory and exists for the daemon's lifetime.
//! - `linkScanPath` miss → `None` from the resolver; the caller decides
//!   the fallback (typically: log WARN once + use state.json's value).
//! - We sum **all** turns (not just the last one) — `queries.rs::
//!   last_usage_in_jsonl` returns only the tail block, used for
//!   context-remaining computation, NOT cost. The two scanners coexist.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

use ccteam_cost::{estimate_cost, UnifiedTokenUsage as Usage, Vendor};

/// Memoization key. `mtime` is encoded as `u64` nanos-since-epoch so
/// the type is `Hash + Eq` without leaning on `SystemTime`'s `Hash`
/// availability (which varies by Rust version).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime_nanos: u128,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    /// File length the entry's `total_usage` sum was computed against.
    /// On the next call, if the file is now larger, we seek to this
    /// offset and parse only the appended bytes.
    len: u64,
    /// Cumulative usage across all turns observed up to `len`.
    total_usage: Usage,
}

static CACHE: Mutex<Option<HashMap<CacheKey, CacheEntry>>> = Mutex::new(None);

/// Test-only instrumentation. Counts how many times we opened the
/// transcript file for *reading bytes off disk* — cache hits don't
/// increment. `t09_memoize_second_call_no_reread` reads this to assert
/// no re-open happens.
#[cfg(any(test, feature = "test-util"))]
static FILE_READ_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Read the on-disk read counter. Test-only.
#[cfg(any(test, feature = "test-util"))]
pub fn file_read_count() -> usize {
    FILE_READ_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Drop the memo cache + reset the read counter. Tests call this in
/// their setup so multi-test interleaving stays deterministic.
#[cfg(any(test, feature = "test-util"))]
pub fn reset_cache_for_tests() {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
    FILE_READ_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Compute total session cost from the transcript JSONL pointed to by
/// `state` (a parsed `~/.claude/jobs/<id>/state.json`) using `model`'s
/// rate sheet.
///
/// Returns `None` when no transcript path can be resolved (linkScanPath
/// missing AND cwd+sessionId fallback can't produce one). The caller
/// should fall back to `state.json::cost_usd_total` and log a WARN.
pub fn session_cost_from_jsonl(state: &Value, model: &str) -> Option<f64> {
    let path = resolve_jsonl_path(state)?;
    let usage = sum_usage_with_cache(&path)?;
    // Transcript JSONLs are Claude-only — `state.json::respawnFlags`
    // is a Claude-CLI artifact. Hardcode Vendor::Claude; Codex sessions
    // route their cost through `agent_done.cost_usd` direct from the
    // exec stream and never touch this scanner.
    Some(estimate_cost(&usage, Vendor::Claude, model))
}

/// Resolve the absolute jsonl transcript path. Prefer
/// `state.json::linkScanPath`, fall back to the cwd+sessionId derivation
/// claude-code itself uses on disk.
///
/// Returns `None` when neither shape produces a path (linkScanPath
/// empty AND no cwd / sessionId).
pub fn resolve_jsonl_path(state: &Value) -> Option<PathBuf> {
    if let Some(p) = state.get("linkScanPath").and_then(|v| v.as_str()) {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let cwd = state.get("cwd").and_then(|v| v.as_str())?;
    let session_id = state.get("sessionId").and_then(|v| v.as_str())?;
    let home = dirs::home_dir()?;
    let encoded = cwd.replace('/', "-");
    Some(
        home.join(".claude")
            .join("projects")
            .join(encoded)
            .join(format!("{session_id}.jsonl")),
    )
}

/// Returns the cumulative usage across all `message.usage` blocks in
/// `path`, served from cache when the (path, mtime, len) hasn't moved
/// and incrementally extended when the file has grown.
///
/// Returns `None` when the file can't be opened or has zero bytes.
fn sum_usage_with_cache(path: &Path) -> Option<Usage> {
    let meta = std::fs::metadata(path).ok()?;
    let len = meta.len();
    if len == 0 {
        return Some(Usage::default());
    }
    let mtime = meta.modified().ok()?;
    let key = CacheKey {
        path: path.to_path_buf(),
        mtime_nanos: system_time_to_nanos(mtime),
    };

    // Cache hit & no growth → return immediately.
    {
        let mut guard = CACHE.lock().ok()?;
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(entry) = map.get(&key) {
            if entry.len == len {
                return Some(entry.total_usage);
            }
            if entry.len < len {
                // File grew — read only the appended bytes and add.
                let extra = read_usage_from_offset(path, entry.len)?;
                let merged = merge_usage(&entry.total_usage, &extra);
                map.insert(
                    key,
                    CacheEntry {
                        len,
                        total_usage: merged,
                    },
                );
                return Some(merged);
            }
            // File shrank (truncate / rotate) — fall through to a full
            // re-scan; replace the cache entry below.
        }
    }

    // Full scan path.
    let total = read_usage_from_offset(path, 0)?;
    if let Ok(mut guard) = CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(
            key,
            CacheEntry {
                len,
                total_usage: total,
            },
        );
    }
    Some(total)
}

/// Parse every `message.usage` block from `offset` onwards in `path`.
/// Returns the cumulative usage observed. Lines that fail to parse are
/// skipped (matches `queries::last_usage_in_jsonl`'s tolerance).
fn read_usage_from_offset(path: &Path, offset: u64) -> Option<Usage> {
    let mut file = File::open(path).ok()?;
    #[cfg(any(test, feature = "test-util"))]
    FILE_READ_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).ok()?;
    }

    // When seeking into the middle of a file the first line may be a
    // partial fragment of a JSON object the writer hadn't finished
    // flushing when we cached the previous length. Skip that first line
    // on a mid-file seek to avoid mis-parsing.
    let reader = BufReader::new(&mut file);
    let mut acc = Usage::default();
    let mut first = true;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if first && offset > 0 {
            first = false;
            continue;
        }
        first = false;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(usage_obj) = value
            .get("message")
            .and_then(|m| m.get("usage"))
            .filter(|u| u.is_object())
        else {
            continue;
        };
        let usage: Usage = match serde_json::from_value::<Usage>(usage_obj.clone()) {
            Ok(u) => u,
            Err(_) => continue,
        };
        acc = merge_usage(&acc, &usage);
    }
    Some(acc)
}

fn merge_usage(a: &Usage, b: &Usage) -> Usage {
    fn add_opt(x: Option<u64>, y: Option<u64>) -> Option<u64> {
        match (x, y) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
    Usage {
        input_tokens: a.input_tokens + b.input_tokens,
        cached_input_tokens: a.cached_input_tokens + b.cached_input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        cache_creation_input_tokens: add_opt(
            a.cache_creation_input_tokens,
            b.cache_creation_input_tokens,
        ),
        reasoning_output_tokens: add_opt(a.reasoning_output_tokens, b.reasoning_output_tokens),
    }
}

fn system_time_to_nanos(t: SystemTime) -> u128 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // resolve_*/sum_* path coverage lives in
    // `crates/ccteam-core/tests/cost_summary_test.rs::t06..t11` —
    // those drive `session_cost_from_jsonl` end-to-end and assert the
    // dollar value, the memoization counter, and the linkScanPath
    // miss fallback. The thin in-module tests below are only the
    // ones that don't require fixture jsonl files.

    #[test]
    fn resolve_prefers_link_scan_path() {
        let state = json!({"linkScanPath": "/tmp/foo.jsonl"});
        let p = resolve_jsonl_path(&state).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/foo.jsonl"));
    }

    #[test]
    fn resolve_returns_none_when_nothing_present() {
        assert!(resolve_jsonl_path(&json!({})).is_none());
    }
}
