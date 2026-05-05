//! ccteam-hooks: Claude Code hook handlers, exposed by `ccteam-cli` as
//! the `ccteam hook <name>` subcommand group. Each handler reads stdin
//! JSON and writes either a stdout decision or a side-effecting append
//! to `~/.ccteam/progress/<slug>.jsonl` / state.json. M0.3 fills in
//! `progress-append`, `parse-phase-end`, and `cost-accumulate`.
