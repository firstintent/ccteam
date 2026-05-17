# Agent Teams 测试 fixture

来自 host (`ssh rob@192.168.1.19`) 实地探测的 Anthropic Agent Teams 文件,V0.5.0 F95/F96 用作 schema 真源。

| 文件 | 来源 | 用途 |
|---|---|---|
| `config-roblog.json` | `~/.claude/teams/roblog/config.json` | `members[]` 拓扑 schema (5 members,全 ad-hoc) — F95 teams_config_parser 测试 |
| `inbox-team-lead.json` | `~/.claude/teams/roblog/inboxes/team-lead.json` | message 数组 schema (39 messages,含 `read: bool` + idle_notification 系统消息) — F95 teams_inbox_parser 测试 |

**红线** — `~/.claude/teams/` 是 Anthropic SoT,ccteam **只读不写** (PRD V0.5.0 §整体红线 1)。
