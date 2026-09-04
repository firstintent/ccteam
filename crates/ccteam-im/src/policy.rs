//! The programmable pre-flight policy hook for `agent` (Card H).
//!
//! WHY a hook at all: the delegation guardrails ccteam ships (depth / fan-out /
//! ceiling / cycle / budget) are the ones every team needs and nobody can
//! express differently. Everything past that — "don't hire claude while the 5h
//! window is over 80%", "this project only hires codex for reviews", "no
//! delegation after midnight" — is a POLICY, and a policy belongs to whoever
//! runs the team, not to the engine. Rather than grow a config DSL that would
//! keep chasing the next rule, the engine hands the caller's own facts to a
//! script the user writes, and honours its verdict. That also keeps the engine
//! LLM-free red line intact: the decision is a user program, never a model.
//!
//! The shape is deliberately the one users already know — git hooks (a file at
//! a known path, executable, run as a subprocess) plus the Claude Code
//! PreToolUse verdict dialect (exit 0 = allow, exit 2 = deny with the reason on
//! stderr). Nothing to register, nothing to restart: the file is resolved and
//! executed on every call, so an edit takes effect on the next `agent`.
//!
//! Resolution (REPLACE, never merge — a project that states a policy states all
//! of it, exactly like `.ccteam/routing.md`):
//!
//! 1. `<project>/.ccteam/hooks/pre-agent`
//! 2. `<ccteam_home>/hooks/pre-agent`
//! 3. neither exists → allow (an unconfigured daemon behaves exactly as before)
//!
//! Fail-closed asymmetry: a hook that DENIES is a policy answer, and a hook
//! that FAULTS (timeout, exit 7, not executable) is refused too — a guardrail
//! that silently opens when its script breaks is not a guardrail. The two are
//! never spelled the same way, because "your policy said no" and "your policy
//! is broken" need different humans to act.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::delegation::DenyReason;

/// File name of the pre-agent hook, in both the project and the global rung.
pub const PRE_AGENT_HOOK_FILE: &str = "pre-agent";

/// Env var naming the hook that is running, so one script can serve several
/// hook points later without guessing from `$0`.
pub const HOOK_ENV_VAR: &str = "CCTEAM_HOOK";

/// Value of [`HOOK_ENV_VAR`] for this hook point.
pub const HOOK_ENV_VALUE: &str = "pre-agent";

/// Hard wall-clock budget for one hook run. A delegation is an interactive act
/// (a caller is blocked on it), so a policy that cannot answer in three seconds
/// is a broken policy, not a slow one.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(3);

/// Grace period for draining the hook's stderr AFTER it exited. Its own pipe
/// closes at exit; this only bounds the pathological case of a grandchild
/// holding the write end open.
const STDERR_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Cap on the deny reason relayed to the caller. It lands in an agent's
/// context, where every byte is paid for; a policy that needs an essay should
/// point at a document.
pub const DENY_REASON_MAX_BYTES: usize = 2000;

/// How much of the task text the payload carries. Enough for a policy to
/// recognise WHAT is being delegated, far short of shipping the whole prompt
/// through a user script on every call.
pub const TASK_HEAD_MAX_CHARS: usize = 500;

/// The verdict of one pre-agent hook run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// No hook, or the hook exited 0.
    Allow,
    /// The hook exited 2. `reason` is its stderr, verbatim (capped).
    Deny {
        /// The script's own words for the caller.
        reason: String,
    },
    /// The hook could not deliver a verdict (spawn failure, timeout, any other
    /// exit code). Fail-closed, but never dressed up as a policy answer.
    ScriptError {
        /// The script that faulted — the one fact its owner needs.
        path: PathBuf,
        /// The failure mode (exit code / timeout / io error).
        detail: String,
    },
}

impl PolicyOutcome {
    /// The guardrail reason this outcome records, `None` when allowed.
    pub fn deny_reason(&self) -> Option<DenyReason> {
        match self {
            PolicyOutcome::Allow => None,
            PolicyOutcome::Deny { .. } => Some(DenyReason::Policy),
            PolicyOutcome::ScriptError { .. } => Some(DenyReason::PolicyScriptError),
        }
    }

    /// The caller-facing refusal, without the tool prefix (the call site owns
    /// that, so `agent` errors all read alike). `None` when allowed.
    pub fn refusal_text(&self) -> Option<String> {
        match self {
            PolicyOutcome::Allow => None,
            PolicyOutcome::Deny { reason } if reason.trim().is_empty() => Some(format!(
                "delegation denied by policy: the {PRE_AGENT_HOOK_FILE} hook exited 2 (deny) without writing a reason to stderr"
            )),
            // Verbatim: the script's sentence is the whole point — ccteam
            // paraphrasing it would put words in the policy owner's mouth.
            PolicyOutcome::Deny { reason } => Some(format!("delegation denied by policy: {reason}")),
            PolicyOutcome::ScriptError { path, detail } => Some(format!(
                "delegation refused: {} — {} {detail}",
                DenyReason::PolicyScriptError.tag(),
                path.display()
            )),
        }
    }
}

/// One resolved hook: which file to run, and from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookScript {
    /// Absolute path of the executable to run.
    pub path: PathBuf,
    /// Working directory for the run — the project root, or the ccteam home
    /// for the global rung, so a script's relative paths mean what its author
    /// meant when writing it next to its own files.
    pub cwd: PathBuf,
}

/// Everything one `agent` call knows about itself, in the order a policy reads
/// it. Every field is best-effort: an unavailable fact is omitted from the
/// payload, never a reason to block (an observability gap must not become an
/// outage).
#[derive(Debug, Default, Clone)]
pub struct PolicyFacts {
    /// `"hire"` (no `sid`) or `"dispatch"` (an existing `sid`).
    pub kind: &'static str,
    /// Who is delegating.
    pub caller: CallerFacts,
    /// What was asked for.
    pub request: RequestFacts,
    /// Per-vendor account usage, the same map `status{detail:"usage"}` renders.
    /// Handed IN so a hook needs no token and no round trip into the daemon.
    pub usage: Option<Value>,
    /// Cheap running totals a policy would otherwise have to count itself.
    pub counts: CountFacts,
}

/// The delegating session.
#[derive(Debug, Default, Clone)]
pub struct CallerFacts {
    /// The caller's own sid (empty for a human/admin caller).
    pub sid: String,
    /// Its harness.
    pub vendor: String,
    /// Its delegation depth (root = 0).
    pub depth: Option<u32>,
    /// Its project slug.
    pub project: String,
    /// How full its context window is, when a turn has reported it.
    pub context_pct: Option<u64>,
}

/// The delegation being requested.
#[derive(Debug, Default, Clone)]
pub struct RequestFacts {
    /// Harness to hire (hire), or the target's harness (dispatch).
    pub vendor: String,
    /// Requested model, verbatim.
    pub model: String,
    /// Requested role.
    pub role: String,
    /// Target sid on a dispatch.
    pub sid: String,
    /// Inline wait seconds (0 = fire and notify).
    pub wait: u64,
    /// The full task text — only its head reaches the payload.
    pub task: String,
    /// Ledger title.
    pub title: String,
}

/// Running totals at the moment of the call.
#[derive(Debug, Default, Clone)]
pub struct CountFacts {
    /// The caller's active direct children.
    pub children: Option<u32>,
    /// Active delegated sessions in the target project.
    pub delegated: Option<u32>,
    /// The project's trailing-24h cost.
    pub cost_24h_usd: Option<f64>,
}

impl PolicyFacts {
    /// Render the stdin payload: compact JSON, one line, empty facts omitted.
    pub fn payload(&self) -> Value {
        let mut body = Map::new();
        body.insert("kind".into(), json!(self.kind));

        let mut caller = Map::new();
        insert_str(&mut caller, "sid", &self.caller.sid);
        insert_str(&mut caller, "vendor", &self.caller.vendor);
        if let Some(depth) = self.caller.depth {
            caller.insert("depth".into(), json!(depth));
        }
        insert_str(&mut caller, "project", &self.caller.project);
        if let Some(pct) = self.caller.context_pct {
            caller.insert("context_pct".into(), json!(pct));
        }
        insert_section(&mut body, "caller", caller);

        let mut request = Map::new();
        insert_str(&mut request, "vendor", &self.request.vendor);
        insert_str(&mut request, "model", &self.request.model);
        insert_str(&mut request, "role", &self.request.role);
        insert_str(&mut request, "sid", &self.request.sid);
        // `wait` is always present: 0 is a fact (fire-and-notify), not an
        // absence, and a policy that keys on it must not have to guess.
        request.insert("wait".into(), json!(self.request.wait));
        if !self.request.task.is_empty() {
            request.insert("task_head".into(), json!(task_head(&self.request.task)));
            request.insert(
                "task_chars".into(),
                json!(self.request.task.chars().count()),
            );
        }
        insert_str(&mut request, "title", &self.request.title);
        insert_section(&mut body, "request", request);

        if let Some(usage) = self
            .usage
            .as_ref()
            .filter(|usage| !matches!(usage, Value::Object(map) if map.is_empty()))
        {
            body.insert("usage".into(), usage.clone());
        }

        let mut counts = Map::new();
        if let Some(children) = self.counts.children {
            counts.insert("children".into(), json!(children));
        }
        if let Some(delegated) = self.counts.delegated {
            counts.insert("delegated".into(), json!(delegated));
        }
        if let Some(cost) = self.counts.cost_24h_usd {
            // Money, rounded to a hundredth of a cent: past that the digits are
            // float noise in a payload a shell script has to read.
            counts.insert(
                "cost_24h_usd".into(),
                json!((cost * 10_000.0).round() / 10_000.0),
            );
        }
        insert_section(&mut body, "counts", counts);

        Value::Object(body)
    }
}

fn insert_str(map: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        map.insert(key.to_string(), json!(value));
    }
}

/// An all-empty section says nothing; omitting it keeps the line short and
/// makes `if payload.caller.sid` in a script the only check that matters.
fn insert_section(body: &mut Map<String, Value>, key: &str, section: Map<String, Value>) {
    if !section.is_empty() {
        body.insert(key.to_string(), Value::Object(section));
    }
}

/// The first [`TASK_HEAD_MAX_CHARS`] characters of the task (char-wise, so a
/// multi-byte prompt is never cut mid-character).
pub fn task_head(task: &str) -> String {
    task.chars().take(TASK_HEAD_MAX_CHARS).collect()
}

/// What one `agent` call hands the policy layer.
#[derive(Debug, Clone, Copy)]
pub struct PolicyCtx<'a> {
    /// The TARGET project's working tree, when it is local to this daemon.
    /// `None` for a project bound to a satellite host: its `.ccteam/hooks/` is
    /// on the other machine and unreadable here, and running the local
    /// daemon's copy of a foreign path would be a lie about whose policy ran.
    pub project_dir: Option<&'a Path>,
    /// `~/.ccteam` as the RUNNING daemon resolved it. Passed in rather than
    /// re-derived from the environment because a daemon may run with `--home`
    /// / `CCTEAM_HOME` pointing elsewhere, and the hook must come from the
    /// same home as everything else it reads.
    pub ccteam_home: &'a Path,
    /// The facts the hook is handed on stdin.
    pub payload: &'a Value,
}

/// Run the pre-agent policy for one call.
pub async fn check(ctx: PolicyCtx<'_>) -> PolicyOutcome {
    check_in(ctx.project_dir, ctx.ccteam_home, ctx.payload).await
}

/// [`check`] with both roots given explicitly — the injection seam tests use so
/// they never read the developer's real `$HOME` / `$CCTEAM_HOME`.
pub async fn check_in(
    project_dir: Option<&Path>,
    ccteam_home: &Path,
    payload: &Value,
) -> PolicyOutcome {
    let Some(script) = resolve_hook(project_dir, ccteam_home) else {
        return PolicyOutcome::Allow;
    };
    run_hook(&script, payload).await
}

/// Resolve which hook file governs this call, if any. Pure: a stat per rung,
/// no caching — the file is the registration, so editing it IS the deploy.
pub fn resolve_hook(project_dir: Option<&Path>, ccteam_home: &Path) -> Option<HookScript> {
    if let Some(dir) = project_dir {
        let path = dir.join(".ccteam").join("hooks").join(PRE_AGENT_HOOK_FILE);
        if path.exists() {
            return Some(HookScript {
                path,
                cwd: dir.to_path_buf(),
            });
        }
    }
    let path = ccteam_home.join("hooks").join(PRE_AGENT_HOOK_FILE);
    path.exists().then(|| HookScript {
        path,
        cwd: ccteam_home.to_path_buf(),
    })
}

/// Execute one resolved hook with `payload` on stdin and read its verdict.
/// Split from [`check_in`] so a caller that already knows a hook exists (the
/// `agent` gate) can resolve once, gather the expensive facts only then, and
/// run — an unconfigured daemon never pays for the payload it would not use.
pub async fn run_hook(script: &HookScript, payload: &Value) -> PolicyOutcome {
    run_hook_with(script, payload, HOOK_TIMEOUT).await
}

/// [`run_hook`] with an explicit timeout, so tests can exercise the timeout
/// path in milliseconds instead of paying the production budget.
async fn run_hook_with(script: &HookScript, payload: &Value, timeout: Duration) -> PolicyOutcome {
    let fault = |detail: String| PolicyOutcome::ScriptError {
        path: script.path.clone(),
        detail,
    };
    let mut command = Command::new(&script.path);
    command
        .current_dir(&script.cwd)
        .env(HOOK_ENV_VAR, HOOK_ENV_VALUE)
        .stdin(Stdio::piped())
        // The verdict protocol is exit code + stderr only. stdout is discarded
        // rather than inherited so a chatty hook can neither pollute the
        // daemon's own output nor wedge on a pipe nobody drains.
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // A hook that outlives its verdict is not the daemon's tenant.
        .kill_on_drop(true);
    // Give the hook its own process group so a timeout can kill the WHOLE
    // tree: a policy that spawns helpers (curl, jq pipelines, background
    // probes) must not leak them past its verdict. setpgid is
    // async-signal-safe, as pre_exec requires.
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        // Covers the "present but not executable" case (EACCES) and a hook
        // whose shebang names a missing interpreter (ENOENT).
        Err(error) => return fault(format!("cannot run it: {error}")),
    };

    // Feed stdin from its own task: a hook that never reads its input must not
    // be able to block the daemon on a full pipe.
    if let Some(mut stdin) = child.stdin.take() {
        let mut line = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        line.push('\n');
        tokio::spawn(async move {
            let _ = stdin.write_all(line.as_bytes()).await;
            // EOF, so a hook that reads to end (jq, cat) returns.
            let _ = stdin.shutdown().await;
        });
    }
    // Drain stderr CONCURRENTLY: a hook that writes more than a pipe buffer
    // before exiting would otherwise deadlock into a spurious timeout.
    let stderr = child.stderr.take().map(|mut pipe| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        })
    });

    let waited = tokio::time::timeout(timeout, child.wait()).await;
    let status = match waited {
        Err(_elapsed) => {
            // Kill the whole process group and reap: the caller is blocked,
            // and a hung policy must not leave the hook OR its descendants
            // behind it.
            kill_hook_tree(&mut child);
            let _ = child.wait().await;
            if let Some(handle) = stderr {
                let _ = join_drain(handle).await;
            }
            return fault(format!(
                "timed out after {:.1}s (killed)",
                timeout.as_secs_f32()
            ));
        }
        Ok(Err(error)) => {
            // Exotic (ECHILD-class) wait failure: same discipline as the
            // timeout — no process left behind, no drain task left unjoined.
            kill_hook_tree(&mut child);
            if let Some(handle) = stderr {
                let _ = join_drain(handle).await;
            }
            return fault(format!("could not be waited on: {error}"));
        }
        Ok(Ok(status)) => status,
    };
    let captured = match stderr {
        Some(handle) => join_drain(handle).await,
        None => Vec::new(),
    };

    match status.code() {
        Some(0) => PolicyOutcome::Allow,
        Some(2) => PolicyOutcome::Deny {
            reason: truncate_utf8(&captured, DENY_REASON_MAX_BYTES),
        },
        // The dialect has exactly two verdicts. Anything else is a script that
        // crashed, mis-set `set -e`, or invented a code — all faults, and all
        // fail-closed, because "unknown" must never mean "go ahead".
        Some(code) => fault(format!(
            "exited {code} (only 0 = allow and 2 = deny are verdicts)"
        )),
        None => fault(format!("ended without an exit code ({status})")),
    }
}

/// Kill the hook's whole process group (unix; elsewhere: the direct child).
/// The group was created in `pre_exec`, so `-pid` addresses the hook and every
/// helper it spawned — a timed-out policy must not leak grandchildren.
fn kill_hook_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // A dead group answers ESRCH, which is fine; SIGKILL because the
        // 3s budget was the grace.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        return;
    }
    let _ = child.start_kill();
}

/// Join the stderr drain, bounded by the grace. On expiry the task is
/// ABORTED, never detached: dropping a `JoinHandle` leaves the task running,
/// so a stray writer that escaped the group kill (a double-forked setsid
/// descendant holding the pipe) would otherwise pin a read task and an fd
/// forever. Used by BOTH exit paths — the hazard is the pipe, not the timeout.
async fn join_drain(mut handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    match tokio::time::timeout(STDERR_DRAIN_GRACE, &mut handle).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_join_error)) => Vec::new(),
        Err(_elapsed) => {
            handle.abort();
            Vec::new()
        }
    }
}

/// Lossy-decode and cut at a char boundary at or below `max_bytes`, so a
/// truncated reason is still valid UTF-8 for every consumer downstream.
fn truncate_utf8(bytes: &[u8], max_bytes: usize) -> String {
    // Verbatim contract: the reason is the script's stderr byte-for-byte
    // (lossy-decoded), subject ONLY to the byte cap — no trimming; a policy
    // author's formatting is theirs.
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= max_bytes {
        return text.into_owned();
    }
    let end = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    text[..end].to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable fixture hook at `path` (parents created).
    fn write_hook(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn project_hook(project: &Path) -> PathBuf {
        project
            .join(".ccteam")
            .join("hooks")
            .join(PRE_AGENT_HOOK_FILE)
    }

    fn global_hook(home: &Path) -> PathBuf {
        home.join("hooks").join(PRE_AGENT_HOOK_FILE)
    }

    fn facts() -> Value {
        json!({"kind": "hire"})
    }

    /// The zero-config case: no hook on either rung is an allow, and costs one
    /// stat per rung (behaviour before Card H, unchanged).
    #[tokio::test]
    async fn no_hook_anywhere_allows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(resolve_hook(Some(&project), &home), None);
        assert_eq!(
            check_in(Some(&project), &home, &facts()).await,
            PolicyOutcome::Allow
        );
    }

    /// exit 2 = deny, and the script's stderr is the reason verbatim.
    #[tokio::test]
    async fn exit_two_denies_with_stderr_verbatim() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        write_hook(
            &project_hook(&project),
            "#!/bin/sh\necho 'quota low, use codex' >&2\nexit 2\n",
        );
        let outcome = check_in(Some(&project), &tmp.path().join("home"), &facts()).await;
        assert_eq!(
            outcome,
            PolicyOutcome::Deny {
                // echo's trailing newline survives: stderr is VERBATIM.
                reason: "quota low, use codex\n".to_string()
            }
        );
        assert_eq!(outcome.deny_reason(), Some(DenyReason::Policy));
        assert_eq!(
            outcome.refusal_text().unwrap(),
            "delegation denied by policy: quota low, use codex\n"
        );
    }

    /// Whitespace is part of the verbatim contract: leading/trailing bytes
    /// survive, because trimming a policy author's formatting is editing it.
    #[tokio::test]
    async fn deny_reasons_keep_their_edges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        write_hook(
            &project_hook(&project),
            "#!/bin/sh\nprintf '\\n  spaced reason  \\n' >&2\nexit 2\n",
        );
        let outcome = check_in(Some(&project), &tmp.path().join("home"), &facts()).await;
        assert_eq!(
            outcome,
            PolicyOutcome::Deny {
                reason: "\n  spaced reason  \n".to_string()
            }
        );
    }

    /// A timed-out hook is killed as a PROCESS GROUP: helpers it spawned die
    /// with it instead of leaking past the verdict.
    #[tokio::test]
    async fn a_timed_out_hooks_descendants_die_with_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let pid_file = tmp.path().join("helper.pid");
        write_hook(
            &project_hook(&project),
            &format!(
                "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
                pid_file.display()
            ),
        );
        let script = resolve_hook(Some(&project), &tmp.path().join("home")).unwrap();
        let outcome = run_hook_with(&script, &facts(), Duration::from_millis(300)).await;
        assert!(
            matches!(outcome, PolicyOutcome::ScriptError { .. }),
            "got {outcome:?}"
        );
        let helper: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The helper was SIGKILLed with its group; allow the kernel a moment
        // to reap the reparented corpse, then require it gone.
        let mut dead = false;
        for _ in 0..40 {
            if unsafe { libc::kill(helper, 0) } == -1 {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(dead, "helper {helper} survived the group kill");
    }

    /// The drain grace ABORTS a stuck reader; detaching it would leak a task
    /// (and the pipe fd) whenever a stray writer outlives the hook.
    #[tokio::test]
    async fn a_stuck_drain_is_aborted_not_detached() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = SetOnDrop(dropped.clone());
        let handle = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
            Vec::new()
        });
        assert_eq!(join_drain(handle).await, Vec::<u8>::new());
        // Abort propagation is asynchronous; the guard's Drop is the proof
        // the task was cancelled rather than left running.
        let mut cancelled = false;
        for _ in 0..40 {
            if dropped.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(cancelled, "drain task was detached, not aborted");
    }

    /// Any other exit code is a FAULT, not a verdict: fail-closed, and worded
    /// so nobody mistakes a broken script for a policy decision.
    #[tokio::test]
    async fn other_exit_codes_are_script_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let path = project_hook(&project);
        write_hook(&path, "#!/bin/sh\nexit 3\n");
        let outcome = check_in(Some(&project), &tmp.path().join("home"), &facts()).await;
        let PolicyOutcome::ScriptError {
            path: named,
            detail,
        } = &outcome
        else {
            panic!("expected a script error, got {outcome:?}");
        };
        assert_eq!(named, &path);
        assert!(detail.contains("exited 3"), "got {detail}");
        assert_eq!(outcome.deny_reason(), Some(DenyReason::PolicyScriptError));
        let text = outcome.refusal_text().unwrap();
        assert!(text.contains("policy_script_error"), "got {text}");
        assert!(text.contains(&path.display().to_string()), "got {text}");
        assert!(!text.contains("denied by policy"), "got {text}");
    }

    /// A hook that never returns is killed at the deadline and reported as a
    /// timeout — the caller waits three seconds, not forever.
    #[tokio::test]
    async fn a_hanging_hook_times_out_and_is_killed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        write_hook(&project_hook(&project), "#!/bin/sh\nsleep 30\n");
        let script = resolve_hook(Some(&project), &tmp.path().join("home")).unwrap();
        let started = std::time::Instant::now();
        let outcome = run_hook_with(&script, &facts(), Duration::from_millis(300)).await;
        let PolicyOutcome::ScriptError { detail, .. } = &outcome else {
            panic!("expected a script error, got {outcome:?}");
        };
        assert!(detail.contains("timed out"), "got {detail}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the deadline must bound the wait"
        );
    }

    /// Present but not executable: a fault (the daemon cannot ask it anything),
    /// never a silent allow.
    #[tokio::test]
    async fn a_non_executable_hook_is_a_script_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let path = project_hook(&project);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let outcome = check_in(Some(&project), &tmp.path().join("home"), &facts()).await;
        let PolicyOutcome::ScriptError { detail, .. } = &outcome else {
            panic!("expected a script error, got {outcome:?}");
        };
        assert!(detail.contains("cannot run it"), "got {detail}");
    }

    /// No registration, no cache: rewriting the file changes the next verdict.
    #[tokio::test]
    async fn editing_the_hook_takes_effect_on_the_next_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        let path = project_hook(&project);
        write_hook(&path, "#!/bin/sh\necho no >&2\nexit 2\n");
        assert!(matches!(
            check_in(Some(&project), &home, &facts()).await,
            PolicyOutcome::Deny { .. }
        ));
        write_hook(&path, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            check_in(Some(&project), &home, &facts()).await,
            PolicyOutcome::Allow
        );
    }

    /// The project rung REPLACES the global one; with no project hook the
    /// global runs; with neither, allow.
    #[tokio::test]
    async fn project_hook_replaces_global_and_absence_falls_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&project).unwrap();
        write_hook(&global_hook(&home), "#!/bin/sh\necho global >&2\nexit 2\n");
        assert_eq!(
            check_in(Some(&project), &home, &facts()).await,
            PolicyOutcome::Deny {
                reason: "global\n".to_string()
            }
        );

        write_hook(
            &project_hook(&project),
            "#!/bin/sh\necho project >&2\nexit 2\n",
        );
        assert_eq!(
            check_in(Some(&project), &home, &facts()).await,
            PolicyOutcome::Deny {
                reason: "project\n".to_string()
            }
        );

        // A remote-host project has no readable project rung; the global one
        // still governs.
        assert_eq!(
            check_in(None, &home, &facts()).await,
            PolicyOutcome::Deny {
                reason: "global\n".to_string()
            }
        );
        assert_eq!(
            check_in(None, &tmp.path().join("empty-home"), &facts()).await,
            PolicyOutcome::Allow
        );
    }

    /// cwd and env are part of the contract: a hook may read its own project's
    /// files by relative path, and know which hook point it is serving.
    #[tokio::test]
    async fn hook_runs_in_the_project_root_with_the_hook_env_var() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        write_hook(
            &project_hook(&project),
            "#!/bin/sh\nprintf '%s %s' \"$PWD\" \"$CCTEAM_HOOK\" >&2\nexit 2\n",
        );
        let outcome = check_in(Some(&project), &tmp.path().join("home"), &facts()).await;
        let PolicyOutcome::Deny { reason } = outcome else {
            panic!("expected a deny");
        };
        // macOS resolves the tempdir through /private; compare on the suffix.
        assert!(reason.ends_with("pre-agent"), "got {reason}");
        assert!(reason.contains("project"), "got {reason}");
    }

    /// stdin is one compact line of JSON the script can parse with any tool.
    #[tokio::test]
    async fn stdin_carries_the_payload_as_one_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let dump = tmp.path().join("stdin.json");
        write_hook(
            &project_hook(&project),
            &format!("#!/bin/sh\ncat > '{}'\nexit 0\n", dump.display()),
        );
        let payload = json!({"kind": "hire", "caller": {"sid": "s42"}});
        assert_eq!(
            check_in(Some(&project), &tmp.path().join("home"), &payload).await,
            PolicyOutcome::Allow
        );
        let raw = std::fs::read_to_string(&dump).unwrap();
        assert_eq!(raw.lines().count(), 1, "one line: {raw}");
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), payload);
    }

    /// A reason larger than the cap is cut on a char boundary, so the relayed
    /// text is always valid UTF-8.
    #[test]
    fn oversized_reasons_are_cut_on_a_char_boundary() {
        let long = "é".repeat(DENY_REASON_MAX_BYTES);
        let cut = truncate_utf8(long.as_bytes(), DENY_REASON_MAX_BYTES);
        assert!(cut.len() <= DENY_REASON_MAX_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        // No trimming: verbatim survives the cap path too.
        assert_eq!(
            truncate_utf8(b"  spaced  ", DENY_REASON_MAX_BYTES),
            "  spaced  "
        );
    }

    /// The payload contract: sections present, unknown facts omitted (never
    /// null, never zero), and the task head capped.
    #[test]
    fn payload_omits_what_is_unknown_and_caps_the_task_head() {
        let facts = PolicyFacts {
            kind: "hire",
            caller: CallerFacts {
                sid: "s42".into(),
                vendor: "claude".into(),
                depth: Some(0),
                project: "cct".into(),
                context_pct: Some(41),
            },
            request: RequestFacts {
                vendor: "grok".into(),
                task: "x".repeat(TASK_HEAD_MAX_CHARS + 734),
                ..Default::default()
            },
            usage: Some(json!({"claude": {"windows": [{"w": "5h", "pct": 8}]}})),
            counts: CountFacts {
                children: Some(3),
                delegated: Some(12),
                cost_24h_usd: Some(4.200000001),
            },
        };
        let payload = facts.payload();
        assert_eq!(payload["kind"], "hire");
        assert_eq!(payload["caller"]["sid"], "s42");
        assert_eq!(payload["caller"]["context_pct"], 41);
        assert_eq!(payload["request"]["vendor"], "grok");
        assert_eq!(payload["request"]["wait"], 0);
        assert_eq!(
            payload["request"]["task_head"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            TASK_HEAD_MAX_CHARS
        );
        assert_eq!(
            payload["request"]["task_chars"],
            json!(TASK_HEAD_MAX_CHARS + 734)
        );
        assert_eq!(payload["usage"]["claude"]["windows"][0]["pct"], 8);
        assert_eq!(payload["counts"]["cost_24h_usd"], json!(4.2));
        // Unknown facts are absent, not null/empty.
        for absent in ["model", "role", "sid", "title"] {
            assert!(
                payload["request"].get(absent).is_none(),
                "`{absent}` must be omitted: {payload}"
            );
        }
        // One line, always.
        let line = serde_json::to_string(&payload).unwrap();
        assert!(!line.contains('\n'), "{line}");
    }

    /// A call with nothing known still produces a legal payload: `kind` only,
    /// no empty sections to trip a script's `.caller.sid` read.
    #[test]
    fn an_empty_call_renders_only_its_kind() {
        let payload = PolicyFacts {
            kind: "dispatch",
            ..Default::default()
        }
        .payload();
        assert_eq!(payload["kind"], "dispatch");
        assert!(payload.get("caller").is_none(), "{payload}");
        assert!(payload.get("usage").is_none(), "{payload}");
        assert!(payload.get("counts").is_none(), "{payload}");
        // `request` survives on `wait` alone — the one field always known.
        assert_eq!(payload["request"], json!({"wait": 0}));
    }
}
