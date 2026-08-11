# Installing ccteam

> **Agents:** if you were asked to "install https://github.com/firstintent/ccteam", follow this file top to bottom. Do the **Install** step, then relay the **Finish in the web console** step to the user. Do not improvise other commands.

ccteam is a resident daemon (IM gateway + web console + MCP) that drives the stock Claude Code / Codex / Grok / OpenCode / Kimi / Pi CLIs. You install it once, start it in the background, then finish setup in the web console.

## Prerequisites

- **At least one coding CLI installed and signed in** — ccteam is the bridge, not the agent, so a vendor that is missing (or installed but not logged in) cannot host a session:

  | Vendor | Install | Sign in |
  |---|---|---|
  | Claude Code | [docs.claude.com/en/docs/claude-code](https://docs.claude.com/en/docs/claude-code) | `claude auth login` |
  | Codex | [github.com/openai/codex](https://github.com/openai/codex) | `codex login` |
  | Grok Build | [docs.x.ai/build/overview](https://docs.x.ai/build/overview) | `grok login` |
  | OpenCode | [opencode.ai](https://opencode.ai) | `opencode auth login` |
  | Kimi Code | [moonshotai.github.io/kimi-code](https://moonshotai.github.io/kimi-code/) | `kimi login` |
  | Pi | `npm i -g @earendil-works/pi-coding-agent` | provider API key, checked with `pi auth check --provider <provider>` |

  Verify with `<bin> --version`; after install, `ccteam status` reports which vendors this machine has and whether each is authenticated.
- For `make install`: **Rust + Node.js**. No toolchain? Use the prebuilt `install.sh` instead (see below).

## Install

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam
make install
```

`make install` builds the release binary, symlinks it to `~/.local/bin/ccteam`, starts the daemon in the **background**, and prints the **web console URL and login token**.

No Rust / Node.js on this machine? Use the prebuilt binary:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam daemon start
```

The daemon is self-managed — one command, the same on Linux, macOS, and WSL. It keeps running after you close the terminal, but it does **not** auto-start on boot: after a reboot, run `ccteam daemon start` again.

### Do NOT

- **Do not run `ccteam start`** for a real install — that runs the daemon in the *foreground* (dev / one-off), tied to your terminal. Use `ccteam daemon start` (background), which `make install` already did.
- **Do not run `ccteam config` by hand** — MCP registration is a one-time click in the web console.

## Finish in the web console

Open the URL the install printed (the login token is also at `~/.ccteam/secrets/web-token`), then:

1. **Register MCP** (one-time) — Hosts page → *Register ccteam MCP*.
2. **Create a project**.
3. **Settings → IM** — connect Telegram (bot token) or Lark/Feishu (App ID + Secret).

Then drive your agents from the console or your IM.

## Managing it later

```bash
ccteam daemon status     # is it running, and on which version?
ccteam daemon restart    # restart it
ccteam daemon stop       # stop it (sessions come back next start)
ccteam update            # update to the latest release, then restart onto it
```

Day-2 ops and the full command surface are in [docs/usage.md](docs/usage.md) ([中文](docs/usage-cn.md)).

> The web console binds `0.0.0.0:7331` with token auth and no TLS — keep it on a trusted LAN; do not expose it to the public internet.
