# ccteam

> **在 Claude session 里一句话召唤 AI 团队 — 接进你的 IM,跨设备 24/7 替你干活。**

![demo](docs/v0-6-0/demos/30s-tg-bot-team.gif)

> _GIF wave 2 ship 后填_

## 5 分钟上手

```bash
# 0. 先装 Claude Code:https://code.claude.com/docs/install
# 1. 起 Claude session
claude
# 2. 在 session 里装 ccteam plugin
/plugin install ccteam
# 3. 一次性绑你的 IM(TG / Slack / Discord 任选)
/ccteam-im-setup
# 4. 起第一个 AI 助理(自然语言对话起 bot)
/ccteam-creator "做个 TG 私聊助理 bot,帮我管邮件"
```

→ 详 [quickstart](docs/quickstart.md)(5 步收到 bot 第一条 IM 回话)。

## 5 种用法,任你挑

| 用法 | 一句话场景 | 怎么起 |
|---|---|---|
| **Solo Sidekick** | Claude 写代码时临时召唤一个帮手 | `/ccteam <自然语言>` |
| **Team Sprint** | 几小时冲一波,3-5 agent 并行 | `/ccteam-team 3 "<task>"` |
| **Overnight Builder** | 丢任务睡觉去,长跑几小时到几天 | `/ccteam-creator "夜里跑 …"` |
| **Pocket Assistant** ⭐ | 手机 IM 私聊一个 AI 助理 | `/ccteam-creator "做个 TG 助理"` |
| **IM Squad** | IM 群里多个 bot 互相 @ 协作 | `/ccteam-creator "做个 TG 多 bot 团队"` |

⭐ **V0.6 旗舰场景。Pocket Assistant + IM Squad 让 AI 团队跨进你的 IM**,跨设备 24/7 替你干活 — 这是 ccteam 跟 ChatGPT / Cursor / Devin 的关键差异:bot 跑在**你电脑上**,能动你的文件 / 跑你的命令 / 看你的代码,手机只是入口。

## 三种跟 ccteam 对话的方式

- 🟢 **Claude session 内**:`/ccteam <自然语言>` 总入口(发啥都行)
- 🟢 **IM 端**:私聊 bot 或群里 `@ccteam <NL admin>`
- 🟡 **Web 仪表板**:`http://localhost:7331`(只看不操作)

> 你**不需要**学一堆 CLI 命令,也**不需要**写任何 yaml 配置。所有 setup 都通过 Claude session 对话生成。

## 文档

| 想干什么 | 读这个 |
|---|---|
| 5 分钟跑通第一个 bot | [docs/quickstart.md](docs/quickstart.md) |
| 完整 5 种 preset 使用手册 | [docs/user-manual.md](docs/user-manual.md) |
| 抄一份现成的 use case | [docs/recipes.md](docs/recipes.md) |
| 出错查不到 | [docs/troubleshooting.md](docs/troubleshooting.md) |
| 高级定制 / Codex 集成 | [docs/advanced/](docs/advanced/) |
| 看代码 / 改 ccteam | [docs/architecture/](docs/architecture/) |

## Status

**V0.6.0 蓄势中**(2026-05 立项)— Epic A 5 min 起第一个 IM bot · Epic B bot 在 IM 像真人助理 · Epic C 中文用户能用(铺底)。详 [docs/v0-6-0/README.md](docs/v0-6-0/README.md)。

**V0.5.1 in production**(2026-05 ship)— 升级 V0.6 **0 用户操作**:工具名 / skill 入口 / workflow.yaml 全兼容。

## License

详 [LICENSE](LICENSE)。

## 致谢

- [Claude Code](https://code.claude.com/) — 运行时
- [openhuman/channels](https://github.com/openhuman/channels) — 14+ IM 平台 Rust 抽象
- Anthropic *Building Effective Agents* — 5 编排模式 taxonomy
