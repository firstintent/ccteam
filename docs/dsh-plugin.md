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
| `@ccteam/ccteam-ui` | People using DSH Web | The ccteam workbench in DSH — a whole-page surface with the cross-vendor team tree, a native-grade conversation (streaming Markdown, tool steps, choice prompts, attachments, interrupt) and a details column, opened with the ccteam button at the bottom of DSH’s own sidebar — plus the **ccteam-ui** and **ccteam-client** cards in DSH Settings → Plugins. |

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
tools; install `ccteam-ui` when you want the human workbench. Install both for the
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
owns its Host (see usage.md → DSH Web; on 0.1.1-rc.2 and earlier ccteam
back-ports that read into the served client bundle); a native `dsh web`
opened over a LAN address leaves the tab empty.

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
the REST token identifies your ccteam account for the workbench. Do not substitute
one for the other.

## 4. Using the workbench

The ccteam button at the bottom of DSH’s sidebar opens the workbench as a
pane **docked beside DSH**: DSH’s own sidebar, conversation and details keep
working on the left while ccteam runs on the right (to DSH the pane is
simply a narrower window). Drag the pane’s left edge to resize it, press
**⤢** to expand it over the full page and again to dock it back; one
animated width is the whole transition. Inside, the layout follows the
pane’s width — three columns from about 1240px, two columns (details as a
slide-over sheet) from about 880px, and a single column below that, where
the team fills the pane until a session is chosen, a back control returns
to it, and the details sheet slides over the conversation.

- **Team** (left): **New session**, a search box, and every session grouped
  by project — vendor monogram, title, `vendor · model · when`, the activity
  dot and the accumulated cost; delegated children are indented under their
  parent. Project headers fold and show the project’s total cost.
- **Main** (center): the selected session’s conversation, or the new-session
  hero when nothing is selected.
- **Details** (right, toggle in the header): identity (sid, project, vendor,
  model, effort, role, host), state, usage (cost, tokens, context window from
  the live statusline), delegation links, and actions — rename, interrupt the
  running turn, stop the session (two-step confirm), copy the sid. Clicking a
  step row in the conversation shows that step here.

**New session** is DSH’s own empty-conversation shape: pick the **project**,
**vendor** (not-installed vendors are greyed) and optional **role** (the
project’s `.claude/agents/*.md`), choose a **model** and **effort** from the
vendor’s catalog in the composer bar (blank = vendor default), type the first
task and press **Enter**. The title comes from the task’s first line; the
session opens as soon as it exists. Validation is inline and the daemon’s own
error is shown verbatim under the box.

**Conversation**: user turns are bubbles; assistant turns render Markdown with
DSH’s own renderer, streaming while the turn runs, with the turn’s steps
(tool calls, commands, file edits, searches, thinking) as compact rows above
the text — spinning while running, green when done. Human-in-the-loop prompts
appear as a card with the choices; pick one to answer. Queued turns say what
they are queued behind; failures show their error kind; session lifecycle and
delegation events show as small notes. While a turn runs the send button
becomes **stop** (a non-destructive interrupt — the session keeps its
context). **Enter** sends, **Shift+Enter** inserts a newline, `/` at the start
lists the pass-through commands (`/compact`, `/new`, `/clear`, `/role`,
`/model`), the paper-clip adds attachments (images render inline).
**Load earlier messages** pages the transcript back. ccteam records a step’s
name and summary only; the full tool input/output stays in the vendor’s own
session, and steps are shown live, not replayed from history.

**Esc** leaves a text field first, then closes the details, then docks a
full-page pane, then closes the pane. When the workbench is closed, the ccteam button carries a
completion count for turns finished since the last open; opening it clears
the badge.

The workbench needs the native sidebar-footer and overlay seats DSH ships
since 0.1.0-rc.7.

## 5. Troubleshooting

| Symptom | Fix |
|---|---|
| **Not connected** | Run `ccteam start`; the workbench also shows a copyable command. |
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
