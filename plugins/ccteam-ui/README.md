# @ccteam/ccteam-ui

The one plugin that connects DeepSeek Harness and ccteam. It carries three
faces, and every DSH profile that installs it gets all three:

| Face | For | What it is |
| --- | --- | --- |
| **Workbench** | people using DSH Web | A cross-harness session tree, an embedded chat view and one-click spawn, reached from a native sidebar-footer button and rendered entirely with DSH's own UI primitives and design tokens. |
| **Tools** | the DSH agent | The eight ccteam MCP tools, under their original names, so a DSH session can hire and drive the rest of the team. |
| **Transport** | ccteam | An ACP server on a unix socket, so ccteam can hire this runtime's sessions. Armed only when the profile's row carries `transportSocket`, which only a ccteam-managed runtime sets. |

Built the way a DSH client plugin is built: `dsh.client` in package.json
names the packages it composes against, the client half contributes its seats
through `ctx.slots.inject(...) → ctx.slots.register(...)`
(`sidebar.footer.action`, `shell.overlay`, `settings.plugin.item`), copy goes
through `ctx.locale`, business state reaches components as framework-bound
selector hooks, and stylesheets are lightningcss CSS Modules under the
preset's `[hash]_[local]` pattern.

Each face injects its own Cordis services (`webServer` for the workbench,
`agents`/`tools` for the other two), so a profile that lacks one still gets
the rest.

| Not this | Boundary |
| --- | --- |
| A second ccteam web | No ccteam-web components, styles, or theme are ported here. Only wire shapes are shared. |
| A credential holder in the browser | Both credentials live in the host half. The browser speaks only to this plugin's own `/ccteam/api` BFF. |
| A prompt surface | No persona, skill, or system prompt ships here; ccteam only routes (`scripts/no-persona-scan.sh` enforces it). |

## Install (hand-started DSH)

1. `dsh plugin --profile web add @ccteam/ccteam-ui`
2. Restart `dsh web`, hard-refresh, then open DSH Settings → Plugins →
   **ccteam-ui** and fill in one card: the daemon URL, your personal REST
   token (ccteam web console → Settings → Account) for the workbench, and the
   enrollment credential (Settings → Access) for the agent's tools.
3. Look for the ccteam button at the bottom of the sidebar.

The two credentials are not interchangeable: the REST token identifies **you**,
the enrollment credential identifies **this DSH process**.

ccteam-managed DSH runtimes get this plugin and its credentials materialized
automatically — no manual steps.
