//! Errors the runner surfaces to its *caller* (the CLI in F0b, a daemon route
//! later).
//!
//! Deliberately small. Anything that happens to a *worker* is not an error
//! here: a failed hire, a refusing guardrail or a broken worker resolves the
//! script's `agent()` call to `null` (see `client::AgentOutcome`), because a
//! workflow author must be able to write `.filter(Boolean)` instead of
//! wrapping every call in try/catch. Only things that make the *run itself*
//! meaningless — an unreadable script, a malformed `meta`, an engine failure —
//! travel out as a `FlowError`.

use std::path::PathBuf;

/// A failure that aborts the whole run.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The script file could not be read.
    #[error("cannot read workflow script {path}: {source}")]
    ReadScript {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `export const meta = {...}` is missing, not a pure literal, or is
    /// missing a required field.
    #[error("{0}")]
    Meta(String),

    /// The script references an API that would break resume (`Date.now()`,
    /// `Math.random()`, argless `new Date()`, `Intl` date formatting).
    #[error("{0}")]
    Determinism(String),

    /// The JS engine could not be created, or the script failed to compile.
    #[error("workflow engine: {0}")]
    Engine(String),

    /// The run directory could not be prepared. Journal *writes* never fail a
    /// run (they warn), but a run directory we cannot create at all means
    /// resume is silently impossible, which is worse than a loud failure.
    #[error("cannot prepare run directory {path}: {source}")]
    RunDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
