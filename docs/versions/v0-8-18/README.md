# v0.8.18 PRD — 环境驾驶舱(多 agent 装机/健康检测面)

> 状态:**讨论稿(doc-first,代码未动)**。先看原型,再定方案。
> 原型:[`prototype/environment-cockpit.html`](prototype/environment-cockpit.html)(浏览器直接打开,可点:复制 / 一键注册 / 重新探测 / 切屏)。

---

## 〇、一句话

把「N 个本地 agent CLI(claude / codex / …)各自装没装好、登没登录、连没连上 ccteam」从**逐个进终端查**,收成 web 上一张 **`环境体检`** 面板 + 一个 `GET /api/v1/environment`。

这是「**ccteam = 多 agent 驾驶舱**」论题的**第一条腿(A · Day-0 装机/健康)**。不是孤立 feature。

---

## 一、为什么(痛点映射 → `docs/requirements.md`)

OpenRouter 解决「太多 LLM provider,各有 key/SDK/定价 → 给我一个」。agent-CLI 世界的等价痛点不是「太多可选」(多数人就 1–2 个),而是:

- 每个 agent CLI 是**终端绑定**的:装机、登录、注册 MCP、模型怪癖,全在终端逐个手搓。
- ccteam 已经替你解决了「**从任何地方驱动**」(IM/web 网关)和「**可恢复**」(持久 sid + 双 SoT),但**装好 / 看见健康**这一层还在终端里。
- 从手机想确认「我那台云机上 codex 到底连上没」—— 现在做不到。

**护城河对齐**(见 memory `ccteam-moat-is-the-shell-not-features`):ccteam 的护城河是**结构位置**(云端终端壳),不是聪明逻辑。本版只把**已有信息**(配置路径、PATH 探测、doctor 检查)**汇成一个面**,不引入任何 vendor 会自己吃掉的智能逻辑。

### OpenRouter 类比的边界(写进 PRD 免得跑偏)

| | OpenRouter | ccteam |
|---|---|---|
| 坐在钱/API 路径上 | 是(代理每次调用) | **否**(驱动本地 CLI,各自向 vendor 登录,ccteam 不碰钱) |
| 能做的 | 统一 key/账单/路由/计量 | **统一驾驶舱**:装好→看见→驱动→盯住 |

→ ccteam 做 OpenRouter 的 **Activity/Credits/Keys 面板那一半,减去 proxy**。**跨 vendor 计费路由器对 ccteam 结构性不可能,本版及以后都不追。**

---

## 二、设计(以原型为准)

### 2.1 后端:`GET /api/v1/environment`(web-token 门后)

把 `crates/ccteam-web/src/routes/capabilities.rs`(现在只探 `--version` 二态、写死 claude/codex)升级成真正的环境报告。形状:

```jsonc
{
  "daemon": { "version":"0.8.17","uptime_s":15120,"port":7331,
              "home_drift":false,"mcp_tools":{"total":15,"stub":0},
              "sessions":{"total":3,"live":2},"cost_today_usd":4.21,"budget_usd":20.0 },
  "vendors": [
    { "id":"claude-code","vendor":"claude","status":"ready",
      "bin":{"present":true,"path":"~/.local/bin/claude","version":"2.1.185"},
      "auth":{"ok":true,"detail":"oauth"},
      "mcp":{"registered":true},
      "settings":{"hook_block":true},
      "default_model":"claude-opus-4-8[1m]","effort":"max",
      "fixes":[] },
    { "id":"codex","vendor":"codex","status":"needs_config",
      "bin":{"present":true,"path":"~/.local/bin/codex","version":"0.42"},
      "auth":{"ok":false}, "mcp":{"registered":false},
      "fixes":[
        {"label":"登录 Codex","cmd":"codex login","kind":"manual"},
        {"label":"注册 ccteam MCP","cmd":"ccteam config","kind":"ccteam_footprint","action":"register_mcp"}
      ] },
    { "id":"gemini","vendor":"gemini","status":"not_installed",
      "bin":{"present":false},
      "fixes":[{"label":"安装 Gemini CLI","cmd":"npm i -g @google/gemini-cli","kind":"manual"}] }
  ]
}
```

- `status` ∈ `ready | needs_config | not_installed | broken`(对应原型四个状态点 绿/黄/灰/红)。
- **`fixes[].kind` 编码了红线**:`manual` = 只给命令、前端复制;`ccteam_footprint` = ccteam 自己的足迹,**允许**一个写端点执行。

### 2.2 写端点(唯一一个):`POST /api/v1/environment/register-mcp`

body `{ "vendor": "codex" }` → 调现成的 `mcp_serve::install_mcp` / `install_codex_mcp`(就是 `ccteam config` 跑的那段,幂等)。**这是从 web 唯一能写的东西。** 复用现有逻辑,零新执行面。

### 2.3 前端:底部导航加 `环境`(可能再加 `舰队` 预览)

挂在现有 Status/Settings 旁。每 vendor 一卡(原型已画全):状态点 + 标题/path/version + 勾叉清单(登录/MCP/hook)+ 缺啥给可复制命令 + daemon 卡。右上 `↻ 探测` 手动刷新(破现在 daemon-终身 cache)。

### 2.4 vendor 可扩展

把写死的 claude/codex 两条改成 `AgentVendor` enum + 每 vendor 一个 `ProbeSpec`(bin 名、version 参数、auth 探测怎么做、配置文件路径)。加 gemini/grok = 填一条数据,不改路由代码。

---

## 三、范围切口(这是判断,不是选项)

| | 做 | 不做 |
|---|---|---|
| 检测 | 装/登录/MCP/hook/version/home-drift,只读汇总 | — |
| 写 | **仅** ccteam 自身足迹(一键注册 MCP) | ❌ 从 web 写 vendor 身份(登录 / API key) |
| 安装 | 给可复制命令 | ❌ 从 web 跑 install 脚本(包管理器/执行面) |
| cache | 加手动 re-probe | — |
| 路由 | — | ❌ 跨 vendor 路由/fallback(prompt 层,`pk` skill,不进 Rust) |
| 舰队 | 原型放预览定方向 | ❌ 真做(留下个 minor,B 腿) |

**红线**(与 CLAUDE.md §三一致):①「ccteam executes nothing」—— 除自身 MCP 注册外不执行任何 vendor 命令;②「绝不碰 `settings.json`」—— hook 检测只读,任何写仍走 `settings.local.json`;③ No-prompt-injection 不受影响(本版不碰 spawn 路径)。

---

## 四、验收

1. `GET /api/v1/environment` 在 claude 装好/codex 没装的机器上,返回 `claude=ready` + `codex=not_installed`,带 version 串。
2. `POST …/register-mcp {vendor:codex}` 幂等:codex 装了但没注册 → 注册后 `mcp.registered=true`;重复调不报错。
3. 探测用确定性 fake(`CCTEAM_CLAUDE_BIN`/`CCTEAM_CODEX_BIN` 指向假脚本)→ 不依赖真实 binary。
4. web 面板:四态(就绪/需配置/未安装/故障)渲染正确;复制按钮出命令;一键注册翻 ✗→✓;↻ 重新探测不重启 daemon。
5. baseline 不退:`cargo test --workspace --exclude ccteam-web` ≥ 现状;clippy 0 warning;`cargo fmt --all` clean;vitest/Playwright 不退。

---

## 五、落地节奏(若拍板)

doc-first(本文)→ 你 review 原型 + PRD → 一个 minor(v0.8.18),约两步:① 后端 `environment` 路由 + register-mcp 写端点(扩 `capabilities.rs`);② SPA 面板 + 底部导航项。直接在 dev 落、按 memory `ship-flow` 不发 tag。

**本轮只讨论 + 出原型/PRD,代码不碰。**
