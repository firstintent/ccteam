//! ccteam-flow — the dynamic-workflow runner.
//!
//! A workflow is a plain JavaScript file. Its control flow is deterministic
//! and written by a human (or by an agent, once); its *work* is done by
//! ccteam sessions. Every `agent()` call in the script is an A2A hire on any
//! harness — claude, codex, grok, opencode, kimi, pi, dsh — and therefore a
//! real ledger session with a sid, a transcript and a cost, not an anonymous
//! subprocess.
//!
//! ```text
//!   script.js ──► engine (QuickJS)  ──► scheduler ──► FlowClient ──► daemon
//!                      │                    │              │
//!                   prelude              brakes,        hire / wait /
//!                (traps + hooks)      pools, ramp        stop / usage
//!                      │
//!                   journal.jsonl  ◄── every call, twice (dispatch, done)
//! ```
//!
//! What this crate deliberately does NOT do: talk to the network, spawn a
//! process, read `$HOME`, print anything, or call an LLM. The single door out
//! is [`FlowClient`]; the transport that implements it, and the CLI that
//! drives a run, live outside.
//!
//! # Contract
//!
//! * a script opens with `export const meta = {name, description, phases?}`,
//!   a pure literal, extracted before anything runs;
//! * `Date.now()`, `Math.random()`, argless `new Date()` and
//!   `Intl.DateTimeFormat` throw — they would make a resume produce a
//!   different program;
//! * `agent()` never throws for a worker's failure (it resolves `null`) but
//!   always throws for the author's (unknown option, tripped brake);
//! * `parallel()` is a barrier, `pipeline()` is not;
//! * a run is resumable from its journal, and the first call whose inputs
//!   changed invalidates every cached answer after it.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), ccteam_flow::FlowError> {
//! use std::sync::Arc;
//! use ccteam_flow::{run_workflow, FakeClient, RunConfig, ScriptSource};
//!
//! let client = Arc::new(FakeClient::new());
//! let report = run_workflow(
//!     ScriptSource::path("review.js"),
//!     RunConfig::new("/tmp/run-1", client),
//! )
//! .await?;
//! println!("{} agents, {:.2} USD", report.totals.agents, report.totals.cost_usd);
//! # Ok(())
//! # }
//! ```

mod engine;
mod journal;

#[cfg(test)]
mod tests;

pub mod client;
pub mod error;
pub mod meta;
pub mod progress;
pub mod run;
pub mod scheduler;
pub mod schema;

#[cfg(feature = "test-util")]
pub mod fake;

pub use client::{AgentOutcome, ClientError, FlowClient, HireSpec, Hired};
pub use error::FlowError;
pub use journal::{call_key, CacheReport, JournalEntry};
pub use meta::{assert_deterministic, extract_meta, PhaseMeta, WorkflowMeta, DETERMINISM_HINT};
pub use progress::{ProgressCallback, ProgressEvent};
pub use run::{run_workflow, AgentRecord, RunConfig, RunReport, RunTotals, ScriptSource};
pub use scheduler::{Brakes, RunControl, SchedulerConfig, VendorPools, HARD_AGENT_CAP};
pub use schema::SCHEMA_RETRY_PROMPT;

#[cfg(feature = "test-util")]
pub use fake::{FakeCall, FakeClient, FakeReply};
