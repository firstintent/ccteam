# ccteam DSH plugins — install and use

> Chinese version: [dsh-plugin-cn.md](dsh-plugin-cn.md)

Two independent plugins connect DeepSeek Harness (DSH) and ccteam. This guide
covers hand-started `dsh web` profiles and the ccteam UI inside DSH. For
ccteam's own **DSH** page and the reverse direction (ccteam hiring DSH
sessions), see [usage.md](usage.md) under “DSH Web”.

`ccteam-ui` is where ccteam's core experience — driving several harnesses
from one place — lives inside DSH's own UI. It is built as a DSH client plugin
in every respect (DSH slots, DSH primitives and design tokens, DSH locale,
DSH settings cards), never as a port of the ccteam web console.

## 1. What you get

| Plugin | Audience | What it provides |
|---|---|---|
| `@ccteam/ccteam-client` | DSH agents (the LLM) | The eight ccteam MCP tools inside DSH sessions, plus the transport that lets ccteam hire DSH sessions. |
| `@ccteam/ccteam-ui` | People using DSH Web | The ccteam panel in DSH — a cross-vendor session tree, embedded chat, and one-click spawn, opened with the ccteam button at the bottom of DSH’s own sidebar — plus the **ccteam-ui** and **ccteam-client** cards in DSH Settings → Plugins. |

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
dsh plugin --profile web add @ccteam/ccteam-client
dsh plugin --profile web add @ccteam/ccteam-ui
```

Restart that `dsh web` process, then hard-refresh the browser (Ctrl+Shift+R or
Cmd+Shift+R). Install `ccteam-client` when a DSH agent should call ccteam
tools; install `ccteam-ui` when you want the human panel. Install both for the
full connection. DSH’s Settings → Plugins → **Plugin list** shows them as
`ccteam-client` and `ccteam-ui`.

As an alternative, an administrator can open ccteam web → **Settings → Hosts**
and click **Register DSH plugin** for a detected local DSH instance. That
shortcut registers both plugins; restart the DSH process yourself.

### 3.2 Configure each plugin

Open DSH **Settings → Plugins → Plugin configuration**. `ccteam-ui`
contributes one card per ccteam plugin (a card only appears once its plugin
is installed); fill in the card, then **Save**. DSH shows this tab only to a
browser it considers the operator's own machine: open a hand-started `dsh web`
from `127.0.0.1`, or go through ccteam's DSH page, which declares the page
owns its Host (see usage.md → DSH Web; DSH reads that from the release after
0.1.1-rc.2 — on rc.2 use an SSH tunnel to the companion port); a native
`dsh web` opened over a LAN address leaves the tab empty.

| Card | Set | Where to get it |
|---|---|---|
| **ccteam-client** | the ccteam daemon URL and the enrollment credential `ccteam-enroll:<id>:<secret>` | ccteam web → **Settings → Access** (copy the enrollment value) |
| **ccteam-ui** | the ccteam daemon URL and your personal REST API token (optionally a default project) | ccteam web → **Settings → Account**, developer REST card (a prefix-less paste is accepted). Only a hand-started `dsh web` needs this: an instance ccteam starts or registers (Hosts → Register DSH plugin) already carries your own token. |

Credentials are write-only in the cards: they show **Configured** or **Not
configured**, never the value. Leaving a credential field blank keeps the
stored value. If you installed only `ccteam-client`, edit the
`ccteam-client` section of DSH’s settings file instead (Settings → **Open
configuration file**).

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

The panel needs the native sidebar-footer and overlay seats DSH ships since
0.1.0-rc.7.

## 5. Troubleshooting

| Symptom | Fix |
|---|---|
| **Not connected** | Run `ccteam start`; the panel also shows a copyable command. |
| **401** | A REST request on the wire uses `Bearer ccteam:<hex>`. Plugin 1’s setting is the `ccteam-enroll:<id>:<secret>` enrollment string; plugin 2’s setting is the personal REST token. They are different credentials. In the panel settings, paste the REST token without `Bearer`. |
| **`duplicate loader entry id` at boot** | The same plugin was inserted twice (for example, registry plus bundle patch, or a hand-edited `cordis.patch.yml`). Keep exactly one entry and remove the duplicate. |
| **No ccteam button in the sidebar** | The plugin needs DSH 0.1.0-rc.7 or newer (the native sidebar footer and overlay seats). Update DSH; then check Settings → Plugins → Plugin list shows `ccteam-ui` as Enabled. |
| **Plain-HTTP LAN problems** | See [usage.md](usage.md) → “Access and security” for the DSH Web security-context note. |
| **Human DSH turns missing from ccteam** | Expected: turns typed in DSH’s own UI remain vendor-native. ccteam’s ledger and transcript contain only turns ccteam routed; DSH keeps the complete conversation. |

## 6. Versions and updates

Use **DSH 0.1.0-rc.7 or newer**. Update or remove a package with the same
profile-scoped command family (the package name is required):

```bash
dsh plugin --profile web update @ccteam/ccteam-client
dsh plugin --profile web update @ccteam/ccteam-ui
dsh plugin --profile web remove @ccteam/ccteam-client
dsh plugin --profile web remove @ccteam/ccteam-ui
```

Removing either plugin is safe. It removes only that plugin’s own entry; it
does not delete DSH sessions or rewrite DSH’s unrelated configuration.
