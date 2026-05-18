# V0.6.0 Wave 2 — Decisions

Captured during the imd teammate's Wave 2 implementation
(`crates/ccteam-imd/`). Reviewed: 2026-05-19.

---

## §1. New workspace crate `ccteam-imd`

**Decision**: ship F109 + F116 as a standalone workspace member
`crates/ccteam-imd/` rather than folding into `ccteam-core` or
`ccteam-cli`.

**Why**:
- Keeps `ccteam-core` free of `reqwest` and IM-platform deps (red
  line — `tests/dep_graph_test.rs` enforces this).
- Lets the daily-driver `ccteam` binary stay lightweight; the daemon
  is opt-in.
- The supervisor + tmux long-session story (F108) lives behind the
  `HarnessAdapter` trait, so cross-crate calls are pure trait
  dispatch — no execution-module reach-through.

**Cost**: one extra `cargo build` target. The decision adds ~3.5
kLOC Rust (12 source files + 5 integration test files).

---

## §2. CLI integration model

**Decision**: `ccteam daemon {start|stop|status}` exec's
`ccteam-imd` as a child process (via
`Command::new(env::var("CCTEAM_IMD_BIN").unwrap_or("ccteam-imd"))`)
rather than linking it as a workspace crate dependency.

**Why**: linking would pull `reqwest` into the `ccteam` binary's
closure (≈ +1.4 MB on release builds, every `ccteam status` invocation
loads it). Exec-out keeps the CLI fast and the daemon optionally
installable / upgradable. The `CCTEAM_IMD_BIN` env knob makes hermetic
testing easy.

**Trade-off**: status is two-process round-trip; no in-process error
introspection. Acceptable for a daemon — it has its own log stream.

---

## §3. openhuman integration — **Option C (vendor + slim)** chosen over A/B

Wave 2 brief offered three options for pulling in IM transport code
from `references/openhuman/`:

| Option | Description | Verdict |
| --- | --- | --- |
| A | Add `openhuman_core` as a path dep | **Reject** — pulls whisper-rs, Tauri, browser embedders, AI libs; multi-minute compile delta. |
| B | Extract `references/openhuman/src/openhuman/channels/` into a fresh workspace crate `ccteam-channels` | **Reject** — telegram provider has tight coupling to openhuman's `event_bus` / `security::pairing` / `config` subsystems; extracting them cleanly is more work than rewriting against our trait surface. |
| **C** | **Vendor the [`Channel`] trait + slim TG/Slack/Discord providers directly under `crates/ccteam-imd/src/transport/`** | **Accept** |

**Why C**:
- The trait surface is ~60 lines (see `references/openhuman/src/openhuman/channels/traits.rs`).
  Lifting it preserves wire-format parity without ceremony.
- ccteam-imd has its own ACL / sanitize / rate-limit layers; the
  openhuman providers' `event_bus::publish_global` / `PairingGuard`
  hooks are redundant in our context. We get a smaller, less coupled
  module by reimplementing the network shells.
- One workspace crate is easier to feature-gate per provider than
  upstreaming `#[cfg]` into openhuman.

**Attribution**: each vendored file carries a
`// Slim port of references/openhuman/src/openhuman/channels/...`
header. Stable upstream features (e.g. Telegram's `getUpdates` shape)
remain wire-compatible.

---

## §4. Three-layer security parity with OMC

Mirror of `references/oh-my-claudecode/src/notifications/reply-listener.ts`:

1. **Layer 1 — signature / sender auth.** Slack HMAC-SHA256 over
   `v0:<ts>:<body>` (replay window 300 s); Telegram chat-ID binding
   (post-`getUpdates`); Discord allowlist by user id.
2. **Layer 2 — rate limit.** Per-sender sliding window, default
   10 events / 60 s (matches OMC `RateLimiter` constructor).
3. **Layer 3 — content sanitize.** Strip control chars (`\x00–\x08`,
   `\x0b`, `\x0c`, `\x0e–\x1f`, `\x7f`), bidi overrides
   (`U+202A–U+202E`, `U+2066–U+2069`), escape `\` `` ` `` `$(` `${`,
   then `trim()`. Tmux-bound variant additionally collapses
   newline/CR/tab into single spaces.

Slack `verify_slack_signature_stub` is a **stub** in V0.6 — replay
window enforced but no HMAC backend wired (no `hmac`/`sha2`/`subtle`
deps). Conservative deny by default. V0.7 swap when Slack inbound
HTTP receiver lands.

---

## §5. Transport mode choices

| Platform | Inbound | Outbound | Rationale |
| --- | --- | --- | --- |
| Telegram | `getUpdates` long-poll (25 s) | `sendMessage` | No public HTTPS URL required for host-probe scope. |
| Slack    | `conversations.history` polling (4 s) | `chat.postMessage` | Avoids `tokio-tungstenite` Socket Mode dep; HTTP-only deployment is sufficient for V0.6 mock probes. V0.7 may add Socket Mode if needed. |
| Discord  | `messages` REST polling (4 s) | `messages` POST | No gateway WebSocket — same rationale as Slack. |

---

## §6. TG token state — **mock-only V0.6 host probe**

User has not pasted a Telegram bot token. Acceptance for the
ccteam-imd crate:

- Binary builds + `cargo test -p ccteam-imd` passes against a `MockChannel`.
- Real-network probes (`getMe` health check, end-to-end TG round-trip)
  are deferred to a follow-up after `telegram:configure` skill
  collects the token via paste flow.
- The `TelegramChannel::api_url` template + getUpdates JSON shape are
  unit-tested so a wrong endpoint shape would fail without a token.

---

## §7. Heartbeat + signal-file contract (F116)

Per-bot directory layout (relative to `<project>/.ccteam/chat/<bot>/`):

```text
heartbeat                # mtime liveness — refreshed by claude-tui adapter every <60s
signals/shutdown.signal  # @ccteam stop → graceful tear-down (terminal)
signals/drain.signal     # @ccteam pause → stop accepting new turns; drain inflight
turns.jsonl              # written by tui adapter; tailed by outbound.rs
inbox/msg-<ts>-<seq>.md  # mailbox envelope written by inbound.rs
```

Daemon-global:

```text
~/.ccteam/state/imd.heartbeat            # refreshed every supervisor tick
~/.ccteam/imd/registry/<slug>/<role>.json  # one file per registered bot
~/.ccteam/im/credentials.json            # mode 0600 (enforced on load)
```

---

## §8. What this PR does *not* cover

These are explicitly out of Wave 2 imd scope; tracked elsewhere:

- **`ClaudeTuiAdapter` implementation** — Wave 2 tui-impl teammate
  (in-progress). The daemon will call through `HarnessAdapter` once
  it lands; no further code change required here.
- **workflow.yaml `mode: chat`** — added by tui-impl teammate
  (`ccteam-core/src/workflow.rs`). The imd registry mirrors `(slug,
  role, vendor, im_platform, im_chat_id)` independently of workflow
  schema.
- **Real Slack HMAC verification** — V0.7 (stub left in place; replay
  window enforced).
- **Lark / DingTalk / QQ / WeChat providers** — V0.7 (feature names
  reserved in `Cargo.toml`).
- **Bot-to-bot routing via IM group** — router supports the hop counter
  and bot-handle resolution; the daemon's `decide_and_log` will route
  recursive `@bot` turns through `process_inbound` in the F108-wired
  followup.
