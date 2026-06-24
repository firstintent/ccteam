//! V0.5.0 F92 — Sum cumulative cost from a Claude Code session transcript.
//!
//! Why this exists: `~/.claude/jobs/<id>/state.json::cost_usd_total`
//! reads `0` on the host even after a session has burned real dollars
//! (V0.4.6 dex-ui probe, 2026-05-16). The authoritative numbers live in
//! the transcript JSONL that `state.json::linkScanPath` points at; each
//! assistant turn writes a `message.usage` block with the token counters
//! Anthropic's pricing API charges on, **plus** a `message.model` field
//! carrying the canonical model id for that turn.
//!
//! ## Per-turn canonical pricing (determinism)
//!
//! A single session genuinely mixes models — the transcript routinely
//! carries `claude-opus-4-8` (the user's model), `claude-sonnet-4-6`
//! (title / sub-task turns) **and** `<synthetic>` (non-billable internal
//! turns) in the SAME file. Summing all usage and pricing once with a
//! single model is therefore wrong. Instead we price EACH assistant turn
//! by its OWN `message.model` (canonical — not the `/model` alias) ×
//! that turn's `message.usage`, and sum the priced results. A turn whose
//! model is not in the pricing table (e.g. `<synthetic>`) contributes
//! nothing and is counted as an `unpriced_turn` (exposed, not faked).
//!
//! This module:
//!
//! 1. Resolves the transcript path from a parsed `state.json` Value —
//!    prefers `linkScanPath`, falls back to
//!    `~/.claude/projects/<cwd-with-/-as-->/<sessionId>.jsonl` (matches
//!    `queries::session_jsonl_path` and Claude Code's on-disk layout).
//! 2. Streams every assistant turn, prices it per-turn by its own
//!    canonical `message.model` via [`ccteam_cost::estimate_cost`], and
//!    sums the `Some` results. Returns `None` when **zero** turns priced.
//! 3. Memoizes the priced cost keyed by `(path, mtime, len)`. The second
//!    call on an unchanged file returns the cached cost without
//!    re-reading. When the file has grown, we read **only the appended
//!    bytes** (seek to old length + price new turns) and add to the
//!    cached total. Test-visible reset is exposed via
//!    [`reset_cache_for_tests`] so the integration tests run hermetic.
//!
//! ## Red lines (CLAUDE.md §三 + F92 PRD)
//!
//! - No new state files. The memo cache is `Mutex<HashMap<_,_>>` in
//!   process memory and exists for the daemon's lifetime.
//! - `linkScanPath` miss → `None` from the resolver; the caller decides
//!   the fallback (typically: log WARN once + use state.json's value).
//! - We price **all** turns (not just the last one) — `queries.rs::
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

/// Cumulative per-turn pricing result observed up to a file length.
#[derive(Debug, Clone, Copy, Default)]
struct PricedScan {
    /// Summed dollar cost of every turn that priced (its `message.model`
    /// is in the table). `0.0` is a real lower bound — distinguish "no
    /// turns at all" via [`PricedScan::priced_turns`].
    cost_usd: f64,
    /// Count of assistant turns whose `message.model` WAS in the table
    /// (contributed to `cost_usd`).
    priced_turns: usize,
    /// Count of assistant turns whose `message.model` was absent / not in
    /// the table (e.g. `<synthetic>`) — exposed, not faked.
    unpriced_turns: usize,
}

impl PricedScan {
    fn merge(&self, other: &PricedScan) -> PricedScan {
        PricedScan {
            cost_usd: self.cost_usd + other.cost_usd,
            priced_turns: self.priced_turns + other.priced_turns,
            unpriced_turns: self.unpriced_turns + other.unpriced_turns,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    /// File length the entry's `scan` was computed against. On the next
    /// call, if the file is now larger, we seek to this offset and price
    /// only the appended turns.
    len: u64,
    /// Cumulative per-turn pricing across all turns observed up to `len`.
    scan: PricedScan,
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
/// `state` (a parsed `~/.claude/jobs/<id>/state.json`), pricing **each**
/// assistant turn by its OWN canonical `message.model` (the deterministic
/// source) × that turn's usage.
///
/// Returns `None` when:
/// - no transcript path can be resolved (linkScanPath missing AND
///   cwd+sessionId fallback can't produce one) — caller falls back to
///   `state.json::cost_usd_total` and logs a WARN; OR
/// - the transcript has **zero** priceable turns (no assistant turn with
///   a table-matched model) — the cost is genuinely unknown, exposed as
///   `None` (rendered "—") rather than a fabricated `0.0`.
///
/// A transcript that mixes priced + unpriced turns returns the sum of the
/// priced ones (a known lower bound); the unpriced models are surfaced
/// via the WARN-once in `estimate_cost`.
pub fn session_cost_from_jsonl(state: &Value) -> Option<f64> {
    let path = resolve_jsonl_path(state)?;
    let scan = priced_scan_with_cache(&path)?;
    // Zero priced turns → genuinely unknown cost. Expose as None so the
    // caller renders "—"/excludes it, never a fake 0.0. (A real all-zero
    // priced session is vanishingly rare and still honestly "≈ $0".)
    if scan.priced_turns == 0 {
        return None;
    }
    Some(scan.cost_usd)
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

/// Returns the cumulative per-turn pricing across all assistant turns in
/// `path`, served from cache when the (path, mtime, len) hasn't moved and
/// incrementally extended when the file has grown. Each turn is priced by
/// its OWN canonical `message.model`.
///
/// Returns `None` when the file can't be opened or has zero bytes.
fn priced_scan_with_cache(path: &Path) -> Option<PricedScan> {
    let meta = std::fs::metadata(path).ok()?;
    let len = meta.len();
    if len == 0 {
        return Some(PricedScan::default());
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
                return Some(entry.scan);
            }
            if entry.len < len {
                // File grew — price only the appended turns and add.
                let extra = price_turns_from_offset(path, entry.len)?;
                let merged = entry.scan.merge(&extra);
                map.insert(key, CacheEntry { len, scan: merged });
                return Some(merged);
            }
            // File shrank (truncate / rotate) — fall through to a full
            // re-scan; replace the cache entry below.
        }
    }

    // Full scan path.
    let total = price_turns_from_offset(path, 0)?;
    if let Ok(mut guard) = CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(key, CacheEntry { len, scan: total });
    }
    Some(total)
}

/// Price every assistant turn from `offset` onwards in `path`, each by its
/// own canonical `message.model` × its `message.usage`. Returns the
/// cumulative [`PricedScan`]. Lines that fail to parse are skipped
/// (matches `queries::last_usage_in_jsonl`'s tolerance).
///
/// Transcript JSONLs are Claude-only (`state.json` / `linkScanPath` is a
/// Claude-CLI artifact; Codex routes cost through `agent_done.cost_usd`
/// from the exec stream and never touches this scanner) → price as
/// [`Vendor::Claude`].
fn price_turns_from_offset(path: &Path, offset: u64) -> Option<PricedScan> {
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
    let mut acc = PricedScan::default();
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
        let Some(message) = value.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let Some(usage_obj) = message.get("usage").filter(|u| u.is_object()) else {
            continue;
        };
        let usage: Usage = match serde_json::from_value::<Usage>(usage_obj.clone()) {
            Ok(u) => u,
            Err(_) => continue,
        };
        // The canonical model id for THIS turn (e.g. `claude-opus-4-8`).
        // Absent → unpriceable (counted, contributes nothing).
        let model = message.get("model").and_then(|v| v.as_str()).unwrap_or("");
        match estimate_cost(&usage, Vendor::Claude, model) {
            Some(cost) => {
                acc.cost_usd += cost;
                acc.priced_turns += 1;
            }
            None => acc.unpriced_turns += 1,
        }
    }
    Some(acc)
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
