# ccteam's DSH plugin — install and use

> Chinese version: [dsh-plugin-cn.md](dsh-plugin-cn.md)

One plugin, `@ccteam/ccteam-ui`, connects DeepSeek Harness (DSH) and ccteam.
This guide covers hand-started `dsh web` profiles and the ccteam workbench
inside DSH. For ccteam's own **DSH** page and the reverse direction (ccteam
hiring DSH sessions), see [usage.md](usage.md) under “DSH Web”.

The plugin is where ccteam's core experience — driving several harnesses from
one place — lives inside DSH's own UI. It is built as a DSH client plugin in
every respect (DSH slots, DSH primitives and design tokens, DSH locale, a DSH
settings card), never as a port of the ccteam web console.

## 1. What you get

Installing it once gives you three faces:

| Face | Audience | What it provides |
|---|---|---|
| **Workbench** | people using DSH Web | The ccteam workbench in DSH — a whole-page surface with the cross-harness team tree, a native-grade conversation (streaming Markdown, tool steps, choice prompts, attachments, interrupt) and a details column, opened with the ccteam button at the bottom of DSH’s own sidebar. |
| **Tools** | DSH agents (the LLM) | The eight ccteam MCP tools inside DSH sessions, so a DSH agent can hire and drive the rest of the team. |
| **Transport** | ccteam | The ACP server that lets ccteam hire DSH sessions. It arms itself only when the profile row carries a socket path, which only a ccteam-managed runtime writes. |

Each face activates on its own: a profile without DSH's web app still gets the
tools, and a profile without an agent runtime still gets the workbench.

## 2. Mode 1 — ccteam-managed (recommended, zero steps)

When DSH is running through ccteam — for example `/new dsh`, the ccteam DSH
page, or `session_spawn` with `vendor:"dsh"` — ccteam materializes the plugin
and its credentials in your identity’s DSH runtime. There is nothing to
install or paste.

Check two things:

- The DSH sidebar has a **ccteam** button at its bottom.
- A DSH session hired by ccteam can answer the `status` tool call.

## 3. Mode 2 — your own `dsh web`

### 3.1 Install

From the profile used by that web instance:

```bash
dsh plugin --profile web add @ccteam/ccteam-ui
```

Restart that `dsh web` process, then hard-refresh the browser (Ctrl+Shift+R or
Cmd+Shift+R). DSH’s Settings → Plugins → **Plugin list** shows it as
`ccteam-ui`.

As an alternative, an administrator can open ccteam web → **Settings → Hosts**
and click **Register DSH plugin** for a detected local DSH instance; restart
the DSH process yourself afterwards.

### 3.2 Configure it

Open DSH **Settings → Plugins → Plugin configuration** and fill in the
**ccteam-ui** card, then **Save**. DSH shows this tab only to a browser it
considers the operator's own machine: open a hand-started `dsh web` from
`127.0.0.1`, or go through ccteam's DSH page, which declares the page owns its
Host (see usage.md → DSH Web; on 0.1.1-rc.2 and earlier ccteam back-ports that
read into the served client bundle); a native `dsh web` opened over a LAN
address leaves the tab empty.

| Field | What it is | Where to get it |
|---|---|---|
| **ccteam daemon URL** | the daemon every face talks to, commonly `http://127.0.0.1:7331` | your ccteam daemon |
| **REST API token** | identifies **you**, so the workbench can read your team | ccteam web → **Settings → Account**, developer REST card (a prefix-less paste is accepted) |
| **Enrollment credential** | identifies **this DSH process**, so its agent can call ccteam tools | ccteam web → **Settings → Access**, copy the `ccteam-enroll:<id>:<secret>` value |
| **Default project** (optional) | the project new sessions land in when the workbench names none | a project slug; blank means the workbench asks |

The two credentials are **not** interchangeable — one is a person, the other a
process. Fill in whichever faces you use; a blank field simply leaves that face
asking.

Credentials are write-only in the card: it shows **Configured** or **Not
configured**, never the value. Leaving a credential field blank keeps the
stored value. An instance ccteam starts or registers (Hosts → Register DSH
plugin) already carries your own REST token; only a hand-started `dsh web`
needs it pasted.

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
  by project — harness monogram, title, `harness · model · when`, the state
  dot and the accumulated cost; delegated children are indented under their
  parent. The dot is the daemon’s activity verdict first (animated = working,
  green = idle, amber = stale, red = stuck); a settled session then shows its
  residency — a hollow ring means the daemon released its process after the
  harness’s prompt-cache TTL and it resumes by sid on the next message, a
  dimmed disc (with a “stopped” caption) means you stopped it. Hovering a
  row explains the state. Project headers fold and show the project’s total
  cost; hovering one reveals **⋯** (new session, copy slug, expand only this
  project, collapse all) and **+**, which opens the new-session page with
  that project preselected — the same affordances DSH’s own workspace rows
  have.
- **Main** (center): the selected session’s conversation, or the new-session
  hero when nothing is selected.
- **Details** (right, toggle in the header): identity (sid, project, harness,
  model, effort, role, host), state, usage (cost, tokens, context window from
  the live statusline), delegation links, and actions — rename, interrupt the
  running turn, stop the session (two-step confirm), copy the sid. Clicking a
  step row in the conversation shows that step here.

**New session** is DSH’s own empty-conversation shape: pick the **project**,
**harness** (not-installed harnesses are greyed) and optional **role** (the
project’s `.claude/agents/*.md`), choose a **model** and **effort** from the
harness’s catalog in the composer bar (blank = harness default), type the first
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
name and summary only; the full tool input/output stays in the harness’s own
session, and steps are shown live, not replayed from history.

**Switching model / effort mid-session**: the `harness · model · effort ▾`
control in the composer bar lists the harness’s models (from the `/models`
catalog) and its effort ladder; picking one sends the same `/model <id>
[effort]` directive a human would type — the harness performs the switch and
answers with a receipt row, and the label follows the live statusline. This
is the one path every front shares (IM, ccteam web, MCP, DSH); the harness
itself cannot be switched.

**Session rows** carry a **⋯** menu like DSH’s own: open, rename (inline),
copy sid, interrupt the running turn, details, and stop (with a confirm; a
stopped session can still be resumed by sid).

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
| **401** | A REST request on the wire uses `Bearer ccteam:<hex>`. The card's **REST API token** is that personal token (paste it without `Bearer`); the **enrollment credential** is the `ccteam-enroll:<id>:<secret>` string. They are different credentials — check you did not paste one into the other's field. |
| **`duplicate loader entry id` at boot** | The plugin was inserted twice (for example, registry plus bundle patch, or a hand-edited `cordis.patch.yml`). Keep exactly one entry and remove the duplicate. |
| **No ccteam button in the sidebar** | The plugin needs DSH 0.1.0-rc.7 or newer (the native sidebar footer and overlay seats). Update DSH; then check Settings → Plugins → Plugin list shows `ccteam-ui` as Enabled. |
| **Plain-HTTP LAN problems** | See [usage.md](usage.md) → “Access and security” for the DSH Web security-context note. |
| **Human DSH turns missing from ccteam** | Expected: turns typed in DSH’s own UI remain harness-native. ccteam’s ledger and transcript contain only turns ccteam routed; DSH keeps the complete conversation. |

## 6. Versions and updates

Use **DSH 0.1.0-rc.7 or newer**. Update or remove the package with the same
profile-scoped command family (the package name is required):

```bash
dsh plugin --profile web update @ccteam/ccteam-ui
dsh plugin --profile web remove @ccteam/ccteam-ui
```

Removing it is safe. It removes only that plugin’s own entry; it does not
delete DSH sessions or rewrite DSH’s unrelated configuration.
