//! Progress events.
//!
//! The library never prints. It hands structured events to a callback and the
//! caller decides what a terminal, an SSE stream or a `progress.jsonl` writer
//! should do with them. That keeps the runner usable from the CLI, the daemon
//! and a test harness without three copies of the same rendering logic.

use crate::client::AgentOutcome;
use ccteam_harness::AgentVendor;
use std::sync::Arc;

/// One thing that happened during a run.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressEvent {
    RunStarted {
        name: String,
        description: String,
        /// Phase titles declared in `meta.phases`, in order. A renderer can
        /// draw the whole outline before the first agent starts.
        phases: Vec<String>,
    },
    PhaseStarted {
        title: String,
    },
    AgentStarted {
        seq: usize,
        label: String,
        vendor: Option<AgentVendor>,
        /// True when the journal answered instead of the client — no spend,
        /// no session.
        cached: bool,
    },
    AgentFinished {
        seq: usize,
        label: String,
        /// The turn's result. `None` means the call produced nothing at all
        /// (hire refused, transport failure, brake) and the script saw `null`.
        outcome: Option<AgentOutcome>,
        cost_usd: f64,
    },
    Log {
        message: String,
        phase: Option<String>,
    },
    /// A brake tripped. Emitted once, at the moment admission first refuses.
    /// In-flight agents keep running — a brake stops *new* work, it never
    /// kills a live worker.
    BrakeTripped {
        reason: String,
    },
    RunFinished {
        agents: usize,
        cost_usd: f64,
        /// False when the script threw, or a brake ended the run early.
        ok: bool,
    },
}

/// Where events go. `Arc<dyn Fn>` rather than a channel so a caller that just
/// wants to print does not have to run a receiver task.
pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync>;
