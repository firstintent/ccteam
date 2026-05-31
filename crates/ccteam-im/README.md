# ccteam-im — IM-bot daemon

V0.6.0 Wave 2 F109 / F116 deliverable.

## Scope

`ccteam-im` is the single per-host daemon that bridges IM platforms
(Telegram / Slack / Discord; Lark / DingTalk / WeChat in V0.7) to
ccteam-managed long-running chat sessions (`mode: chat` bots).

```text
┌──────────────┐    inbound      ┌─────────────┐    HarnessAdapter
│ TG / Slack / │ ──────────────► │ ccteam-im  │ ───► claude-tui /
│ Discord      │                 │  (this)     │      codex (via tmux)
└──────────────┘                 └─────────────┘            │
        ▲                              │                    │
        │                              │ tail               │
        │                              ▼                    ▼
        │                       turns.jsonl ◄────── .ccteam/chat/<bot>/
        └────────── outbound ─────────┘
```

## Architecture decisions

See `docs/versions/v0-6-0/wave-2-decisions.md`:

- **Option C openhuman integration** — vendor trait + slim TG / Slack /
  Discord providers in-crate (avoids openhuman's heavy transitive
  closure: whisper-rs, tauri, browser embedders, AI libs).
- **HTTP-only Slack** — no Socket Mode (saves `tokio-tungstenite` dep,
  V0.6 host-probe scope is mock-only).
- **TG long-polling** — `getUpdates` rather than webhook server (no
  public URL required for V0.6).
- **No DM/group webhook server** — V0.7+ optional.

## Layout

```text
src/
├── main.rs              CLI entry (clap subcommands: run, status, register, unregister)
├── lib.rs               Module declarations + register_bot / unregister_bot public API
├── daemon.rs            Main tokio::select event loop
├── supervisor.rs        Per-bot tmux session health + crash restart + heartbeat
├── inbound.rs           IM → mailbox / send-keys via HarnessAdapter
├── outbound.rs          tail turns.jsonl → Channel::send_message
├── router.rs            @mention parsing + bot-to-bot routing + hop_limit
├── nl_admin.rs          @ccteam <NL admin> command parser (pause/resume/list/...)
├── credentials.rs       ~/.ccteam/im/credentials.json (mode 0600) reader
├── sanitize.rs          OMC reply-listener content stripping (backtick, $(, ${, ctrl, bidi)
├── rate_limit.rs        Per-user token bucket (default 10/min)
├── acl.rs               workflow.yaml chat_acl allowlist enforcement
├── three_layer_sec.rs   sig auth + rate limit + content sanitize composition
└── transport/
    ├── mod.rs           Channel trait + ChannelMessage / SendMessage types
    └── providers/
        ├── mod.rs
        ├── mock.rs      Test-only in-memory channel
        ├── telegram.rs  long-polling getUpdates + sendMessage
        ├── slack.rs     HTTP chat.postMessage + conversations.history polling
        └── discord.rs   webhook send + REST messages GET
systemd/ccteam-im.service
tests/
├── router_test.rs
├── daemon_test.rs
├── credentials_test.rs
├── sanitize_test.rs
└── dep_graph_test.rs    Guards ccteam-core 0 openhuman dep
```

## Usage

> **V0.6.1 F130** — the standalone `ccteam-im` binary has been removed.
> The supervisor loop now runs inside `ccteam start` as one tokio task
> alongside the orchestrator and the embedded web UI, sharing one
> shutdown channel.

```bash
# Start the combined daemon (orchestrator + web + IM supervisor)
ccteam start

# Orchestrator + web only (skip IM bridge)
ccteam start --no-imd

# Orchestrator + IMD only (skip web UI)
ccteam start --no-web

# Register a bot (idempotent; supervisor task picks up via registry watcher).
# Today this is driven through the `ccteam-creator` / `ccteam-im-setup`
# skills + the library API `ccteam_im::register_bot`; the previous
# `ccteam-im register` CLI subcommand was removed with the binary.

# Liveness
ls -l ~/.ccteam/state/imd.heartbeat
```

Library entry for embedding (used by `ccteam start`):

```rust
use ccteam_im::{run_daemon_with_shutdown, DaemonArgs};
run_daemon_with_shutdown(DaemonArgs::default(), async {
    let _ = tokio::signal::ctrl_c().await;
})
.await?;
```

## Files / paths

| Path                                            | Purpose                                  |
| ----------------------------------------------- | ---------------------------------------- |
| `~/.ccteam/im/credentials.json`                 | Per-platform tokens (mode 0600)          |
| `~/.ccteam/imd/registry/<slug>/<role>.json`     | Bot registration                         |
| `~/.ccteam/state/imd.heartbeat`                 | Daemon liveness                          |
| `<project>/.ccteam/chat/<bot>/turns.jsonl`      | Outbound source (written by tui adapter) |
| `<project>/.ccteam/chat/<bot>/heartbeat`        | Per-bot tmux supervisor heartbeat        |
| `<project>/.ccteam/chat/<bot>/signals/*.signal` | Graceful shutdown / drain triggers       |

## Three-layer security (OMC parity)

Inspired by `references/oh-my-claudecode/src/notifications/reply-listener.ts`:

1. **Signature auth** — Slack `v0:<ts>:<body>` HMAC-SHA256 verify (+
   timestamp replay window); Telegram chat-ID binding; Discord
   `authorized_user_ids` allowlist.
2. **Rate limit** — Per-sender token bucket (default 10 msgs / 60 s,
   shared across all platforms via `RateLimiter::can_proceed`).
3. **Content sanitize** — Strip control chars (`\x00-\x08\x0b\x0c\x0e-\x1f\x7f`),
   bidi overrides (`U+202A-U+202E`, `U+2066-U+2069`), escape `` ` ``
   `$(` `${` `\`, replace newlines with spaces, length-cap.
