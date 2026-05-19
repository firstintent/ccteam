# ccteam

> **Claude Code 之上的 multi-agent 编排器** — 一个工具,三档能力:in-proc 临时帮手 / bg 工作流长跑 / IM bot 24/7 常驻。

![demo](docs/v0-6-0/demos/30s-tg-bot-team.gif)

> _GIF 展示形态 3(TG bot 团队)— wave 2 ship 后填_

## 三种运行形态(ccteam 的本体)

ccteam 给 Claude Code 加了 **3 种 multi-agent 运行形态**,各自解决不同节奏的需求:

| # | 形态 | 怎么 host | 用来干啥(典型场景)|
|---|---|---|---|
| **1** | **最轻量(in-proc)** | 用户已有 Claude session 内,ccteam 作 plugin/skill 用内置模板创建临时 teammate;native `Task` 起,跟随 session 同生灭 | 写代码时临时召唤帮手 / 几小时冲一波 3-5 agent 并行 |
| **2** | **bg 工作流编排** | `claude --bg --agent <role>` 多 session 协作,workflow.yaml 编排 trigger,文件 artifact 接力;ccteam Rust daemon 长跑,bg-job 短命 | 高级用户在某领域编排长跑多 agent workflow(qa-loop:test-fix-release / 自激励 build,几小时-几天)|
| **3** | **IM bot(tmux 常驻)** | tmux + `claude` TUI 多 session 24/7 常驻,每 session = 1 个 agent bot;bot ↔ bot 通过 IM group 互相 @ 交流 | 手机 IM 私聊 AI 助理 / IM 群多 bot 团队跨设备协作 |

底层 3 形态固定,**上层应用场景开放** — 下面 5 个 preset 是 V0.6 开箱;`ccteam-creator` skill 也支持 NL 描述新场景自动生成。

## 5 分钟上手(按形态选入口)

```bash
# 0. 先装 Claude Code + ccteam plugin
# https://code.claude.com/docs/install
claude
/plugin install ccteam

# === 形态 1:已有 session 内召唤临时帮手 ===
/ccteam "帮我把这个 TS 报错全清掉"
# 或多 agent 冲一波:
/ccteam-team 3 "重构 src/auth 子模块"

# === 形态 2:起 bg 长跑工作流 ===
/ccteam-creator "夜里跑 qa-loop:每次提交跑测试,失败自动 fix"

# === 形态 3:接 IM bot(V0.6 旗舰)===
/ccteam-im-setup                        # 一次性绑 TG/Slack/Discord
/ccteam-creator "做个 TG 私聊助理 bot,帮我管邮件"
```

→ 详 [quickstart](docs/quickstart.md)。

## 5 个开箱用法,任你挑

每个 preset = 一个推荐"形态 × 编排模式 × persona"配方,`ccteam-creator` 在 NL 对话中自动配:

| Preset | 一句话场景 | 怎么起 | 形态 |
|---|---|---|---|
| **Solo Sidekick** | Claude 写代码时临时召唤一个帮手 | `/ccteam <自然语言>` | 1 |
| **Team Sprint** | 几小时冲一波,3-5 agent 并行 | `/ccteam-team 3 "<task>"` | 1 |
| **Overnight Builder** | 丢任务睡觉去,长跑几小时到几天 | `/ccteam-creator "夜里跑 …"` | 2 |
| **Pocket Assistant** ⭐ | 手机 IM 私聊一个 AI 助理 | `/ccteam-creator "做个 TG 助理"` | 3 |
| **IM Squad** ⭐ | IM 群里多个 bot 互相 @ 协作 | `/ccteam-creator "做个 TG 多 bot 团队"` | 3 |

⭐ V0.6 旗舰场景。形态 3 把 AI 团队带进你的 IM,跨设备 24/7 替你干活 — 与 ChatGPT / Cursor / Devin 的差异:bot 跑在**你电脑上**,能动你的文件 / 跑你的命令 / 看你的代码,手机只是入口。

## 三种跟 ccteam 对话的方式

- 🟢 **Claude session 内**:`/ccteam <自然语言>` 总入口(形态 1/2/3 通用)
- 🟢 **IM 端**(形态 3):私聊 bot 或群里 `@ccteam <NL admin>`
- 🟡 **Web 仪表板**(形态 2/3):`http://localhost:7331`(只看不操作)

> 你**不需要**学一堆 CLI 命令,也**不需要**写任何 yaml 配置。所有 setup 都通过 Claude session 对话生成。

## 文档

| 想干什么 | 读这个 |
|---|---|
| 5 分钟跑通第一个 preset | [docs/quickstart.md](docs/quickstart.md) |
| 完整 3 形态 + 5 preset 使用手册 | [docs/user-manual.md](docs/user-manual.md) |
| 抄一份现成的 use case | [docs/recipes.md](docs/recipes.md) |
| 出错查不到 | [docs/troubleshooting.md](docs/troubleshooting.md) |
| 高级定制 / Codex 集成 | [docs/advanced/](docs/advanced/) |
| 看代码 / 改 ccteam | [docs/architecture/](docs/architecture/) |

## Status

**V0.6.0 in production**(2026-05-19 shipped,`git tag v0.6.0`)— Epic A 5 min 起第一个 IM bot · Epic B bot 在 IM 像真人助理 · Epic C 中文用户能用(铺底)。`HarnessAdapter` 5-method trait + tmux 长跑 mode 3 + Codex Option B + per-vendor budget caps + ccteam-imd daemon。baseline 1283/1 · clippy `-D warnings` clean · 4 wave handoff doc 全 land。详 [docs/v0-6-0/README.md](docs/v0-6-0/README.md) + [wave-4-handoff.md](docs/v0-6-0/wave-4-handoff.md)。

**V0.6.1 蓄势中** — F119 probe daemon-start / F120 overnight-builder full probe / F121 pricing version check / F122 mode-3 Codex bridge / F123 demo GIF 录制 / F98 plan-approval↔outbox / F124 HITL Approval Gating(详 wave-4-handoff §Remaining)。

## License

详 [LICENSE](LICENSE)。

## 致谢

- [Claude Code](https://code.claude.com/) — 运行时
- [openhuman/channels](https://github.com/openhuman/channels) — 14+ IM 平台 Rust 抽象
- Anthropic *Building Effective Agents* — 5 编排模式 taxonomy
- `ccgram` + `oh-my-claudecode` — 形态 3 IM bot 模式(tmux + send-keys + transcript polling)参考实施
