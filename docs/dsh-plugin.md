# ccteam's DSH plugin — install and use

> Chinese version: [dsh-plugin-cn.md](dsh-plugin-cn.md)

One plugin, `@ccteam/ccteam-ui`, connects DeepSeek Harness (DSH) and ccteam.
This guide covers installing it — either as the whole of ccteam, engine
included, or next to a ccteam you already run — and using the ccteam
workbench inside DSH. For ccteam's own **DSH** page and the reverse direction
(ccteam hiring DSH sessions), see [usage.md](usage.md) under “DSH Web”.

The plugin is where ccteam's core experience — driving several harnesses from
one place — lives inside DSH's own UI. It is built as a DSH client plugin in
every respect (DSH slots, DSH primitives and design tokens, DSH locale, a DSH
settings card), never as a port of the ccteam web console.

## 1. What you get

Installing it once gives you three faces and an engine supervisor:

| Face | Audience | What it provides |
|---|---|---|
| **Workbench** | people using DSH Web | The ccteam workbench in DSH — a whole-page surface with the cross-harness team tree, a native-grade conversation (streaming Markdown, tool steps, choice prompts, attachments, interrupt) and a details column, opened with the ccteam button at the bottom of DSH’s own sidebar. |
| **Tools** | DSH agents (the LLM) | The eight ccteam MCP tools inside DSH sessions, so a DSH agent can hire and drive the rest of the team. |
| **Transport** | ccteam | The ACP server that lets ccteam hire DSH sessions. It arms itself only when the profile row carries a socket path, which only a ccteam-managed runtime writes. |
| **Engine** | you | The ccteam daemon itself: the plugin ships the `ccteam` binary as a platform package, installs it, starts the daemon, and shows an **Engine** section (state, version, **Start / Stop / Restart / Update engine**) at the top of its settings card. |

Each face activates on its own: a profile without DSH's web app still gets the
tools, and a profile without an agent runtime still gets the workbench.

## 2. Two install paths, one daemon

ccteam is one binary engine with two install surfaces — the `ccteam` CLI and
this plugin. Both put the binary in the same place, use the same
`$CCTEAM_HOME` (default `~/.ccteam`), and run the **same daemon**. Pick
whichever you start from; the other one attaches later (see §3).

### 2.1 Path A — the plugin brings the engine

Nothing else to install first. From the profile your `dsh web` uses:

```bash
dsh plugin --profile web add @ccteam/ccteam-ui
```

Then restart that `dsh web` process and hard-refresh the browser
(Ctrl+Shift+R or Cmd+Shift+R). On boot the plugin:

1. **Installs the engine.** The binary rides an `optionalDependencies`
   platform package, `@ccteam/engine-<os>-<cpu>` (`linux`/`darwin` ×
   `x64`/`arm64`; npm and pnpm fetch only the one that matches this machine,
   and none of them has a lifecycle script). The plugin copies its
   `bin/ccteam` to the location `install.sh` uses — `$CCTEAM_INSTALL_DIR`
   if set, else the directory where a `ccteam` already on your `PATH` lives
   (symlinks resolved), else `~/.local/bin` — verifying the copy answers
   `--version` before swapping it in. A destination that is a symlink or
   inside a package manager's tree (`node_modules`, Homebrew, nix, snap) is
   reported, never overwritten; set `CCTEAM_INSTALL_DIR` to install elsewhere.
2. **Starts the daemon.** With **Start the engine when the plugin loads** on
   (the default) it runs
   `ccteam start --json` — the same detached, idempotent launcher the CLI
   uses — and waits for `GET /health`. If a daemon is already answering for
   this home it simply attaches (§3).
3. **Bootstraps credentials.** On the same machine, under the same OS user,
   the workbench needs no pasting: the plugin reads exactly one file,
   `$CCTEAM_HOME/secrets/web-token` (the console token the daemon writes for
   its own operator), and asks the daemon for this installation's tool-face
   enrollment credential — `POST /api/v1/enroll` with `ensure: true` and the
   label `dsh-plugin:<profile>`, so the daemon keeps exactly one credential
   per DSH profile no matter how often DSH restarts — storing it in the
   settings card. A token you enter in the card always wins over the file.
4. **Gates the workbench on the engine** (§4): until the daemon answers, the
   first screen says **The ccteam engine is not running** with a **Start the
   engine** button; with no project yet it shows **Add a workspace**.

The switch can be turned off in the settings card; off, the plugin only
probes — it installs and starts nothing, and a running daemon is unaffected.

### 2.2 Path B — the CLI first

Install the `ccteam` CLI and start it (`curl -sSL
https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh`,
then `ccteam start` — see [usage.md](usage.md)). Then get the plugin in one
of three ways:

- **Materialized by ccteam (zero steps).** When DSH runs *through* ccteam —
  `/new dsh`, the ccteam **DSH** page, or `session_spawn` with
  `vendor:"dsh"` — ccteam materializes the plugin and its credentials in your
  identity's DSH runtime. Check: the DSH sidebar has a **ccteam** button at
  its bottom, and a DSH session hired by ccteam can answer the `status` tool
  call.
- **Registered from ccteam web.** An administrator opens **Settings →
  Hosts** and clicks **Register DSH plugin** for a detected local DSH
  instance; restart that DSH process yourself afterwards (ccteam never
  restarts a process it did not start).
- **Installed by hand.** `dsh plugin --profile web add @ccteam/ccteam-ui`,
  then restart `dsh web`. The plugin finds the running daemon and attaches.

In every Path B variant the Engine section reads **Attached** (*Started from
the CLI / another entry; the plugin shows it*): the plugin manages nothing it
did not start (§3).

## 3. Coexistence: one `$CCTEAM_HOME`, one daemon

Native DSH with the plugin, ccteam web, the CLI, and a systemd unit under the
same OS user all share one home and one daemon. The rules:

- **Whoever starts first wins; everyone else attaches.** The plugin probes
  `GET /health` before doing anything. A daemon whose `home` equals the
  plugin's resolved `$CCTEAM_HOME` is *the* daemon: the Engine section reads
  **Attached** (started elsewhere) or **Running** (started by this plugin).
  Typing `ccteam start` while the plugin's daemon is up answers
  `alreadyRunning{pid,home}` with the same pid — there is no second instance,
  and no foreground mode to start one with.
- **Disposing the plugin never stops the daemon.** A DSH restart,
  `dsh plugin update`, or disabling the plugin releases the plugin's probes
  and nothing else; your Telegram gateway and running delegations continue,
  and the plugin re-attaches on its next boot.
- **Stop is explicit and total.** **Stop** in the Engine section confirms
  first (*Stop the engine?* — “ccteam web and the IM gateway stop with it;
  existing sessions are unaffected and resume on their next message.”) and
  then runs `ccteam daemon stop`: it stops the one daemon (agent processes are
  never killed). **Restart** confirms the same way, since it takes the daemon
  down too. Under systemd, the unit may bring it back.
- **A different home is reported, never fixed.** If `/health` reports a
  `home` other than the plugin's, the Engine section reads **Home mismatch** (the
  first screen: **Engine home mismatch**, both homes listed) and the plugin
  will not start a second daemon; point `CCTEAM_HOME` (or the card's
  daemon URL) at the same home so both halves share one engine.
- **Your own install is never duplicated.** When ccteam registers or
  materializes its plugin into your `~/.dsh` web profile and finds
  `@ccteam/ccteam-ui` already installed by you (`dsh plugin add`), it writes
  only its configuration row — no second copy, no second bundle entry, so
  DSH never sees a `duplicate loader entry id`. A version that differs from
  the copy ccteam embeds is reported by `ccteam doctor` and on the **Hosts**
  page as `plugin_version_mismatch{installed,embedded}`, and left for your
  own `dsh plugin --profile web update @ccteam/ccteam-ui`.
- **The plugin never writes `~/.dsh`.** Installing it there was your
  `dsh plugin add`; ccteam only appends its own override row.
- **Remote or managed daemons are attach-only.** A daemon URL that is not
  loopback, a runtime ccteam itself started (the daemon is its parent), or a
  profile row ccteam materialized with credentials all mean somebody else
  owns the engine: the Engine section shows the state and one sentence saying
  why, and no buttons.

## 4. The Engine section, the first screen and the banner

**Settings card.** DSH **Settings → Plugins → Plugin configuration →
ccteam-ui** opens with the **Engine** section:

- **State** — a dot and one of: *Reading engine state…* · **Running**
  (started by this plugin) · **Attached** (started elsewhere; the hint reads
  *Started from the CLI / another entry; the plugin shows it*) · **Starting**
  · **Installing** · **Stopped** (installed, daemon not running) · **Not
  installed** · **Platform not supported** · **Home mismatch** · **Version
  mismatch** (§3 / §7). Under a mismatch or an unsupported platform the
  host's own sentence is printed underneath; in the healthy states the facts
  line stands in for it.
- **Facts** — `engine v<installed>` (hover shows the binary's path),
  `daemon v<running>` only when the running daemon differs from the installed
  binary, `pid <n>`, `$CCTEAM_HOME` (middle-truncated; hover shows it whole),
  the web bind, and an **Open ccteam web** link while the daemon answers.
- **Start · Stop · Restart · Update engine** — `ccteam start`,
  `ccteam daemon stop`, stop-then-start, and
  `ccteam update --channel npm --binary <platform package bin>`. **Update
  engine** appears only when that is the fix (a version mismatch with an older
  engine). **Stop** and **Restart** ask first — *Stop the engine?* / *Restart
  the engine?* — because both take the daemon down: “ccteam web and the IM
  gateway stop with it; existing sessions are unaffected and resume on their
  next message.” Update goes through the engine's own updater on purpose: it
  drains in-flight turns, restarts gracefully and verifies the new version,
  none of which copying a file over a running daemon would do.
- **Start the engine when the plugin loads** — the auto-start switch, on by
  default; off, “the plugin only probes — it installs and starts nothing; a
  running daemon is unaffected.”
- **Advanced** → **Engine path override** (“Blank = ccteam on PATH, then the
  default install location; applies on save.”) and **Resolved binary** — the
  `ccteam` actually in use and how it was found (`configured` · `path` ·
  `canonical`).
- **Engine log** — the last 50 lines of `$CCTEAM_HOME/daemon.log` (the file
  `ccteam daemon logs` prints), with **Refresh**.

When the plugin does not supervise the engine (§3), the buttons are replaced
by one sentence saying why — for example *ccteam started this DSH; the plugin
does not manage the engine's lifecycle and only shows its state.* or *The
engine is not on this machine; there is nothing here to start or stop.*

**Inside the workbench.** The header carries an **Engine: <state>** dot;
clicking it opens the same engine panel inside the workbench (DSH gives a
plugin no way to jump to its settings page, so the panel ends with *Engine
settings: DSH Settings → Plugins → Plugin configuration → ccteam-ui*); **Esc**
closes it. On first use the workbench is gated on the engine:

- **The ccteam engine is not running** — with the reason (*The engine is
  installed; the daemon is not running.* or *No engine is installed yet;
  starting installs it from the plugin's platform package.*) and one **Start
  the engine** button. **Engine home mismatch** lists the plugin's home and
  the daemon's with *Point both at one CCTEAM_HOME, then restart DSH*;
  **Platform not supported** says that ccteam publishes engines for linux and
  macOS on x64 / arm64 only.
- **Add a workspace** — once the engine answers and there is no project yet:
  **Directory (absolute path)**, **slug (optional)** (blank = from the
  directory name), **Add**. When DSH itself has workspaces, an **Import from
  DSH** list offers each of them as a one-click row; with none, the list is
  simply absent.
- The **version banner** across the top: *Engine v… is older than the plugin
  requires (v…)* with an **Update engine** button, or *Plugin v… is older than
  the engine (v…)* with a copyable
  `dsh plugin --profile <name> update @ccteam/ccteam-ui` and the hint *profile
  = the one you started dsh web with*; **Dismiss** hides it.
  The repair is one-way — the running binary is never swapped silently.

## 5. Configure the faces

The same card holds the connection settings. DSH shows this tab only to a
browser it considers the operator's own machine: open a hand-started
`dsh web` from `127.0.0.1`, or go through ccteam's DSH page, which declares
the page owns its Host (see usage.md → DSH Web; on 0.1.1-rc.2 and earlier
ccteam back-ports that read into the served client bundle); a native
`dsh web` opened over a LAN address leaves the tab empty.

| Field | What it is | When you fill it |
|---|---|---|
| **ccteam daemon URL** | the daemon every face talks to; default `http://127.0.0.1:7331` | only for a daemon elsewhere (another port, a LAN machine); a non-loopback URL makes the Engine section attach-only |
| **REST API token** | identifies **you**, so the workbench can read your team | bootstrapped from `$CCTEAM_HOME/secrets/web-token` on the same machine; paste it (ccteam web → **Settings → Account**, prefix-less accepted) only for a daemon whose home you cannot read |
| **Enrollment credential** | identifies **this DSH process**, so its agent can call ccteam tools | asked of a local daemon automatically — one slot per DSH profile (`dsh-plugin:<profile>`), the daemon answers the same record on every restart — and stored here; paste one from ccteam web → **Settings → Access** (`ccteam-enroll:<id>:<secret>`) for a daemon elsewhere |
| **Default project** (optional) | the project new sessions land in when the workbench names none | a project slug; blank means the workbench asks |

The two credentials are **not** interchangeable — one is a person, the other a
process. Credentials are write-only in the card: it shows **Configured** or
**Not configured**, never the value, and leaving a field blank keeps the
stored value. An instance ccteam starts or registers (Hosts → Register DSH
plugin) already carries your own REST token.

## 6. Using the workbench

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

## 7. Troubleshooting

| Symptom | Fix |
|---|---|
| **Not connected** / **The ccteam engine is not running** | Press **Start the engine** on the first screen (or **Start** in the settings card's Engine section, or run `ccteam start`; the panel also shows a copyable command). **Stopped** means the binary is there and the daemon is not; **Not installed** means no binary — starting installs it from the platform package first. |
| **Home mismatch** (first screen: **Engine home mismatch**) | A daemon is answering at the configured URL from a different `$CCTEAM_HOME`; the panel lists both homes. The plugin will not start a second one. Point both at one `CCTEAM_HOME` and restart DSH, or point the card's daemon URL at the daemon you mean. |
| **Version mismatch** / version banner | The running engine is not the version this plugin ships against. *Engine … is older than the plugin requires* → **Update engine** (drain + graceful restart + verify). *Plugin … is older than the engine* → copy the banner's `dsh plugin --profile <name> update @ccteam/ccteam-ui` (`<name>` = the profile you started `dsh web` with, usually `web`) and restart DSH. The running binary is never swapped silently. |
| **Platform not supported** | ccteam publishes engines for Linux and macOS on x64 and arm64; nothing is installed on other platforms (Windows is unsupported; WSL counts as Linux). Install ccteam another way and point the card at it, or use ccteam web. |
| **Install refused: symlink / package-managed destination** | The ladder landed on a `ccteam` that belongs to something else (a symlink, Homebrew, nix, snap, a `node_modules` tree). Update it with that tool, or set `CCTEAM_INSTALL_DIR=<dir>` for the DSH process so the plugin installs elsewhere. |
| **401** | A REST request on the wire uses `Bearer ccteam:<hex>`. The card's **REST API token** is that personal token (paste it without `Bearer`); the **enrollment credential** is the `ccteam-enroll:<id>:<secret>` string. They are different credentials — check you did not paste one into the other's field. |
| **`duplicate loader entry id` at boot** | The plugin was inserted twice (for example, registry plus bundle patch, or a hand-edited `cordis.patch.yml`). Keep exactly one entry and remove the duplicate. ccteam's own registration never adds a second entry next to one you installed. |
| **`cordis.patch.yml` came back reformatted or without its comments** | The first time ccteam registers into a profile you manage by hand, it re-serializes the patch file: formatting is normalized and YAML comments are not preserved (`serde_yaml` has no comment-preserving round-trip). After that ccteam never rewrites the file unless its content changes — a repeated registration is byte-identical — and every write is atomic (temp file + rename, mode 0600). Keep notes outside the patch file. |
| **`ccteam doctor` warns `plugin_version_mismatch`** | Your own `dsh plugin add` copy differs from the version this ccteam embeds. ccteam leaves it alone; run `dsh plugin --profile web update @ccteam/ccteam-ui`. |
| **No ccteam button in the sidebar** | The plugin needs DSH 0.1.0-rc.7 or newer (the native sidebar footer and overlay seats). Update DSH; then check Settings → Plugins → Plugin list shows `ccteam-ui` as Enabled. |
| **Plain-HTTP LAN problems** | See [usage.md](usage.md) → “Access and security” for the DSH Web security-context note. |
| **Human DSH turns missing from ccteam** | Expected: turns typed in DSH’s own UI remain harness-native. ccteam’s ledger and transcript contain only turns ccteam routed; DSH keeps the complete conversation. |

## 8. Versions and updates

Use **DSH 0.1.0-rc.7 or newer**.

**Version lockstep.** Each plugin release pins the engine version it was
published with (`ccteam.engine` in its `package.json`, equal to the
`@ccteam/engine-*` packages it depends on). When no daemon is running, the
plugin brings the installed binary to that version before starting it. When
one is running, the plugin never touches the binary: a different version is
shown as a mismatch with a one-way fix — **Update engine** on the card when
the engine is older, `dsh plugin --profile web update @ccteam/ccteam-ui` when
the plugin is older. The CLI's `ccteam update` reaches the same engine, and
`ccteam status` shows the running-vs-binary version.

**Platforms.** `@ccteam/engine-linux-x64`, `-linux-arm64`, `-darwin-x64`,
`-darwin-arm64`. Windows has no engine package; the Engine section reads
**Platform not supported** and nothing is installed.

Update or remove the plugin with the same profile-scoped command family (the
package name is required):

```bash
dsh plugin --profile web update @ccteam/ccteam-ui
dsh plugin --profile web remove @ccteam/ccteam-ui
```

Removing it is safe. It removes only that plugin’s own entry; it does not
delete DSH sessions, rewrite DSH’s unrelated configuration, stop the daemon,
or uninstall the `ccteam` binary (remove that with `install.sh --uninstall`
if you no longer want it).
