# @ccteam/dsh-client

| Not this | Boundary |
| --- | --- |
| Installer | Start `ccteam` yourself. This package never downloads or starts it. |
| Content pack | No roles, prompts, memories, or workflow content ship here. |
| Secret vault | Managed credentials are used in memory and scrubbed from `process.env` at boot. |
| Standalone daemon | The package only bridges DSH to an already running ccteam daemon. |

## Mode 2 Install

1. `dsh plugin --profile ccteam add @ccteam/dsh-client`
2. In DSH Settings, set `daemonUrl` and paste the `ccteam-enroll:<id>:<secret>` string from `ccteam config`.
3. Restart the DSH profile, then call `status`.
