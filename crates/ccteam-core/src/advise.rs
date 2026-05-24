//! V0.6.5 F152 + F153 — `advise_vote` / `advise_parallel` real impls.
//!
//! Replaces the V0.6.0 Wave 1 STUB dispatch in
//! `ccteam-cli/src/mcp_advise_tools.rs`. Provides two public entry
//! points:
//!
//! - [`advise_vote`] — fans one hard question to Claude + Codex in
//!   parallel, then runs a third Claude call to synthesise a verdict
//!   (agreement / disagreement / suggested winning approach).
//! - [`advise_parallel`] — fans the same question to N parallel advisor
//!   sessions (vendors round-robin); no synthesis.
//!
//! ## Vendor adapter paths
//!
//! - **Claude advisor** invokes `claude -p <prompt> --output-format text
//!   --dangerously-skip-permissions` as a one-shot subprocess. The
//!   `CCTEAM_CLAUDE_BIN` env override redirects the binary path so
//!   hermetic tests can inject a fake script.
//! - **Codex advisor** invokes `codex exec --json -` (prompt via stdin)
//!   through the same path the [`super::execution::codex_exec`] adapter
//!   uses, then folds the JSONL stream to the final assistant message.
//!   `CCTEAM_CODEX_BIN` env override mirrors the bg adapter for tests.
//!
//! ## Codex availability
//!
//! Per CLAUDE.md §三 "vendor red line" + the `ccteam-advise` skill's
//! "Codex unavailable" surface requirement, when the codex binary is
//! not on PATH (or `CCTEAM_CODEX_BIN` points at a non-executable file),
//! the Codex slot returns `CodexStatus::Unavailable { reason }` and the
//! synthesised verdict explicitly says "Codex unavailable: <reason>"
//! — we never silently downgrade to "Claude-only" without flagging it.
//!
//! ## Budget enforcement
//!
//! Each `advise_*` call records an approximate token cost into
//! `~/.ccteam/cost-budget.json::advise_today_usd` (rolling 24h sum).
//! Before fan-out, we check `max_cost_usd_per_24h` (defaulting to
//! `DEFAULT_ADVISE_BUDGET_USD_24H` when no explicit cap is supplied)
//! and refuse the call with `BudgetExceeded` when the cap is reached.
//! Per-vendor sub-tracking (claude vs codex spend) is preserved so
//! future per-vendor caps land in the same file shape.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::execution::codex_app_server::CODEX_BIN_ENV;
use crate::harness::{AgentVendor, CLAUDE_BIN_ENV};

/// Per-vendor word-rate proxy used when synthesising an upper-bound
/// cost for the advise budget ledger. Each Claude / Codex one-shot
/// advisor call is small (~250 words capped by the skill prompt); we
/// stick to a single rough multiplier rather than parsing usage out of
/// the vendor JSONL (Claude's `-p` text mode has no usage block, and
/// Codex's `--json` does but we charge a flat estimate either way so
/// the budget ledger keeps the same shape across vendors).
///
/// Default ~3.5 USD / 1M tokens, scaled to a typical ~1500-token
/// roundtrip → ~0.005 USD per advisor call. Conservative enough that
/// the V0.6.5 default cap (`DEFAULT_ADVISE_BUDGET_USD_24H`) keeps
/// hundreds of calls/day workable while preventing runaway loops.
pub const APPROX_COST_PER_CALL_USD: f64 = 0.005;

/// V0.6.5 F152 — fallback rolling 24h cap on advise calls when no
/// per-vendor cap is supplied at MCP invocation time. 0.50 USD = 100
/// advisor calls at the [`APPROX_COST_PER_CALL_USD`] estimate. Caller
/// can override via the `max_cost_usd` MCP arg.
pub const DEFAULT_ADVISE_BUDGET_USD_24H: f64 = 0.50;

/// V0.6.5 F152 — default per-vendor wall clock for advisor subprocesses.
/// 60s lines up with the `ccteam-advise` skill's documented Step 1
/// fan-out cap; tests override via the `codex_timeout_secs` MCP arg.
pub const DEFAULT_CODEX_TIMEOUT_SECS: u64 = 60;

/// One vendor's slot in an `advise_vote` outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum CodexStatus {
    /// Codex returned an assistant message.
    Ok,
    /// Codex binary not on PATH (or `CCTEAM_CODEX_BIN` not executable).
    Unavailable { reason: String },
    /// Codex spawn / exec returned non-zero or stream parse failed.
    Error { reason: String },
    /// Codex did not return within `codex_timeout_secs`.
    Timeout,
}

/// Synthesised verdict shape returned to the MCP caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteResult {
    pub ok: bool,
    pub question: String,
    pub verdict: String,
    pub claude_answer: String,
    pub codex_answer: Option<String>,
    pub codex_status: CodexStatus,
    pub agreement: Agreement,
    pub budget: BudgetSnapshot,
}

/// Coarse agreement classifier — the skill prints this above the
/// verdict so the user can scan once before diving into the per-vendor
/// answers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Agreement {
    Agree,
    Disagree,
    Partial,
    /// Codex didn't return (unavailable / timeout / error) so a 2-way
    /// agreement is undefined.
    Unknown,
}

/// One vendor row inside [`ParallelResult::answers`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorAnswer {
    pub vendor: AgentVendor,
    pub answer: String,
    pub status: AnswerStatus,
}

/// Status of a single advisor slot inside `advise_parallel`. Mirrors
/// [`CodexStatus`] but vendor-agnostic so both Claude and Codex slots
/// share one shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum AnswerStatus {
    Ok,
    Unavailable { reason: String },
    Error { reason: String },
    Timeout,
}

/// `advise_parallel` return shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelResult {
    pub ok: bool,
    pub question: String,
    pub answers: Vec<VendorAnswer>,
    pub budget: BudgetSnapshot,
}

/// Persistent advise-budget ledger written to
/// `<ccteam_root>/cost-budget.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdviseBudgetLedger {
    /// Rolling-window samples. Each entry = one advise call
    /// (vendor + cost USD + UTC ts). The 24h sum is recomputed on
    /// every check.
    #[serde(default)]
    pub samples: Vec<BudgetSample>,
}

/// One ledger row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSample {
    pub vendor: AgentVendor,
    pub usd: f64,
    pub ts: DateTime<Utc>,
}

/// Read-only snapshot returned to the MCP caller so the skill can
/// surface budget headroom without re-reading the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub advise_today_usd: f64,
    pub cap_usd: f64,
}

/// Errors `advise_vote` / `advise_parallel` surface to the MCP layer.
#[derive(Debug, thiserror::Error)]
pub enum AdviseError {
    #[error("budget_exceeded: advise spend over the last 24h ({spent_usd:.4} USD) ≥ cap ({cap_usd:.4} USD)")]
    BudgetExceeded { spent_usd: f64, cap_usd: f64 },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("claude advisor failed: {0}")]
    ClaudeFailed(String),
    #[error("verdict synthesis failed: {0}")]
    VerdictFailed(String),
}

/// V0.6.5 F152 — entry point for the `ccteam__advise_vote` MCP tool.
///
/// `max_cost_usd` is the caller-overridable rolling 24h cap; `None`
/// falls back to [`DEFAULT_ADVISE_BUDGET_USD_24H`].
pub async fn advise_vote(
    ccteam_root: &Path,
    question: &str,
    context: Option<&str>,
    codex_timeout_secs: Option<u64>,
    max_cost_usd: Option<f64>,
) -> Result<VoteResult, AdviseError> {
    if question.trim().is_empty() {
        return Err(AdviseError::InvalidInput("`question` is empty".into()));
    }
    let cap = max_cost_usd.unwrap_or(DEFAULT_ADVISE_BUDGET_USD_24H);
    let timeout = Duration::from_secs(codex_timeout_secs.unwrap_or(DEFAULT_CODEX_TIMEOUT_SECS));

    // Budget pre-check — refuse before spending API quota.
    let pre_spent = load_budget_ledger(ccteam_root)
        .map(|l| sum_advise_today(&l))
        .unwrap_or(0.0);
    if pre_spent >= cap {
        return Err(AdviseError::BudgetExceeded {
            spent_usd: pre_spent,
            cap_usd: cap,
        });
    }

    let full_prompt = compose_advisor_prompt(question, context);

    // Fan-out: Claude + Codex in parallel.
    let claude_fut = run_claude_advisor(&full_prompt);
    let codex_fut = run_codex_advisor(&full_prompt, timeout);
    let (claude_res, codex_res) = tokio::join!(claude_fut, codex_fut);

    let claude_answer = claude_res.map_err(|e| AdviseError::ClaudeFailed(e.to_string()))?;
    let (codex_answer, codex_status) = match codex_res {
        Ok(text) => (Some(text), CodexStatus::Ok),
        Err(CodexCallError::Unavailable(r)) => (None, CodexStatus::Unavailable { reason: r }),
        Err(CodexCallError::Timeout) => (None, CodexStatus::Timeout),
        Err(CodexCallError::Other(r)) => (None, CodexStatus::Error { reason: r }),
    };

    // Record one cost sample per vendor that actually returned. Codex
    // unavailable / timeout / error → no charge.
    if let Err(err) =
        append_budget_sample(ccteam_root, AgentVendor::Claude, APPROX_COST_PER_CALL_USD)
    {
        tracing::warn!(error = %err, "advise_vote: failed to record claude cost sample");
    }
    if matches!(codex_status, CodexStatus::Ok) {
        if let Err(err) =
            append_budget_sample(ccteam_root, AgentVendor::Codex, APPROX_COST_PER_CALL_USD)
        {
            tracing::warn!(error = %err, "advise_vote: failed to record codex cost sample");
        }
    }

    // Synthesise verdict — third Claude call. When Codex is unavailable
    // the verdict body must explicitly say so (red line).
    let verdict = synthesise_verdict(
        question,
        &claude_answer,
        codex_answer.as_deref(),
        &codex_status,
    )
    .await
    .map_err(|e| AdviseError::VerdictFailed(e.to_string()))?;

    // Tertiary Claude charge for the verdict call.
    if let Err(err) =
        append_budget_sample(ccteam_root, AgentVendor::Claude, APPROX_COST_PER_CALL_USD)
    {
        tracing::warn!(error = %err, "advise_vote: failed to record verdict cost sample");
    }

    let agreement = classify_agreement(&claude_answer, codex_answer.as_deref(), &codex_status);

    let post_spent = load_budget_ledger(ccteam_root)
        .map(|l| sum_advise_today(&l))
        .unwrap_or(0.0);

    Ok(VoteResult {
        ok: true,
        question: question.to_string(),
        verdict,
        claude_answer,
        codex_answer,
        codex_status,
        agreement,
        budget: BudgetSnapshot {
            advise_today_usd: post_spent,
            cap_usd: cap,
        },
    })
}

/// V0.6.5 F153 — entry point for the `ccteam__advise_parallel` MCP
/// tool. N-of-N raw answers (no verdict synthesis). `vendors` rotates
/// round-robin when `vendors.len() < n`.
pub async fn advise_parallel(
    ccteam_root: &Path,
    question: &str,
    context: Option<&str>,
    n: usize,
    vendors: &[AgentVendor],
    timeout_secs: Option<u64>,
    max_cost_usd: Option<f64>,
) -> Result<ParallelResult, AdviseError> {
    if question.trim().is_empty() {
        return Err(AdviseError::InvalidInput("`question` is empty".into()));
    }
    if !(2..=8).contains(&n) {
        return Err(AdviseError::InvalidInput(format!(
            "`n` must be in 2..=8, got {n}"
        )));
    }
    if vendors.is_empty() {
        return Err(AdviseError::InvalidInput(
            "`vendors` must not be empty".into(),
        ));
    }
    if vendors.len() > n {
        return Err(AdviseError::InvalidInput(format!(
            "`vendors.len() ({}) > n ({})`",
            vendors.len(),
            n
        )));
    }
    let cap = max_cost_usd.unwrap_or(DEFAULT_ADVISE_BUDGET_USD_24H);
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_CODEX_TIMEOUT_SECS));

    let pre_spent = load_budget_ledger(ccteam_root)
        .map(|l| sum_advise_today(&l))
        .unwrap_or(0.0);
    if pre_spent >= cap {
        return Err(AdviseError::BudgetExceeded {
            spent_usd: pre_spent,
            cap_usd: cap,
        });
    }

    let full_prompt = compose_advisor_prompt(question, context);

    // Round-robin vendors → exactly N slots.
    let slot_vendors: Vec<AgentVendor> = (0..n).map(|i| vendors[i % vendors.len()]).collect();

    let mut futs = Vec::with_capacity(n);
    for v in slot_vendors.iter().copied() {
        let prompt = full_prompt.clone();
        futs.push(async move {
            match v {
                AgentVendor::Claude => match run_claude_advisor(&prompt).await {
                    Ok(text) => (v, AnswerStatus::Ok, text),
                    Err(e) => (
                        v,
                        AnswerStatus::Error {
                            reason: e.to_string(),
                        },
                        String::new(),
                    ),
                },
                AgentVendor::Codex => match run_codex_advisor(&prompt, timeout).await {
                    Ok(text) => (v, AnswerStatus::Ok, text),
                    Err(CodexCallError::Unavailable(r)) => {
                        (v, AnswerStatus::Unavailable { reason: r }, String::new())
                    }
                    Err(CodexCallError::Timeout) => (v, AnswerStatus::Timeout, String::new()),
                    Err(CodexCallError::Other(r)) => {
                        (v, AnswerStatus::Error { reason: r }, String::new())
                    }
                },
            }
        });
    }
    let results = futures::future::join_all(futs).await;

    let mut answers = Vec::with_capacity(n);
    for (vendor, status, answer) in results {
        if matches!(status, AnswerStatus::Ok) {
            if let Err(err) = append_budget_sample(ccteam_root, vendor, APPROX_COST_PER_CALL_USD) {
                tracing::warn!(error = %err, vendor = ?vendor, "advise_parallel: failed to record cost sample");
            }
        }
        answers.push(VendorAnswer {
            vendor,
            answer,
            status,
        });
    }

    let post_spent = load_budget_ledger(ccteam_root)
        .map(|l| sum_advise_today(&l))
        .unwrap_or(0.0);

    Ok(ParallelResult {
        ok: true,
        question: question.to_string(),
        answers,
        budget: BudgetSnapshot {
            advise_today_usd: post_spent,
            cap_usd: cap,
        },
    })
}

// ============================================================
// Vendor adapter helpers
// ============================================================

/// Compose the per-vendor prompt. Inline the optional context so both
/// vendors see exactly the same payload (no positional drift).
fn compose_advisor_prompt(question: &str, context: Option<&str>) -> String {
    let critic_preamble = r#"You are a critical advisor. Read the question, return:
(a) your recommendation (one sentence),
(b) the 2-3 strongest supporting reasons,
(c) any dealbreaker risks or caveats.
Keep total response under 250 words. Be specific."#;
    match context {
        Some(c) if !c.trim().is_empty() => {
            format!("{critic_preamble}\n\nContext:\n{c}\n\nQuestion:\n{question}")
        }
        _ => format!("{critic_preamble}\n\nQuestion:\n{question}"),
    }
}

/// Internal error from the codex one-shot subprocess. Mapped to
/// [`CodexStatus`] / [`AnswerStatus`] at the boundary.
#[derive(Debug)]
enum CodexCallError {
    Unavailable(String),
    Timeout,
    Other(String),
}

impl std::fmt::Display for CodexCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(r) => write!(f, "codex unavailable: {r}"),
            Self::Timeout => write!(f, "codex timeout"),
            Self::Other(r) => write!(f, "codex error: {r}"),
        }
    }
}

/// Resolve the claude binary path. Mirrors `claude_bg::start_thread`'s
/// behaviour — `CCTEAM_CLAUDE_BIN` override before PATH default.
fn claude_bin() -> String {
    std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

/// Resolve the codex binary path. Same env var the codex_exec adapter
/// uses so tests can swap both adapters at once.
fn codex_bin() -> String {
    std::env::var(CODEX_BIN_ENV).unwrap_or_else(|_| "codex".to_string())
}

/// Run `claude -p <prompt>` as a one-shot subprocess, returning the
/// assistant text on stdout. The Claude CLI's `-p` "print" mode writes
/// the answer directly to stdout and exits 0 — no JSON parsing needed.
async fn run_claude_advisor(prompt: &str) -> Result<String, AdviseError> {
    let bin = claude_bin();
    let mut cmd = Command::new(&bin);
    cmd.arg("-p")
        .arg("--output-format")
        .arg("text")
        .arg("--dangerously-skip-permissions")
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| AdviseError::ClaudeFailed(format!("spawn {bin}: {e}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdviseError::ClaudeFailed("missing stdout pipe".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| AdviseError::ClaudeFailed("missing stderr pipe".into()))?;

    let mut buf = String::new();
    let read_stdout = async {
        stdout
            .read_to_string(&mut buf)
            .await
            .map(|_| ())
            .map_err(|e| AdviseError::ClaudeFailed(format!("read stdout: {e}")))?;
        Ok::<String, AdviseError>(buf)
    };
    let mut err_buf = String::new();
    let read_stderr = async {
        let _ = stderr.read_to_string(&mut err_buf).await;
    };
    let (out, _) = tokio::join!(read_stdout, read_stderr);
    let stdout_text = out?;

    let status = child
        .wait()
        .await
        .map_err(|e| AdviseError::ClaudeFailed(format!("wait: {e}")))?;
    if !status.success() {
        return Err(AdviseError::ClaudeFailed(format!(
            "claude -p exited {} (stderr: {})",
            status.code().unwrap_or(-1),
            err_buf.trim()
        )));
    }
    Ok(stdout_text.trim().to_string())
}

/// Run `codex exec --json` as a one-shot subprocess, fold the JSONL
/// stream into the final assistant message.
async fn run_codex_advisor(prompt: &str, timeout: Duration) -> Result<String, CodexCallError> {
    // Pre-flight: probe the binary path.
    let bin = codex_bin();
    if !codex_binary_usable(&bin) {
        return Err(CodexCallError::Unavailable(format!(
            "`{bin}` not on PATH or not executable (set CCTEAM_CODEX_BIN to override)"
        )));
    }

    let mut cmd = Command::new(&bin);
    // Same argv shape as `codex_exec::build_exec_argv(None)`: exec
    // --json - (prompt via stdin).
    cmd.arg("exec")
        .arg("--json")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| CodexCallError::Other(format!("spawn {bin}: {e}")))?;

    // Write the prompt to stdin and close so codex starts producing
    // output.
    if let Some(mut stdin) = child.stdin.take() {
        let payload = prompt.as_bytes().to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&payload).await;
            let _ = stdin.shutdown().await;
        });
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CodexCallError::Other("missing stdout pipe".into()))?;

    let stdout_collect = async move {
        let mut s = String::new();
        let mut r = tokio::io::BufReader::new(stdout);
        r.read_to_string(&mut s)
            .await
            .map_err(|e| CodexCallError::Other(format!("read stdout: {e}")))?;
        Ok::<String, CodexCallError>(s)
    };

    // Apply the per-call timeout to the read loop. We don't kill the
    // child explicitly on timeout — letting it drop sends SIGKILL on
    // wait so the codex subprocess stops promptly.
    let stdout_text = match tokio::time::timeout(timeout, stdout_collect).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            // Best-effort kill so the child doesn't outlive the call.
            let _ = child.kill().await;
            return Err(CodexCallError::Timeout);
        }
    };

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => return Err(CodexCallError::Other(format!("wait: {e}"))),
    };
    if !status.success() {
        return Err(CodexCallError::Other(format!(
            "codex exec exited {}",
            status.code().unwrap_or(-1)
        )));
    }

    let body = fold_codex_jsonl_to_text(&stdout_text);
    if body.trim().is_empty() {
        return Err(CodexCallError::Other(
            "codex exec returned empty assistant body".into(),
        ));
    }
    Ok(body)
}

/// Concatenate all `agent_message` text items in the codex `--json`
/// stream. Tolerant of unknown event types (V0.6.3 F144 forward-compat
/// red line) — they're skipped silently.
pub fn fold_codex_jsonl_to_text(stream: &str) -> String {
    let mut out = String::new();
    for line in stream.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // V0.6.5 F152 — extract the text from item.completed +
        // agent_message items. The item may be nested under
        // `item` or carry `type` at the top level; cover both.
        let item_obj = v.get("item").unwrap_or(&v);
        let kind = item_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if kind == "agent_message" {
            if let Some(text) = item_obj.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// Probe whether a candidate binary path is invocable. We treat the
/// `codex` literal as PATH-dependent (assume usable; let spawn error
/// propagate normally), and an absolute path as `is_file() &&
/// has_exec_bits()`.
fn codex_binary_usable(bin: &str) -> bool {
    let p = Path::new(bin);
    if p.is_absolute() {
        if !p.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = match std::fs::metadata(p) {
                Ok(m) => m.permissions(),
                Err(_) => return false,
            };
            return perms.mode() & 0o111 != 0;
        }
        #[cfg(not(unix))]
        {
            return true;
        }
    }
    // Bare name — defer to PATH resolution by `which`-style scan.
    which_on_path(bin).is_some()
}

/// Tiny PATH walker — avoids pulling in the `which` crate as a direct
/// dep. Returns the first matching executable along `$PATH`.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(&candidate) {
                    if m.permissions().mode() & 0o111 != 0 {
                        return Some(candidate);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Third Claude call — verdict synthesiser. Receives the two raw
/// answers plus the Codex status and asks Claude to summarise the
/// agreement / disagreement. Codex-unavailable verdict must include a
/// `Codex unavailable: <reason>` line (red line check).
async fn synthesise_verdict(
    question: &str,
    claude_answer: &str,
    codex_answer: Option<&str>,
    codex_status: &CodexStatus,
) -> Result<String, AdviseError> {
    // Compose synthesiser prompt. If codex is unavailable we don't ask
    // Claude to invent a Codex side — we splice an explicit note into
    // the output so the caller (and downstream skill prompt) sees the
    // unavailability and renders it to the user verbatim.
    let codex_block = match (codex_answer, codex_status) {
        (Some(text), CodexStatus::Ok) => format!("Codex's answer:\n{text}"),
        (_, CodexStatus::Unavailable { reason }) => {
            format!("Codex unavailable: {reason}")
        }
        (_, CodexStatus::Timeout) => "Codex unavailable: timed out before returning".to_string(),
        (_, CodexStatus::Error { reason }) => {
            format!("Codex unavailable: {reason}")
        }
        (None, CodexStatus::Ok) => "Codex unavailable: empty body".to_string(),
    };

    let synth_prompt = format!(
        "Two advisors have answered the following question. Summarise the verdict in 3-5 sentences: \
         do they agree? On what? Where do they diverge? Which approach should the user pick?\n\n\
         Question: {question}\n\n\
         Claude's answer:\n{claude_answer}\n\n\
         {codex_block}\n\n\
         Return only the verdict prose (no preamble)."
    );

    // For unavailable-codex paths we still call Claude (so the prose
    // is consistent with the available-codex path). The Claude call
    // sees the explicit "Codex unavailable" line and is instructed to
    // include it in the verdict.
    let prefix = if !matches!(codex_status, CodexStatus::Ok) {
        format!("Note: {codex_block}\n\n")
    } else {
        String::new()
    };
    let body = run_claude_advisor(&synth_prompt).await?;
    Ok(format!("{prefix}{body}"))
}

/// Coarse 3-bucket classifier — `Agree` / `Partial` / `Disagree` —
/// based on shared n-gram heuristic. Tests pin exact behaviour. For
/// Codex-unavailable paths we return `Unknown` so the JSON consumer
/// can branch.
fn classify_agreement(
    claude_answer: &str,
    codex_answer: Option<&str>,
    codex_status: &CodexStatus,
) -> Agreement {
    let Some(codex) = codex_answer else {
        return Agreement::Unknown;
    };
    if !matches!(codex_status, CodexStatus::Ok) {
        return Agreement::Unknown;
    }
    let a: std::collections::HashSet<String> = claude_answer
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() > 3)
        .collect();
    let b: std::collections::HashSet<String> = codex
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() > 3)
        .collect();
    if a.is_empty() || b.is_empty() {
        return Agreement::Partial;
    }
    let intersect = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    let jaccard = intersect / union;
    if jaccard >= 0.30 {
        Agreement::Agree
    } else if jaccard >= 0.10 {
        Agreement::Partial
    } else {
        Agreement::Disagree
    }
}

// ============================================================
// Budget ledger persistence
// ============================================================

/// Resolve the ledger file path under a ccteam-root directory.
pub fn budget_ledger_path(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join("cost-budget.json")
}

/// Read the ledger, returning an empty one on missing / malformed file
/// (we never want budget reads to fail loud — recovery is automatic
/// on the next call).
pub fn load_budget_ledger(ccteam_root: &Path) -> Result<AdviseBudgetLedger, AdviseError> {
    let path = budget_ledger_path(ccteam_root);
    if !path.exists() {
        return Ok(AdviseBudgetLedger::default());
    }
    let body = std::fs::read_to_string(&path)
        .map_err(|e| AdviseError::Io(format!("read {}: {e}", path.display())))?;
    let ledger: AdviseBudgetLedger = serde_json::from_str(&body).unwrap_or_default();
    Ok(ledger)
}

/// Append one sample, atomic-rename the ledger. We GC samples older
/// than 48h on every write so the file stays bounded even under
/// many-calls-per-day workloads.
pub fn append_budget_sample(
    ccteam_root: &Path,
    vendor: AgentVendor,
    usd: f64,
) -> Result<(), AdviseError> {
    std::fs::create_dir_all(ccteam_root)
        .map_err(|e| AdviseError::Io(format!("mkdir {}: {e}", ccteam_root.display())))?;
    let mut ledger = load_budget_ledger(ccteam_root)?;
    ledger.samples.push(BudgetSample {
        vendor,
        usd,
        ts: Utc::now(),
    });
    // GC: drop > 48h old samples.
    let cutoff = Utc::now() - chrono::Duration::hours(48);
    ledger.samples.retain(|s| s.ts >= cutoff);
    let body = serde_json::to_string_pretty(&ledger)
        .map_err(|e| AdviseError::Io(format!("serialize ledger: {e}")))?;
    let path = budget_ledger_path(ccteam_root);
    // Atomic write: tmp + rename so a crash mid-write can never leave
    // a half-flushed file (the read path silently falls back to an
    // empty ledger on parse failure, but we prefer not to depend on
    // that for the steady-state).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| AdviseError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        AdviseError::Io(format!(
            "rename {} → {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

/// V0.6.6 F173 — typed alias for [`append_budget_sample`] used by
/// non-advise adapter ledger hooks (e.g. `CodexExecAdapter::submit_turn`'s
/// per-turn rollup). The function body is identical; the alias exists
/// so calling code documents intent ("vendor adapter recording a
/// ledger row" vs. "advise call sampling cost"), and so future
/// per-call-site policy (e.g. separate cap for vendor adapter vs.
/// advise) can diverge without a sed-sweep.
pub fn append_budget_ledger_row(
    ccteam_root: &Path,
    vendor: AgentVendor,
    usd: f64,
) -> Result<(), AdviseError> {
    append_budget_sample(ccteam_root, vendor, usd)
}

/// Sum advise spend over the last 24h (regardless of vendor).
pub fn sum_advise_today(ledger: &AdviseBudgetLedger) -> f64 {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    ledger
        .samples
        .iter()
        .filter(|s| s.ts >= cutoff)
        .map(|s| s.usd)
        .sum()
}

/// V0.6.6 F169 — per-vendor 24h spend, surfaced to the IM `@ccteam
/// cost today` admin path so the user sees `claude: $X / codex: $Y`
/// rather than the V0.6.1 bot-count placeholder. Same rolling window
/// the bare [`sum_advise_today`] uses, just filtered by vendor.
pub fn sum_advise_today_by_vendor(ledger: &AdviseBudgetLedger, vendor: AgentVendor) -> f64 {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    ledger
        .samples
        .iter()
        .filter(|s| s.ts >= cutoff && s.vendor == vendor)
        .map(|s| s.usd)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fold_codex_jsonl_extracts_agent_messages_in_order() {
        let stream = r#"{"type":"thread.started","thread_id":"t1"}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"i2","type":"reasoning","text":"hidden"}}
{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"second"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;
        let folded = fold_codex_jsonl_to_text(stream);
        assert_eq!(folded, "first\nsecond");
    }

    #[test]
    fn fold_codex_jsonl_tolerates_unknown_event_types() {
        let stream = r#"{"type":"holographic_drift","payload":{}}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"hello"}}"#;
        let folded = fold_codex_jsonl_to_text(stream);
        assert_eq!(folded, "hello");
    }

    #[test]
    fn fold_codex_jsonl_returns_empty_string_on_empty_stream() {
        assert!(fold_codex_jsonl_to_text("").is_empty());
        assert!(fold_codex_jsonl_to_text("   \n  \n").is_empty());
    }

    #[test]
    fn classify_agreement_high_overlap_returns_agree() {
        let a = "Use approach foobar barbaz quux because performance security maintainability";
        let b = "Foobar approach barbaz performance security maintainability quux";
        let r = classify_agreement(a, Some(b), &CodexStatus::Ok);
        assert_eq!(r, Agreement::Agree);
    }

    #[test]
    fn classify_agreement_no_overlap_returns_disagree() {
        let a = "alpha beta gamma delta epsilon zeta";
        let b = "marigold petunia chrysanthemum hyacinth";
        let r = classify_agreement(a, Some(b), &CodexStatus::Ok);
        assert_eq!(r, Agreement::Disagree);
    }

    #[test]
    fn classify_agreement_codex_unavailable_returns_unknown() {
        let r = classify_agreement(
            "anything",
            None,
            &CodexStatus::Unavailable {
                reason: "no binary".into(),
            },
        );
        assert_eq!(r, Agreement::Unknown);
        // Even if codex returned text, an unavailable status flips to Unknown.
        let r = classify_agreement(
            "anything",
            Some("text"),
            &CodexStatus::Unavailable {
                reason: "no binary".into(),
            },
        );
        assert_eq!(r, Agreement::Unknown);
    }

    #[test]
    fn budget_ledger_roundtrip_through_disk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        append_budget_sample(root, AgentVendor::Claude, 0.01).unwrap();
        append_budget_sample(root, AgentVendor::Codex, 0.02).unwrap();
        let ledger = load_budget_ledger(root).unwrap();
        assert_eq!(ledger.samples.len(), 2);
        let total = sum_advise_today(&ledger);
        assert!((total - 0.03).abs() < 1e-9, "expected 0.03, got {total}");
    }

    #[test]
    fn budget_ledger_gc_drops_stale_samples_on_next_write() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Seed a stale sample (49h old) by writing the ledger directly,
        // then a fresh sample via the API. The append path should GC
        // the stale row.
        let stale_ts = Utc::now() - chrono::Duration::hours(49);
        let ledger = AdviseBudgetLedger {
            samples: vec![BudgetSample {
                vendor: AgentVendor::Claude,
                usd: 99.0,
                ts: stale_ts,
            }],
        };
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            budget_ledger_path(root),
            serde_json::to_string_pretty(&ledger).unwrap(),
        )
        .unwrap();

        append_budget_sample(root, AgentVendor::Claude, 0.005).unwrap();
        let ledger = load_budget_ledger(root).unwrap();
        assert_eq!(ledger.samples.len(), 1, "stale sample must be GC'd");
        assert!(
            (sum_advise_today(&ledger) - 0.005).abs() < 1e-9,
            "24h sum reflects only the fresh sample"
        );
    }

    #[test]
    fn compose_advisor_prompt_inlines_context() {
        let p = compose_advisor_prompt("Should I use X?", Some("Constraint: low latency."));
        assert!(p.contains("Question:\nShould I use X?"));
        assert!(p.contains("Context:\nConstraint: low latency."));
    }

    #[test]
    fn compose_advisor_prompt_skips_empty_context() {
        let p = compose_advisor_prompt("Q?", Some("   "));
        assert!(!p.contains("Context:"));
        assert!(p.contains("Q?"));
    }

    #[test]
    fn codex_binary_usable_returns_false_for_nonexistent_path() {
        assert!(!codex_binary_usable("/tmp/does-not-exist-deadbeef"));
    }

    #[test]
    fn codex_binary_usable_returns_false_for_nonexec_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("notexec");
        std::fs::write(&path, b"hi").unwrap();
        assert!(!codex_binary_usable(path.to_str().unwrap()));
    }

    #[test]
    fn codex_binary_usable_returns_true_for_bin_true() {
        // `/bin/true` is part of coreutils on every linux test runner.
        if Path::new("/bin/true").exists() {
            assert!(codex_binary_usable("/bin/true"));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn advise_vote_rejects_empty_question() {
        let tmp = TempDir::new().unwrap();
        let err = advise_vote(tmp.path(), "  ", None, None, None)
            .await
            .unwrap_err();
        match err {
            AdviseError::InvalidInput(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn advise_vote_rejects_when_budget_already_exceeded() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Pre-seed ledger past the cap.
        for _ in 0..10 {
            append_budget_sample(root, AgentVendor::Claude, 0.10).unwrap();
        }
        let err = advise_vote(root, "Question?", None, None, Some(0.50))
            .await
            .unwrap_err();
        match err {
            AdviseError::BudgetExceeded { spent_usd, cap_usd } => {
                // 10 × 0.10 sums to ~0.99999 due to FP — assert ≥ cap
                // (the gate condition) rather than ≥ 1.0 nominally.
                assert!(spent_usd >= cap_usd, "spent {spent_usd} < cap {cap_usd}");
                assert!((cap_usd - 0.50).abs() < 1e-9);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
