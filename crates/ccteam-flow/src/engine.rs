//! The embedded JavaScript engine.
//!
//! QuickJS via `rquickjs`, chosen over a pure-Rust interpreter for one
//! reason: the script API is *async by nature*. Every `agent()` is a real
//! session turn that takes minutes, and `parallel()` / `pipeline()` only mean
//! anything if dozens of those are genuinely in flight at once. rquickjs
//! exposes host functions that return Rust futures and drives them on the
//! same task that drives the JS job queue, so `Promise.all([...])` over 32
//! agents is 32 concurrent client calls — not 32 sequential ones.
//!
//! The realm is deliberately impoverished. No module loader, no dynamic
//! library loading, no `chrono` bindings; QuickJS itself has no filesystem,
//! network or process surface. The only host functions in the realm are the
//! ones this module installs, and the prelude deletes even those from the
//! global object once it has closed over them.

use crate::error::FlowError;
use rquickjs::prelude::Async;
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Function, Promise};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// The script's world, injected before the prelude runs.
const PRELUDE: &str = include_str!("prelude.js");

/// Ceiling on the JS heap. A workflow accumulates every agent's reply in
/// script memory; 256 MB is far above any sane fan-out and far below "the
/// daemon died".
const JS_MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// What the host must provide for the script globals to work.
///
/// `agent` and `usage` return a JSON *envelope* string rather than a value:
/// it keeps every host reply on one simple wire shape, lets Rust decide which
/// failures are the script's fault (`{k:"throw"}`) versus the worker's
/// (`{k:"ok",v:null}`), and avoids threading QuickJS lifetimes through the
/// scheduler.
#[async_trait::async_trait]
pub(crate) trait Host: Send + Sync + 'static {
    async fn agent(&self, task: String, opts_json: String) -> String;
    async fn usage(&self) -> String;
    fn phase(&self, title: String);
    fn log(&self, message: String);
    fn spent(&self) -> f64;
}

/// Everything the engine needs for one run.
pub(crate) struct EngineInput {
    /// Raw script text, exactly as the author wrote it.
    pub source: String,
    /// `args` serialised, or `None` for the official `undefined`.
    pub args_json: Option<String>,
    /// `budget.total`.
    pub budget_total: Option<f64>,
    /// The message every determinism trap throws.
    pub blocked_message: String,
    /// Last-resort watchdog: if the script spins without ever awaiting, the
    /// engine is interrupted after this long. NOT the wall-clock brake (a
    /// brake refuses new agents and lets the script keep running); this only
    /// exists so `while (true) {}` cannot pin a thread forever.
    pub watchdog: Option<Duration>,
}

/// What the script produced.
pub(crate) struct ScriptResult {
    /// The script's return value, `Null` when it returned nothing.
    pub returned: Value,
    /// Set when the script threw. The run still reports whatever agents ran.
    pub error: Option<String>,
}

/// Strip the leading `export ` from top-level declarations.
///
/// `export const meta = {...}` is the contract's required opening line, but
/// the body is executed as a plain async function, not a module (that is what
/// makes top-level `await` and `return` work). Only line-initial declarations
/// are rewritten, so the word `export` inside an expression is left alone.
fn strip_top_level_exports(source: &str) -> String {
    const DECLS: [&str; 5] = ["const ", "let ", "var ", "function ", "class "];
    source
        .lines()
        .map(|line| {
            let indent_len = line.len() - line.trim_start().len();
            let (indent, rest) = line.split_at(indent_len);
            let Some(after) = rest.strip_prefix("export ") else {
                return line.to_string();
            };
            let after_trimmed = after.trim_start();
            let is_decl = DECLS.iter().any(|d| after_trimmed.starts_with(d))
                || after_trimmed.starts_with("async function ");
            if is_decl {
                format!("{indent}{after_trimmed}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap the body so top-level `await`/`return` work and so *no* JS exception
/// crosses the FFI boundary: success and failure both come back as a JSON
/// string, which keeps error reporting readable instead of `Error::Exception`.
fn wrap_body(body: &str) -> String {
    format!(
        r#"(function () {{
  async function __wf() {{
{body}
  }}
  return __wf().then(
    function (v) {{
      try {{
        return JSON.stringify({{ k: 'ok', v: v === undefined ? null : v }});
      }} catch (e) {{
        return JSON.stringify({{ k: 'err', m: 'workflow return value is not JSON: ' + String(e) }});
      }}
    }},
    function (e) {{
      // QuickJS `e.stack` carries only the frames, not the message, so the
      // message has to come from String(e) or a thrown non-error is reported
      // as an empty stack trace.
      var msg;
      try {{ msg = String(e); }} catch (x) {{ msg = 'workflow script threw an unprintable value'; }}
      try {{ if (e && e.stack) msg = msg + '\n' + String(e.stack); }} catch (x) {{}}
      return JSON.stringify({{ k: 'err', m: msg }});
    }}
  );
}})()"#
    )
}

/// Render a `rquickjs` error, pulling the pending JS exception out of the
/// context so the caller sees `SyntaxError: ...` instead of `Exception`.
fn js_error(ctx: &Ctx<'_>, err: rquickjs::Error) -> String {
    match err {
        rquickjs::Error::Exception => {
            let caught = ctx.catch();
            match caught.as_exception() {
                Some(e) => e.to_string(),
                None => format!("{caught:?}"),
            }
        }
        other => other.to_string(),
    }
}

/// Execute one workflow script to completion.
pub(crate) async fn execute(
    input: EngineInput,
    host: Arc<dyn Host>,
) -> Result<ScriptResult, FlowError> {
    let runtime = AsyncRuntime::new().map_err(|e| FlowError::Engine(e.to_string()))?;
    runtime.set_memory_limit(JS_MEMORY_LIMIT).await;
    if let Some(limit) = input.watchdog {
        // std::time on purpose: this is a real-world hang guard, and a
        // spinning script never yields to the async runtime, so a virtual
        // clock would never advance to fire it.
        let deadline = std::time::Instant::now() + limit;
        runtime
            .set_interrupt_handler(Some(Box::new(move || {
                std::time::Instant::now() >= deadline
            })))
            .await;
    }
    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|e| FlowError::Engine(e.to_string()))?;

    let body = wrap_body(&strip_top_level_exports(&input.source));

    let outcome = context
        .async_with(
            async move |ctx: Ctx<'_>| -> Result<ScriptResult, FlowError> {
                install_host(&ctx, host, &input)?;

                ctx.eval::<(), _>(PRELUDE)
                    .map_err(|e| FlowError::Engine(format!("prelude: {}", js_error(&ctx, e))))?;

                let promise: Promise = ctx.eval(body).map_err(|e| {
                    FlowError::Engine(format!(
                        "workflow script did not compile: {}",
                        js_error(&ctx, e)
                    ))
                })?;
                let raw: String = promise
                    .into_future::<String>()
                    .await
                    .map_err(|e| FlowError::Engine(js_error(&ctx, e)))?;

                let envelope: Value = serde_json::from_str(&raw)
                    .map_err(|e| FlowError::Engine(format!("malformed script envelope: {e}")))?;
                match envelope.get("k").and_then(Value::as_str) {
                    Some("ok") => Ok(ScriptResult {
                        returned: envelope.get("v").cloned().unwrap_or(Value::Null),
                        error: None,
                    }),
                    _ => Ok(ScriptResult {
                        returned: Value::Null,
                        error: Some(
                            envelope
                                .get("m")
                                .and_then(Value::as_str)
                                .unwrap_or("workflow script failed")
                                .to_string(),
                        ),
                    }),
                }
            },
        )
        .await?;

    // Let any host future the script abandoned (an un-awaited agent()) finish
    // rather than being dropped mid-flight.
    runtime.idle().await;
    Ok(outcome)
}

/// Bind the `__ccteam_*` primitives the prelude closes over.
fn install_host(ctx: &Ctx<'_>, host: Arc<dyn Host>, input: &EngineInput) -> Result<(), FlowError> {
    let globals = ctx.globals();
    let bind = |e: rquickjs::Error| FlowError::Engine(format!("binding host: {e}"));

    let agent_host = Arc::clone(&host);
    let agent = Function::new(
        ctx.clone(),
        Async(move |task: String, opts_json: String| {
            let host = Arc::clone(&agent_host);
            async move { Ok::<String, rquickjs::Error>(host.agent(task, opts_json).await) }
        }),
    )
    .map_err(bind)?;
    globals.set("__ccteam_agent", agent).map_err(bind)?;

    let usage_host = Arc::clone(&host);
    let usage = Function::new(
        ctx.clone(),
        Async(move || {
            let host = Arc::clone(&usage_host);
            async move { Ok::<String, rquickjs::Error>(host.usage().await) }
        }),
    )
    .map_err(bind)?;
    globals.set("__ccteam_usage", usage).map_err(bind)?;

    let phase_host = Arc::clone(&host);
    globals
        .set(
            "__ccteam_phase",
            Function::new(ctx.clone(), move |title: String| phase_host.phase(title))
                .map_err(bind)?,
        )
        .map_err(bind)?;

    let log_host = Arc::clone(&host);
    globals
        .set(
            "__ccteam_log",
            Function::new(ctx.clone(), move |message: String| log_host.log(message))
                .map_err(bind)?,
        )
        .map_err(bind)?;

    let spent_host = Arc::clone(&host);
    globals
        .set(
            "__ccteam_spent",
            Function::new(ctx.clone(), move || spent_host.spent()).map_err(bind)?,
        )
        .map_err(bind)?;

    globals
        .set("__ccteam_budget_total", input.budget_total)
        .map_err(bind)?;
    globals
        .set("__ccteam_blocked_msg", input.blocked_message.clone())
        .map_err(bind)?;
    if let Some(args) = &input.args_json {
        globals
            .set("__ccteam_args_json", args.clone())
            .map_err(bind)?;
    }
    Ok(())
}
