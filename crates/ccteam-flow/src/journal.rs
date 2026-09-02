//! Append-only run journal and the resume cache built from it.
//!
//! A workflow that hires 200 agents must survive being killed at agent 137.
//! The journal is what makes that true: one line per `agent()` call, written
//! twice — at dispatch (as soon as the sid exists, so a crashed run can
//! re-attach instead of re-hiring) and at completion (with the result).
//!
//! Resume re-executes the *script*, not the journal: the script is the
//! program, the journal is only its memo table. Call `n` is answered from
//! cache when its position and content key both match. The FIRST mismatch
//! invalidates everything after it — borrowed from travisliu's
//! prefix-cache-invalidate-on-first-mismatch, and it is the only safe rule:
//! once one call's inputs changed, every later call may have been reached
//! through different control flow, so its cached answer means nothing.
//!
//! Write failures WARN and never fail the run. A run that completes without a
//! journal is degraded (no resume); a run aborted because a disk was full
//! would be worse.

use crate::error::FlowError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Results larger than this go to `results/<seq>.json` and the journal line
/// carries a reference. Keeps `journal.jsonl` scannable by eye and by `tail`
/// even when a workflow returns 200 KB documents.
const INLINE_RESULT_LIMIT: usize = 4096;

/// One `agent()` call as recorded on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalEntry {
    /// Position in the run's call order. The cache key's first half.
    pub seq: usize,
    /// `sha256(task + canonical(opts))`. The cache key's second half.
    pub key: String,
    /// Session the task was dispatched to, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// False on the dispatch line, true on the completion line.
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Set instead of `result` when the value was too large to inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
}

/// What the resume cache can offer for call `seq`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PrefixHit {
    /// Nothing usable — run this call live.
    Miss,
    /// The call already finished; here is its answer, free.
    Done {
        result: Value,
        cost_usd: Option<f64>,
    },
    /// The call was dispatched but never finished. Re-attach to the session
    /// rather than hiring a second worker for the same task.
    InFlight { sid: String },
}

/// Resume diagnostics surfaced on the run report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheReport {
    /// Calls answered from the journal (no client traffic).
    pub hits: usize,
    /// Calls re-attached to a still-running session.
    pub reattached: usize,
    /// Call index at which the prefix stopped matching, if it did.
    pub invalidated_at: Option<usize>,
    /// Human-readable reason for that first mismatch.
    pub diagnostic: Option<String>,
}

/// Stable content key for one `agent()` call.
///
/// Canonicalised so JS object key order cannot change the key: the same call
/// written `{vendor:'codex', label:'x'}` and `{label:'x', vendor:'codex'}`
/// must resume from the same cache entry.
/// Read (resume) or mint (fresh) the run's identity token in `dir/run.id`.
/// Host-side entropy is fine here — the determinism ban protects script
/// space, and this token must differ BETWEEN runs by construction.
fn run_token_in(dir: &Path, resume: bool) -> Result<String, FlowError> {
    let path = dir.join("run.id");
    if resume {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let existing = existing.trim().to_string();
            if !existing.is_empty() {
                return Ok(existing);
            }
        }
        // A resume without a token (pre-token journal or lost file) mints one:
        // worse dedup for in-flight dispatches, never a wrong answer.
    }
    let mut hasher = Sha256::new();
    hasher.update(dir.as_os_str().as_encoded_bytes());
    hasher.update(std::process::id().to_le_bytes());
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(now.as_nanos().to_le_bytes());
    }
    let token = format!("{:x}", hasher.finalize())[..16].to_string();
    std::fs::write(&path, &token).map_err(|source| FlowError::RunDir {
        path: path.clone(),
        source,
    })?;
    Ok(token)
}

pub fn call_key(task: &str, opts: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(task.as_bytes());
    hasher.update(b"\0");
    hasher.update(canonical(opts).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Key-sorted, null-stripped JSON. Nulls are dropped so an explicitly-passed
/// `{model: null}` keys the same as an omitted `model`.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .filter(|k| !map[*k].is_null())
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

struct PriorState {
    by_seq: BTreeMap<usize, JournalEntry>,
    /// Flipped false at the first mismatch; never flips back.
    usable: bool,
    invalidated_at: Option<usize>,
    diagnostic: Option<String>,
    hits: usize,
    reattached: usize,
}

/// The run directory's writer plus its resume view.
pub(crate) struct Journal {
    dir: PathBuf,
    /// Identity of THIS run, persisted in `run.id`: minted on a fresh run,
    /// re-read on resume. Folded into every hire's idempotency key so a
    /// crash-then-resume replays its own dispatches at the daemon, while two
    /// different runs (even into a reused directory) can never collide.
    token: String,
    /// `None` when the file could not be opened; every append then warns once
    /// per call and moves on.
    file: Mutex<Option<std::fs::File>>,
    prior: Mutex<PriorState>,
}

impl Journal {
    /// This run's persisted identity (see `run_token_in`).
    pub(crate) fn run_token(&self) -> &str {
        &self.token
    }

    /// Prepare `dir` and, when resuming, load the previous journal.
    pub(crate) fn open(dir: &Path, resume: bool) -> Result<Self, FlowError> {
        std::fs::create_dir_all(dir.join("results")).map_err(|source| FlowError::RunDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let journal_path = dir.join("journal.jsonl");
        let token = run_token_in(dir, resume)?;

        let by_seq = if resume {
            load_prior(&journal_path)
        } else {
            BTreeMap::new()
        };

        // Truncate on a fresh run so a re-run into a reused directory does not
        // resume by accident; append on resume so history is preserved.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(resume)
            .write(true)
            .truncate(!resume)
            .open(&journal_path)
            .map_err(|source| FlowError::RunDir {
                path: journal_path,
                source,
            })?;

        Ok(Self {
            dir: dir.to_path_buf(),
            token,
            file: Mutex::new(Some(file)),
            prior: Mutex::new(PriorState {
                by_seq,
                usable: resume,
                invalidated_at: None,
                diagnostic: None,
                hits: 0,
                reattached: 0,
            }),
        })
    }

    /// Write the self-describing header files: the exact script text, the
    /// parsed meta and the args. A run directory should be readable months
    /// later without the caller's shell history.
    pub(crate) fn write_manifest(&self, script: &str, meta: &Value, args: &Value) {
        warn_on_err(
            "script.js",
            std::fs::write(self.dir.join("script.js"), script),
        );
        let manifest = serde_json::json!({ "meta": meta, "args": args });
        warn_on_err(
            "run.json",
            serde_json::to_vec_pretty(&manifest)
                .map_err(std::io::Error::other)
                .and_then(|b| std::fs::write(self.dir.join("run.json"), b)),
        );
    }

    /// Append one line. Large results spill to `results/<seq>.json` first.
    pub(crate) fn append(&self, mut entry: JournalEntry) {
        if let Some(result) = &entry.result {
            let encoded = result.to_string();
            if encoded.len() > INLINE_RESULT_LIMIT {
                let rel = format!("results/{}.json", entry.seq);
                warn_on_err(&rel, std::fs::write(self.dir.join(&rel), encoded));
                entry.result = None;
                entry.result_ref = Some(rel);
            }
        }
        let Ok(mut line) = serde_json::to_string(&entry) else {
            tracing::warn!(seq = entry.seq, "journal: entry is not serialisable");
            return;
        };
        line.push('\n');
        let mut guard = self.file.lock().expect("journal file mutex poisoned");
        if let Some(file) = guard.as_mut() {
            if let Err(err) = file.write_all(line.as_bytes()) {
                tracing::warn!(%err, "journal: append failed; resume will be incomplete");
            }
        }
    }

    /// Consult the resume cache for call `seq` with content key `key`.
    pub(crate) fn lookup(&self, seq: usize, key: &str, label: &str) -> PrefixHit {
        let mut prior = self.prior.lock().expect("journal prior mutex poisoned");
        if !prior.usable {
            return PrefixHit::Miss;
        }
        let Some(entry) = prior.by_seq.get(&seq).cloned() else {
            prior.invalidate(seq, format!("call #{seq} ({label}) has no journal entry"));
            return PrefixHit::Miss;
        };
        if entry.key != key {
            let diag = format!(
                "call #{seq} ({label}) changed: journal key {}… vs script key {}…",
                short(&entry.key),
                short(key)
            );
            prior.invalidate(seq, diag);
            return PrefixHit::Miss;
        }
        if entry.done {
            prior.hits += 1;
            let result = entry
                .result
                .clone()
                .or_else(|| self.read_result_ref(entry.result_ref.as_deref()))
                .unwrap_or(Value::Null);
            return PrefixHit::Done {
                result,
                cost_usd: entry.cost_usd,
            };
        }
        match entry.sid {
            Some(sid) => {
                prior.reattached += 1;
                PrefixHit::InFlight { sid }
            }
            // Dispatched-but-no-sid means the previous run died before the
            // hire returned. Nothing to re-attach to; run it live.
            None => {
                prior.invalidate(
                    seq,
                    format!("call #{seq} ({label}) was never dispatched in the prior run"),
                );
                PrefixHit::Miss
            }
        }
    }

    fn read_result_ref(&self, rel: Option<&str>) -> Option<Value> {
        let rel = rel?;
        let raw = std::fs::read_to_string(self.dir.join(rel)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub(crate) fn report(&self) -> CacheReport {
        let prior = self.prior.lock().expect("journal prior mutex poisoned");
        CacheReport {
            hits: prior.hits,
            reattached: prior.reattached,
            invalidated_at: prior.invalidated_at,
            diagnostic: prior.diagnostic.clone(),
        }
    }
}

impl PriorState {
    fn invalidate(&mut self, seq: usize, reason: String) {
        if self.usable {
            self.usable = false;
            self.invalidated_at = Some(seq);
            self.diagnostic = Some(format!("{reason}; running live from here"));
        }
    }
}

fn short(key: &str) -> &str {
    &key[..key.len().min(8)]
}

/// Read a prior journal, letting later lines for the same `seq` win — the
/// completion line always overwrites its dispatch line.
fn load_prior(path: &Path) -> BTreeMap<usize, JournalEntry> {
    let mut out = BTreeMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) => {
                out.insert(entry.seq, entry);
            }
            Err(err) => tracing::warn!(%err, "journal: skipping unparseable line"),
        }
    }
    out
}

fn warn_on_err(what: &str, res: std::io::Result<()>) {
    if let Err(err) = res {
        tracing::warn!(%err, file = what, "journal: write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(seq: usize, key: &str, done: bool, sid: Option<&str>, result: Value) -> JournalEntry {
        JournalEntry {
            seq,
            key: key.to_string(),
            sid: sid.map(str::to_string),
            done,
            result: Some(result),
            result_ref: None,
            cost_usd: Some(0.5),
            label: Some(format!("call {seq}")),
            vendor: None,
        }
    }

    #[test]
    fn key_ignores_option_order_and_explicit_nulls() {
        let a = call_key("do it", &json!({"vendor": "codex", "label": "x"}));
        let b = call_key("do it", &json!({"label": "x", "vendor": "codex"}));
        let c = call_key(
            "do it",
            &json!({"label": "x", "vendor": "codex", "model": null}),
        );
        assert_eq!(a, b, "JS key order must not change the cache key");
        assert_eq!(a, c, "an explicit null must key like an omitted option");
    }

    #[test]
    fn key_changes_with_the_task() {
        assert_ne!(
            call_key("a", &json!({})),
            call_key("b", &json!({})),
            "a different task is a different call"
        );
    }

    #[test]
    fn fresh_run_has_no_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::open(dir.path(), false).expect("open");
        assert_eq!(j.lookup(0, "k0", "first"), PrefixHit::Miss);
        assert_eq!(j.report().hits, 0);
    }

    #[test]
    fn resume_returns_completed_calls_and_reattaches_in_flight_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let j = Journal::open(dir.path(), false).expect("open");
            j.append(entry(0, "k0", true, Some("s1"), json!("answer")));
            j.append(entry(1, "k1", false, Some("s2"), Value::Null));
        }
        let j = Journal::open(dir.path(), true).expect("reopen");
        assert_eq!(
            j.lookup(0, "k0", "first"),
            PrefixHit::Done {
                result: json!("answer"),
                cost_usd: Some(0.5)
            }
        );
        assert_eq!(
            j.lookup(1, "k1", "second"),
            PrefixHit::InFlight {
                sid: "s2".to_string()
            }
        );
        let report = j.report();
        assert_eq!(report.hits, 1);
        assert_eq!(report.reattached, 1);
        assert_eq!(report.invalidated_at, None);
    }

    #[test]
    fn a_completion_line_overrides_its_dispatch_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let j = Journal::open(dir.path(), false).expect("open");
            j.append(entry(0, "k0", false, Some("s1"), Value::Null));
            j.append(entry(0, "k0", true, Some("s1"), json!("final")));
        }
        let j = Journal::open(dir.path(), true).expect("reopen");
        assert!(matches!(j.lookup(0, "k0", "first"), PrefixHit::Done { .. }));
    }

    #[test]
    fn first_mismatch_invalidates_everything_after_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let j = Journal::open(dir.path(), false).expect("open");
            for seq in 0..3 {
                j.append(entry(seq, &format!("k{seq}"), true, Some("s"), json!(seq)));
            }
        }
        let j = Journal::open(dir.path(), true).expect("reopen");
        assert!(matches!(j.lookup(0, "k0", "a"), PrefixHit::Done { .. }));
        // Call 1's task was edited: its key no longer matches.
        assert_eq!(j.lookup(1, "EDITED", "b"), PrefixHit::Miss);
        // Everything after it is live even though its key still matches,
        // because control flow may have diverged.
        assert_eq!(j.lookup(2, "k2", "c"), PrefixHit::Miss);

        let report = j.report();
        assert_eq!(report.hits, 1);
        assert_eq!(report.invalidated_at, Some(1));
        let diagnostic = report.diagnostic.expect("a mismatch must be explained");
        assert!(diagnostic.contains("call #1"), "{diagnostic}");
        assert!(
            diagnostic.contains('b'),
            "label should be named: {diagnostic}"
        );
    }

    #[test]
    fn a_script_that_grew_invalidates_at_the_new_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let j = Journal::open(dir.path(), false).expect("open");
            j.append(entry(0, "k0", true, Some("s"), json!(0)));
        }
        let j = Journal::open(dir.path(), true).expect("reopen");
        assert!(matches!(j.lookup(0, "k0", "a"), PrefixHit::Done { .. }));
        assert_eq!(j.lookup(1, "k1", "new call"), PrefixHit::Miss);
        assert_eq!(j.report().invalidated_at, Some(1));
    }

    #[test]
    fn large_results_spill_to_a_file_and_come_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let big = "x".repeat(INLINE_RESULT_LIMIT + 100);
        {
            let j = Journal::open(dir.path(), false).expect("open");
            j.append(entry(0, "k0", true, Some("s"), json!(big)));
        }
        let line = std::fs::read_to_string(dir.path().join("journal.jsonl")).expect("journal");
        assert!(
            line.contains("result_ref"),
            "a large result must not be inlined: {}",
            &line[..line.len().min(200)]
        );
        assert!(dir.path().join("results/0.json").exists());

        let j = Journal::open(dir.path(), true).expect("reopen");
        assert_eq!(
            j.lookup(0, "k0", "a"),
            PrefixHit::Done {
                result: json!(big),
                cost_usd: Some(0.5)
            }
        );
    }

    #[test]
    fn a_fresh_run_truncates_a_reused_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let j = Journal::open(dir.path(), false).expect("open");
            j.append(entry(0, "k0", true, Some("s"), json!("old")));
        }
        {
            let _ = Journal::open(dir.path(), false).expect("reopen fresh");
        }
        let raw = std::fs::read_to_string(dir.path().join("journal.jsonl")).expect("journal");
        assert!(raw.is_empty(), "a non-resume run must not inherit history");
    }

    #[test]
    fn manifest_records_script_meta_and_args() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::open(dir.path(), false).expect("open");
        j.write_manifest("export const meta = {}", &json!({"name": "x"}), &json!([1]));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("script.js")).expect("script"),
            "export const meta = {}"
        );
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("run.json")).expect("run.json"),
        )
        .expect("json");
        assert_eq!(manifest["meta"]["name"], json!("x"));
        assert_eq!(manifest["args"], json!([1]));
    }

    #[test]
    fn unparseable_journal_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("journal.jsonl"),
            "not json\n{\"seq\":0,\"key\":\"k0\",\"done\":true,\"result\":\"v\"}\n",
        )
        .expect("seed");
        let j = Journal::open(dir.path(), true).expect("open");
        assert!(matches!(j.lookup(0, "k0", "a"), PrefixHit::Done { .. }));
    }
}
