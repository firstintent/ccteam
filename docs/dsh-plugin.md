# ccteam DSH plugins — install and use

> Chinese version: [dsh-plugin-cn.md](dsh-plugin-cn.md)

Two independent plugins connect DeepSeek Harness (DSH) and ccteam. This guide
covers hand-started `dsh web` profiles and the panel inside DSH. For ccteam's
own **DSH** page and the reverse direction (ccteam hiring DSH sessions), see
[usage.md](usage.md) under “DSH Web”.

## 1. What you get

| Plugin | Audience | What it provides |
|---|---|---|
| `@ccteam/dsh-client` | DSH agents (the LLM) | The eight ccteam MCP tools inside DSH sessions, plus the transport that lets ccteam hire DSH sessions. |
| `@ccteam/dsh-team` | People using DSH Web | A ccteam panel in DSH: a cross-vendor session tree, embedded chat, and one-click spawn. Open it with the ccteam button at the bottom of DSH’s own sidebar. |

The two packages are independent. You can install either one or both.

## 2. Mode 1 — ccteam-managed (recommended, zero steps)

When DSH is running through ccteam — for example `/new dsh`, the ccteam DSH
page, or `session_spawn` with `vendor:"dsh"` — ccteam materializes both plugins
and their credentials in your identity’s DSH runtime. There is nothing to
install or paste.

Check two things:

- The DSH sidebar has a **ccteam** button at its bottom.
- A DSH session hired by ccteam can answer the `status` tool call.

## 3. Mode 2 — your own `dsh web`

### 3.1 Install

From the profile used by that web instance:

```bash
dsh plugin --profile web add @ccteam/dsh-client
dsh plugin --profile web add @ccteam/dsh-team
```

Restart that `dsh web` process, then hard-refresh the browser (Ctrl+Shift+R or
Cmd+Shift+R). Install the client when a DSH agent should call ccteam tools;
install the team plugin when you want the human panel. Install both for the
full connection.

As an alternative, an administrator can open ccteam web → **Settings → Hosts**
and click **Register DSH plugin** for a detected local DSH instance. That
shortcut registers `@ccteam/dsh-client`; restart the DSH process yourself.

### 3.2 Configure each plugin

In DSH **Settings**, configure the plugin’s own card:

| Plugin | Set | Where to get it |
|---|---|---|
| `@ccteam/dsh-client` | `daemonUrl` and the enrollment string `ccteam-enroll:<id>:<secret>` | ccteam web → **Settings → Access** (copy the enrollment value) |
| `@ccteam/dsh-team` | `daemonUrl` and your personal REST API token | ccteam web → **Settings → Account**, developer REST card (a prefix-less paste is accepted) |

Use the same daemon URL for both, commonly `http://127.0.0.1:7331`. These are
different credentials: enrollment identifies the DSH process for MCP, while
the REST token identifies your ccteam account for the panel. Do not substitute
one for the other.

## 4. Using the panel

The normal loop is:

1. Click the ccteam button in DSH’s sidebar footer.
2. Browse the session tree, grouped by project. Activity dots show working,
   idle, or stale sessions; delegated children are indented below their parent.
3. Select a session to open its embedded chat.
4. Type a turn and press **Enter**. **Shift+Enter** inserts a newline; **Esc**
   returns to the tree (and closes the panel from the tree view).

Receipts are deliberately explicit. A queued turn says it is queued (including
what it is behind when available); a failed turn shows its error kind. If a
new session is created but its first task fails, the session still opens so you
can inspect it and try again.

To hire another vendor, choose **+** in the tree header. The vendor picker
greys out vendors that are not installed on the relevant host. The project
picker is hidden when you have only one visible project. **Advanced** contains
model, effort, and mode controls; **Enter** creates the session and opens its
chat, while **Esc** cancels.

When the panel is closed, the ccteam button carries a completion count for
turns finished since the last open. Opening the panel clears the badge.

On older DSH versions without the newer sidebar entry point, the same panel
appears behind a floating handle on the right edge. That fallback is expected.

## 5. Troubleshooting

| Symptom | Fix |
|---|---|
| **Not connected** | Run `ccteam start`; the panel also shows a copyable command. |
| **401** | A REST request on the wire uses `Bearer ccteam:<hex>`. Plugin 1’s setting is the `ccteam-enroll:<id>:<secret>` enrollment string; plugin 2’s setting is the personal REST token. They are different credentials. In the panel settings, paste the REST token without `Bearer`. |
| **`duplicate loader entry id` at boot** | The same plugin was inserted twice (for example, registry plus bundle patch, or a hand-edited `cordis.patch.yml`). Keep exactly one entry and remove the duplicate. |
| **Floating handle instead of sidebar button** | Your DSH is older than the native sidebar entry point. Use the handle or update DSH. |
| **Plain-HTTP LAN problems** | See [usage.md](usage.md) → “Access and security” for the DSH Web security-context note. |
| **Human DSH turns missing from ccteam** | Expected: turns typed in DSH’s own UI remain vendor-native. ccteam’s ledger and transcript contain only turns ccteam routed; DSH keeps the complete conversation. |

## 6. Versions and updates

Use **DSH 0.1.0-rc.6 or newer**. Update or remove a package with the same
profile-scoped command family (the package name is required):

```bash
dsh plugin --profile web update @ccteam/dsh-client
dsh plugin --profile web update @ccteam/dsh-team
dsh plugin --profile web remove @ccteam/dsh-client
dsh plugin --profile web remove @ccteam/dsh-team
```

Removing either plugin is safe. It removes only that plugin’s own entry; it
does not delete DSH sessions or rewrite DSH’s unrelated configuration.
