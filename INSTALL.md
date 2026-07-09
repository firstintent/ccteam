# Installing ccteam

> **Agents:** if you were asked to "install https://github.com/firstintent/ccteam", follow this file top to bottom. Do the **Install** step, then relay the **Finish in the web console** step to the user. Do not improvise other commands.

ccteam is a resident daemon (IM gateway + web console + MCP) that drives the stock Claude Code / Codex / Grok CLIs. You install it once as a supervised background service, then finish setup in the web console.

## Prerequisites

- **Claude Code** installed and logged in — `claude --version`. (Codex / Grok Build are optional, only for those vendors.)
- For `make install`: **Rust + Node.js**. No toolchain? Use the prebuilt `install.sh` instead (see below).

## Install

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam
make install
```

`make install` builds the release binary, symlinks it to `~/.local/bin/ccteam`, and brings the daemon up as a **supervised service** — `systemd --user` on Linux, `launchd` on macOS (starts on boot/login, restarts on crash). It then prints the **web console URL and login token**.

No Rust / Node.js on this machine? Use the prebuilt binary, which sets up the same service:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

### Do NOT

- **Do not run `ccteam start`** for a real install — that runs the daemon in the foreground (dev / one-off). The daemon must run as the supervised service from `make install`.
- **Do not run `ccteam config` by hand** — MCP registration is a one-time click in the web console.

## Finish in the web console

Open the URL `make install` printed (the login token is also at `~/.ccteam/secrets/web-token`), then:

1. **Register MCP** (one-time) — Hosts page → *Register ccteam MCP*.
2. **Create a project**.
3. **Settings → IM** — connect Telegram (bot token) or Lark/Feishu (App ID + Secret).

Then drive your agents from the console or your IM. Day-2 ops and the full command surface are in [docs/usage.md](docs/usage.md) ([中文](docs/usage-cn.md)).

> The web console binds `0.0.0.0:7331` with token auth and no TLS — keep it on a trusted LAN; do not expose it to the public internet.
