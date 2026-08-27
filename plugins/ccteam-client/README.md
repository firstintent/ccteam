# @ccteam/ccteam-client

| Not this | Boundary |
| --- | --- |
| Installer | Start `ccteam` yourself. This package never downloads or starts it. |
| Content pack | No roles, prompts, memories, or workflow content ship here. |
| Secret vault | Credentials arrive per session over ACP `_meta.ccteam` and stay in memory. Nothing is read from or written to `process.env`. |
| Standalone daemon | The package only bridges DSH to an already running ccteam daemon. |

## Faces

- **Tool surface** — the eight original ccteam MCP tools, always registered. A
  call runs under the calling session's own bearer when ccteam hired it,
  otherwise under the enrollment credential.
- **Transport surface** — with `transportSocket` set, the plugin serves ACP on
  that unix socket so ccteam can drive sessions inside this DSH runtime. Each
  connection is an isolated peer; only turns this transport queued are reported
  back, so a human chatting with the same session in the DSH UI stays private
  to the DSH UI.

## Mode 2 Install

1. `dsh plugin --profile ccteam add @ccteam/ccteam-client`
2. In DSH Settings, set `daemonUrl` and paste the `ccteam-enroll:<id>:<secret>` string from `ccteam config`.
3. Restart the DSH profile, then call `status`.
