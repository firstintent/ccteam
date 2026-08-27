# @ccteam/ccteam-ui

The ccteam UI inside the DeepSeek Harness web console: a cross-vendor session
tree, an embedded chat view, and one-click spawn — reached from a native
sidebar footer button, rendered entirely with DSH's own UI primitives and
design tokens — plus the settings cards of both ccteam plugins in DSH's
Plugin configuration tab.

Built the way a DSH client plugin is built: `dsh.client` in package.json
names the packages it composes against, the client half contributes its seats
through `ctx.slots.inject(...) → ctx.slots.register(...)`
(`sidebar.footer.action`, `shell.overlay`, `settings.plugin.item`), copy goes
through `ctx.locale`, business state reaches components as framework-bound
selector hooks, and stylesheets are lightningcss CSS Modules under the
preset's `[hash]_[local]` pattern.

| Not this | Boundary |
| --- | --- |
| A second ccteam web | No ccteam-web components, styles, or theme are ported here. Only wire shapes are shared. |
| A tool/transport surface | That is `@ccteam/ccteam-client`. This package is UI only; the two install independently. |
| A credential holder in the browser | The ccteam REST token lives in the host half. The browser speaks only to this plugin's own `/ccteam/api` BFF. |

## Install (hand-started DSH)

1. `dsh plugin --profile web add @ccteam/ccteam-ui`
2. Restart `dsh web`, hard-refresh, then open DSH Settings → Plugins →
   **ccteam-ui** and set the daemon URL plus your personal REST token
   (ccteam web console → Settings → Account).
3. Look for the ccteam button at the bottom of the sidebar.

The same tab shows a **ccteam-client** card once that plugin is installed —
this package contributes both cards, because the client plugin ships no
browser half of its own.

ccteam-managed DSH runtimes get this plugin and its credentials materialized
automatically — no manual steps.
