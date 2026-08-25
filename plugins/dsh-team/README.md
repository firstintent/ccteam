# @ccteam/dsh-team

The ccteam team panel inside the DeepSeek Harness web console: a cross-vendor
session tree, an embedded chat view, and one-click spawn — reached from a
native sidebar footer button, rendered entirely with DSH's own UI primitives
and design tokens.

| Not this | Boundary |
| --- | --- |
| A second ccteam web | No ccteam-web components, styles, or theme are ported here. Only wire shapes are shared. |
| A tool/transport surface | That is `@ccteam/dsh-client`. This package is UI only; the two install independently. |
| A credential holder in the browser | The ccteam REST token lives in the host half. The browser speaks only to this plugin's own `/ccteam/api` BFF. |

## Install (hand-started DSH)

1. `dsh plugin --profile web add @ccteam/dsh-team`
2. In DSH Settings → ccteam Team, set `daemonUrl` and paste your personal REST
   token (ccteam web console → Account).
3. Restart `dsh web`, hard-refresh, and look for the ccteam button at the
   bottom of the sidebar.

ccteam-managed DSH runtimes get this plugin and its credentials materialized
automatically — no manual steps.
