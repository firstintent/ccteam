# W2 Design Note — `ccteam mux daemon` re-exec protocol

> **Source-grounded from `references/rmux/crates/rmux-sdk/src/handles/rmux/connect.rs` line 60-180**.

## rmux SDK launches the daemon how?

`rmux_sdk::Rmux::builder().connect_or_start().await`:

1. Resolves endpoint (UDS path on Unix / Named Pipe on Windows; default `/tmp/rmux-{uid}/default`)
2. Tries `try_connect_validated(socket_path)` — if a daemon already listens, just reuse
3. Otherwise calls a *launcher* closure → in default flow:
   ```rust
   Command::new(daemon_binary())
       .arg("--__internal-daemon")
       .arg(endpoint_path)
       .stdin(Null).stdout(Null).stderr(Null)
       .spawn()
   ```
   with detachment via `rmux_os::daemon::configure_hidden_daemon_command` (setsid + double-fork on Unix; `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | DETACHED_PROCESS` on Windows)
4. `daemon_binary()` = `std::env::var_os("RMUX_SDK_DAEMON_BINARY").unwrap_or_else(|| "rmux".into())`
5. Polls socket until daemon answers

**The `--__internal-daemon` flag is the same one `rmux`'s own `src/main.rs::try_main()` checks at startup**, routing to `rmux_server::ServerDaemon::new(config).bind().await.wait()`.

## ccteam single-binary integration

```
                                  ┌──────────────────────────────────┐
                                  │ ccteam orchestrator (binary)      │
                                  │  starts up, needs mux             │
                                  └────────────┬─────────────────────┘
                                               │
                  std::env::set_var("RMUX_SDK_DAEMON_BINARY",
                                    std::env::current_exe()?);
                                               │
                                               ▼
                                  rmux_sdk::Rmux::builder()
                                      .unix_socket("~/.ccteam/run/mux.sock")
                                      .connect_or_start().await
                                               │
                                  Tries to connect → daemon not listening
                                               │
                                  Spawns: <ccteam_exe> --__internal-daemon <socket>
                                               │
                                               ▼
                          ┌─────────────────────────────────────────┐
                          │ ccteam binary, second invocation         │
                          │  parses argv, sees --__internal-daemon   │
                          │  → routes to ccteam_mux::run_daemon(...)│
                          │  → rmux_server::ServerDaemon::new(...)   │
                          │      .bind().await.wait().await          │
                          │                                          │
                          │  Detached process, survives the original │
                          │  ccteam orchestrator exit                │
                          └─────────────────────────────────────────┘
```

## ccteam CLI surface for the daemon

In `ccteam-cli`'s arg parsing (clap), intercept `--__internal-daemon <socket>` BEFORE the normal subcommand parser sees it:

```rust
fn main() -> Result<()> {
    let raw_args: Vec<OsString> = std::env::args_os().collect();

    // Intercept rmux SDK's daemon spawn protocol — mirrors rmux's
    // src/main.rs::try_main()'s first arg check.
    if raw_args.len() >= 3 && raw_args[1] == "--__internal-daemon" {
        let socket = raw_args[2].clone();
        return ccteam_mux::run_internal_daemon(socket);
    }

    // ... normal clap parse continues
}
```

And `ccteam_mux::run_internal_daemon`:

```rust
pub fn run_internal_daemon(socket: OsString) -> io::Result<()> {
    let socket_path = PathBuf::from(socket);
    let config = rmux_server::DaemonConfig::builder()
        .socket_path(socket_path)
        // (optional) override config-file fallback / etc.
        .build();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(hidden_daemon_worker_threads())
        .build()?;

    runtime.block_on(async move {
        let server = rmux_server::ServerDaemon::new(config).bind().await?;
        server.wait().await
    })
}
```

## Default socket path

| OS | path |
|---|---|
| Linux/macOS | `$HOME/.ccteam/run/mux.sock` (owner-only mode 0600) |
| Windows | `\\.\pipe\ccteam-mux-<user>` |

ccteam orchestrator at startup:
```rust
std::env::set_var("RMUX_SDK_DAEMON_BINARY", std::env::current_exe()?);
// then use RmuxEndpoint::UnixSocket(<ccteam-owned-path>)
```

This forces SDK to spawn **ccteam itself** as the daemon, and reach the ccteam-owned socket (not the default `/tmp/rmux-{uid}/default`).

## Why this is better than bundling the `rmux` binary

1. **One binary distribution** — no second artifact to ship in release.yml
2. **Version-pin alignment** — rmux-server lib version always matches what ccteam orchestrator compiles against; no drift
3. **No PATH conflict** — user's own `rmux` install (if any) sees its own socket; ccteam's daemon answers on a separate ccteam-owned socket
4. **Audit-clean** — `ccteam doctor --verify-mcp` style audit can confirm `current_exe() === RMUX_SDK_DAEMON_BINARY`

## Lifecycle edges to think about

- **First ccteam startup**: daemon doesn't exist → `connect_or_start` spawns it; ~50-200ms startup
- **ccteam restart**: daemon still alive (sibling process, survives) → `connect_or_start` reuses
- **Daemon crash**: ccteam re-spawns it via `connect_or_start`; sessions inside the dead daemon are lost (rmux v0.3.1 has no snapshot recovery — V0.9 followup per research §11.2 B1)
- **Daemon idle**: rmux daemon doesn't self-terminate by default; ccteam can pass `--idle-shutdown-secs N` to the daemon via DaemonConfig for non-mode-3 use cases (mode 3 is 24/7 → never idle-shutdown)
- **Multi-ccteam-process safety**: socket path includes uid (`{HOME}/.ccteam/...`); per-user daemon; concurrent ccteam invocations connect to same daemon (UDS allows multi-client)
- **Codex `app-server` UDS coexistence**: Codex's own UDS path (`~/.codex/...`) is unrelated; daemon's CodexUdsBridge is a JSON-RPC *client* against that socket, not a server

## Open items for W2 implementation

1. Confirm `rmux_server::ServerDaemon::new` API stability between 0.3.x patches (semver minor → potentially breaking; pin patch version in Cargo.toml or accept rmux 0.3.x and add CI smoke on bumps)
2. Decide whether to set `RMUX_SDK_DAEMON_BINARY` in ccteam process scope or globally — process scope is cleaner (doesn't leak to subagent child processes)
3. Test ConPTY launcher on Windows — `configure_hidden_daemon_command` does the right thing per rmux source, but verify
4. Permission model: `~/.ccteam/run/` should be mode 0700, socket 0600 — same as rmux upstream defaults

## Reference

- `references/rmux/crates/rmux-sdk/src/handles/rmux/connect.rs:177-180` — `daemon_binary()` resolution
- `references/rmux/crates/rmux-sdk/src/bootstrap/discovery.rs:25` — `RMUX_SDK_DAEMON_BINARY` env var constant
- `references/rmux/src/main.rs:60-90` — rmux's own re-exec routing on `--__internal-daemon` flag (template for ccteam's subcommand)
- `references/rmux/crates/rmux-server/Cargo.toml` — `publish = true`, OK to depend on
