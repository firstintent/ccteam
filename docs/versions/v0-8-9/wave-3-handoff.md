# v0.8.9 Wave 3 Handoff — byte-faithful rmux web terminal (W2b)

> Phase 3 of the v0.8.9 run. One commit on `dev`: `10d3694`. Roots v0.8.8 bug4 (terminal blank-on-connect) + bug6 (line-wrap misalignment).

## Decided
- **NO rmux dep bump** (the dev-prompt's literal "0.3→0.5" scope is DROPPED). The recon **verified in the local cargo cache** that the pinned **rmux-sdk 0.3.1 ALREADY ships the byte-faithful API** — `PaneOutputStream`, `PaneOutputChunk::Bytes { sequence, bytes }` ("preserve every payload byte the daemon"), `PaneOutputStart::{Now,Oldest}`, `Pane::output_stream()` / `output_stream_starting_at()`. The "needs 0.5" premise (`rmux-update.md` + the memory note) was **stale**. So byte-faithfulness ships on 0.3.1 with **zero dep churn** (and the sandbox can't fetch 0.5 anyway). The W2b "gap" was purely a ccteam-side implementation choice (consuming the lossy `line_stream()`/`snapshot()`).
- **`subscribe` rewrite**: `pane.output_stream()` (`Now`, live tail) → `MuxEvent::OutputChunk(raw bytes VERBATIM)` (no `\n` re-append / `\r` strip / lossy decode) + re-split lines from the raw bytes (a `drain_lines_into` mirror of `tmux_backend::subscribe`) to feed `matcher.match_line` → `PatternMatched`. `Lag` → `OutputDropped { behind: missed_events }` + drop the partial line.
- **`capture` rewrite**: `output_stream_starting_at(Oldest)` backlog drain — bounded `poll_once()` (stop on first empty batch + a 64-poll guard + a byte budget) → raw ANSI bytes. `PaneSnapshot` is a parsed cell-grid with **no** raw-byte accessor (even in 0.5), so the backlog stream is the only byte-faithful source. `with_ansi` is now meaningful (raw either way).
- **`pty.rs` capture-then-subscribe (the v0.8.8 bug4 fix) LOGIC unchanged** — it now seeds raw bytes for BOTH backends (only the "rmux renders text" comment updated).
- **Pattern/marker safety**: the recon proved the line/pattern stream is **dormant by default** — `TypedEventTap` (the only `PatternMatched` consumer) is `CCTEAM_TYPED_EVENTS`-gated (set nowhere in prod); the marker chain + `chat_turn_completed` + the live `turns.jsonl` writer are **hook + transcript driven, not the pane stream**. So the framed "HIGH risk" was low; the line-resplit preserves the semantics exactly regardless.

## Rejected
- **The 0.3→0.5 dep bump**: unnecessary (0.3.1 has the API), risky (2-minor drift), unfetchable in the sandbox. Flagged as a scope change from the dev-prompt — the byte-faithful *goal* is fully met on 0.3.1.
- **DUAL subscribe** (open `output_stream()` + `line_stream()` both — the recon's option a): chose option (b) re-split (one subscription, mirrors what tmux already does; the SDK's `PaneLineStream` is literally `output_stream` + LF-split, so re-splitting is the same logic).
- **Leaving `capture` as rendered text**: chose the `Oldest`-drain so the connect seed is byte-faithful too (not just the live stream).

## Risks
- **UNTESTABLE in the sandbox** (no rmux daemon / PTY): verified by compile + the non-PTY unit tests (`drain_lines_*`) + the drift canary + clippy + the full non-web suite. **Real-terminal faithfulness (bug4/bug6 actually fixed) needs the USER on a real rmux machine.** Added an `#[ignore]` env-gated daemon smoke test (`subscribe_and_capture_are_byte_faithful`) that asserts ESC bytes survive subscribe + capture — for that real-machine run.
- **`capture` `Oldest`-drain heuristic**: stops on the first empty `poll_once` batch (assumes the daemon delivers the retained backlog as immediately-available batches before the cursor reaches live). A trickled large backlog with a transient empty gap could yield a short seed — but the live `subscribe` self-corrects on the next TUI redraw, and the 64-poll + byte budget bound the work. Best-effort, documented, untestable here.
- **`rmux-update.md` (this version's design doc) + the `rmux-0-5-raw-terminal` memory are now STALE** ("needs 0.5"). Phase 5 should correct `rmux-update.md`; the memory will be updated.

## Files
- **ccteam-harness**: `src/rmux_backend.rs` (subscribe + capture rewrite + W2b doc removal + module-header rewrite), `tests/smoke_rmux_sdk.rs` (drift canary + new raw types), `tests/rmux_backend_session_roundtrip.rs` (+`#[ignore]` byte-faithful smoke).
- **ccteam-web**: `src/pty.rs` (comment), `src/routes/pane_snapshot.rs` (W2b doc), `src/routes/pty_ws.rs` (W2b doc).
- Commit `10d3694` (review-fixes folded in: `screenshot.rs` stale W2b doc + `lines==0` capture harden). **No `Cargo.toml` change** (confirmed).

## Remaining
- **User real-machine verify**: under default rmux (no `CCTEAM_MUX_BACKEND`), confirm the web terminal renders a claude TUI faithfully (bug6) + shows the screen on connect (bug4). Run the `#[ignore]` smoke + a manual check.
- **Phase 5**: correct `rmux-update.md`'s "needs 0.5" framing → "0.3.1 already had it; no bump".
- (Future) harden the `capture` `Oldest`-drain if real-daemon backlog-batching behaves unexpectedly.

## Gate
`cargo test --workspace --exclude ccteam-web` **1898/0**; clippy `--all-targets` 0; fmt clean; `ccteam-web` 229 + 4 env-gated `ws_*` (pre-existing PTY). **Adversarial review: clean, no P0/P1** — the untestable capture-`Oldest`-drain verified **cannot-hang** against the SDK contract tests (`poll_once` never sleeps + empty-batch exit + 64-poll cap + byte budget); subscribe is byte-verbatim (borrow-then-move, no clone/mangle); the line-resplit→`PatternMatched` is byte-identical to the tmux template. 3 P2: `screenshot.rs` stale W2b doc + `lines==0` harden FIXED (folded into `10d3694`); benign EOF extra-poll noted (no fix).
