# W2c Migration Spec — three deferred sites from W1

> W1 subagent intentionally deferred 3 callers as "too intricate for wrap-only wave". After W2a lands RmuxBackend, these migrations can use either TmuxBackend or RmuxBackend — same trait surface. This doc specifies the exact migration shape for each.

## Target sites (recap from W1 acceptance report)

1. `ClaudeTuiAdapter::start_thread` F164 reattach + F172 V2 `--resume` composite — `crates/ccteam-core/src/execution/claude_tui.rs:251-372`
2. `CodexExecAdapter::start_thread` + `close_thread` lifecycle — `crates/ccteam-core/src/execution/codex_exec.rs:203-256, 496-521`
3. `ccteam-web::pty::PtySession::{bring_up, tear_down}` — 3 raw `tmux pipe-pane` sites + ~300 LOC FIFO + broadcast registry

## Site 1: ClaudeTuiAdapter::start_thread

### Current shape (sync TmuxSession calls inside async fn)

```rust
let session = TmuxSession::from_name(session_name.clone());
if session.exists() {
    if is_pane_running_claude(&session) {
        // (a) reattach — list_pane_pids only
    } else {
        // (b) --resume path
        session.kill()?;
        session.start_with_env(&cwd, &argv, &env)?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        if !is_pane_running_claude(&session) {
            session.kill()?;
            session.start_with_env(&cwd, &fresh_argv, &env)?;
            // emit chat_session_reset
        }
    }
} else {
    // (c) new
    session.start_with_env(&cwd, &argv, &env)?;
}
```

### Migration shape

```rust
let backend = ccteam_mux::default_backend();  // Arc<dyn MuxBackend>
let id = MuxSessionId::from(session_name.clone());

if backend.exists(&id).await? {
    if pane_runs_claude(&*backend, &id).await? {
        // (a) reattach — same intent
    } else {
        // (b) --resume path
        backend.kill(&id).await?;
        backend.spawn(spec_for_resume(...)).await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        if !pane_runs_claude(&*backend, &id).await? {
            backend.kill(&id).await?;
            backend.spawn(spec_for_fresh(...)).await?;
            // emit chat_session_reset (unchanged)
        }
    }
} else {
    // (c) new
    backend.spawn(spec_for_new(...)).await?;
}
```

### `is_pane_running_claude` migration

The current free fn does `session.list_pane_pids()` (sync) + `ps -p <pid> -o comm=` (sync subprocess). Migration:

```rust
async fn pane_runs_claude(backend: &dyn MuxBackend, id: &MuxSessionId) -> Result<bool> {
    let pids = backend.list_pane_pids(id).await?;
    if pids.is_empty() { return Ok(false); }
    for pid in pids {
        if pid == 0 { continue; }
        let comm = tokio::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output().await?;
        if !comm.status.success() { continue; }
        let s = String::from_utf8_lossy(&comm.stdout);
        if s.trim().contains("claude") { return Ok(true); }
    }
    Ok(false)
}
```

`ps` is OS-level not mux-level — stays as direct subprocess. Move into a sibling helper module `crates/ccteam-core/src/execution/process_inspect.rs` so other adapters can share (codex liveness probe likely benefits later).

### `MuxSessionSpec` construction helpers

W1 introduced `MuxSessionSpec`. Add helper builders in claude_tui.rs (or a `claude_tui_spec.rs` sibling):

```rust
fn spec_for_resume(role, slug, cwd, session_id_name) -> MuxSessionSpec {
    MuxSessionSpec {
        name: chat_session_name(slug, role),
        argv: vec![claude_bin(), "--dangerously-skip-permissions".into(),
                   "--resume".into(), session_id_name.into()],
        working_dir: cwd,
        env: chat_spawn_env_owned(role, slug),  // (String, String) pairs
        size: (200, 50),
        kind: MuxSessionKind::LongLived,
    }
}
// similarly spec_for_fresh, spec_for_new
```

### Other ClaudeTuiAdapter methods

- `submit_turn` (line 406) — `session.exists() / send_keys_literal / send_keys_enter` → trait
- `resume_thread` (line 523) — `session.exists()` → trait
- `close_thread` (line 546) — `session.exists() / send_keys_literal("/exit") / send_keys_enter / sleep 500ms / kill` → trait

All straightforward async substitutions.

## Site 2: CodexExecAdapter

### `start_thread` (codex_exec.rs:203-256)

Current uses `TmuxSession::from_name + exists + start + pane_pid`. Migration: same pattern as ClaudeTuiAdapter — substitute trait calls.

### `close_thread` (codex_exec.rs:496-521)

Current: `exists → send_codex_quit_keys → 500ms sleep → exists → kill`. W1 already migrated `send_codex_quit_keys` to use trait calls (commit 128f093). The outer composite needs the wrapping `spawn_blocking` removed (the inner ops are now async).

### Pre-flight: kill the `spawn_blocking` wrapper

Subagent's W1 note says "outer `spawn_blocking`-wrapped composite stays". The migration is just: remove the `spawn_blocking`, await trait calls directly. The composite is short enough that it doesn't need to be off-runtime.

## Site 3: ccteam-web::pty::PtySession (the FIFO + broadcast registry port)

This is the biggest of the three. ~300 LOC across:
- `crates/ccteam-web/src/pty.rs` (PtySession + Subscription + PtyRegistry)
- `crates/ccteam-web/src/routes/pty_ws.rs` (already partially migrated by W1)

### Current architecture (V0.6.x)

```
PtyRegistry (HashMap<key, Arc<PtySession>>)
├── PtySession::bring_up(key, tmux_session, paths) 
│   ├── mkfifo /run/<key>.fifo
│   ├── spawn fifo tail task → broadcast::Sender<Vec<u8>>
│   ├── tmux pipe-pane (defensive stop)
│   └── tmux pipe-pane "cat >> <fifo>" (start)
├── Subscription (refcount inc on subscribe, dec on drop)
│   └── rx: broadcast::Receiver<Vec<u8>>
└── PtySession::tear_down (refcount hits 0)
    ├── tmux pipe-pane (stop)
    └── unlink fifo
```

### Target architecture (V0.8 W2c)

```
TmuxBackend::subscribe(id) → MuxEventStream
├── internally owns the PtyRegistry+PtySession+FIFO machinery
├── Stream items: MuxEvent::OutputChunk(bytes) for each FIFO read
├── PatternMatched events too if patterns registered
├── Drop the stream = subscriber count decrement
└── When count hits 0: tear_down (stop pipe-pane + unlink fifo)

ccteam-web's old PtyRegistry becomes a thin adapter on top of MuxBackend::subscribe
```

### Implementation steps

1. **Move the FIFO+broadcast registry into TmuxBackend internal state**. Currently lives in `ccteam-web::pty`; move (or duplicate cleanly) into `crates/ccteam-mux/src/tmux_backend.rs` as private state.
2. **Implement TmuxBackend::subscribe** to return a `Pin<Box<dyn Stream<Item=MuxEvent> + Send>>` that drives the broadcast::Receiver, mapping `Vec<u8>` chunks to `MuxEvent::OutputChunk { bytes }`.
3. **Add line-buffering layer** that holds partial line bytes across chunks; on each completed line, run registered patterns; emit `MuxEvent::PatternMatched` when a regex matches.
4. **ccteam-web side**: `PtyRegistry::ensure(...)` becomes a thin call to `backend.subscribe(id).await` followed by a tokio::spawn that fans chunks back to the web's old broadcast::Sender (for compat with existing SSE consumer code). Or just replace the old code entirely with direct stream consumption.

### Edge cases the migration must preserve

- **Refcount on shared subscribers**: 5 web SSE clients view the same session — all multiplex on one underlying FIFO. Refcount must work in TmuxBackend internal state too.
- **Lag handling**: broadcast::Receiver can lag if a slow client falls behind. F56 design says "lag is not fatal" — log + skip + recover. Preserve in the migration.
- **Defensive cleanup**: bring_up calls `tmux pipe-pane` stop first to handle stale state from server crash. Preserve.
- **FIFO open ordering**: read end opened (tail task spawn) **before** write end (pipe-pane invocation). Preserve.

## RmuxBackend.subscribe (vs TmuxBackend.subscribe)

For RmuxBackend, the rmux SDK already exposes `pane.output_stream()` and `pane.line_stream()`. No FIFO machinery needed — daemon owns the broadcast. The migration into trait::subscribe is:

```rust
impl MuxBackend for RmuxBackend {
    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream> {
        let pane = self.rmux().await?.session(...).pane(0,0);
        let line_stream = pane.line_stream().await?;
        let registry = Arc::clone(&self.pattern_registry);
        let stream = futures::stream::unfold((line_stream, registry), |(mut s, reg)| async move {
            let item = s.next().await?;
            let event = match item {
                Ok(PaneLineItem::Line(text)) => {
                    // chunk
                    let chunk = MuxEvent::OutputChunk { bytes: text.into_bytes() };
                    // pattern match
                    // ... emit PatternMatched if needed
                    chunk
                }
                Ok(PaneLineItem::Lag(_)) => return Some((MuxEvent::OutputIdle{..}, (s, reg))),
                Err(_) => return None,
            };
            Some((event, (s, reg)))
        });
        Ok(Box::pin(stream))
    }
}
```

RmuxBackend.subscribe is ~50 LOC vs TmuxBackend.subscribe ~200 LOC (the FIFO port). rmux's design pays dividends.

## Acceptance for W2c

- All 3 sites migrated; no remaining `TmuxSession::from_name` / `crate::tmux::*` direct calls in execution adapters or web routes
- Baseline ≥ 1568 pass / 0 fail (W1 number)
- clippy 0, fmt clean
- `ccteam-web` SSE end-to-end works (manual smoke: subscribe via web SSE, send via send_keys, see output stream)
- `ccteam attach`, `ccteam peek` work through the trait
- F164 + F172 V2 paths still pass `crates/ccteam-core/tests/claude_tui_resume_test.rs`

## Order of work

If W2a + W2b land first:
- W2c can be 1 large subagent doing all 3 sites
- OR 2 parallel subagents: (1) claude_tui + codex_exec sites, (2) pty.rs FIFO port

The FIFO port is the heaviest piece (300 LOC). Splitting it off keeps the claude/codex migration subagent small.
