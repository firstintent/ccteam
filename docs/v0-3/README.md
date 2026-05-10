# V0.3 文档索引

V0.3 是首个开「对外可视层」的 ccteam 主线版本 — 一个本地 / 局域网 web UI,
展示全局项目 / agent / subagent 状态(progress.jsonl 事件流 + 项目详情 +
F38 截图),并提供有限的写动作(`/btw` / inject_decision / pause / resume)。
新 crate `crates/ccteam-web`,axum + askama + htmx + SSE 单 binary,无 npm /
无 build toolchain。

**状态**:**已 ship**(2026-05-10,5 PR M5.0-M5.4 全 merge;
workspace.version `0.3.0`;测试 738/0)。

base 起点 = `origin/main` `2988de6`(V0.2.2 F44 ship 终点,测试 631);ship
终点 = V0.3 PR #5 merge 后(测试 738,+107 测试 / V0.3 自身贡献)。
ship 报告:[`e2e-retro.md`](e2e-retro.md)。

## 文档清单

| 文件 | 内容 | 何时读 |
|---|---|---|
| [`prd.md`](prd.md) | V0.3 PRD — 12 节,5 milestone(M5.0-M5.4)设计 + 技术决策 + 威胁模型 + PR sequencing | V0.3 设计意图源头 |
| [`dev-plan.md`](dev-plan.md) | 5 PR milestone 拆解 + worktree 分支 + subagent briefing 模板 + 红线 grep 矩阵 | V0.3 实施 |
| [`e2e-retro.md`](e2e-retro.md) | V0.3 ship gate 端到端验证矩阵(22 场景全 PASS)+ 跨浏览器 spot-check + lingering issues + V0.4 deferred | V0.3 ship 报告 |

## Milestones 速查

| M | 范围 | 优先级 | PR # | 独立可 ship |
|---|---|---|---|---|
| M5.0 scaffold + write helper promote | crate 基础设施 + 解耦 | P0(立 dep 图) | 1 | 否(基础) |
| M5.1 read-only dashboard | 项目列表 + 详情页 | P0(用户首批 dogfood)| 2 | **是** |
| M5.2 SSE event push + 按需截图 | 实时流 + F38 reuse | P0 | 3 | **是** |
| M5.3 写动作 + token 鉴权 | /btw / inject_decision / pause / resume + 默认 token | P0 + 安全 critical | 4 | 否(依赖 #2 模板)|
| M5.4 e2e + retro + ship gate | workspace.version 0.2.2 → 0.3.0 + retro | P0 | 5 | 否(ship gate)|

## 关键设计决策

详 `prd.md §8` 技术决策汇总:

- **新 crate `crates/ccteam-web`**:依赖 ccteam-core only,**不依赖 ccteam-cli**
  (避免 binary-as-library dep 倒挂)
- **写动作 helper promote**:`send_to_session` / `inject_decision` / `pause` /
  `resume` 从 `ccteam-cli::mcp_serve` 私有 fn 提到 `ccteam-core::actions`
  pub fn(M5.0 关键解耦)
- **SSE 拓扑**:两个端点(`/sse/all` 全局 + `/sse/project/<slug>` per-slug),
  不在一条连接 multiplex
- **截图按需 GET**,**不 polling**(F38 单次渲染百 ms 级,polling 烧 CPU)
- **默认 token 鉴权**(非 loopback bind 自动开;loopback 不强 token);
  `--no-auth` 显式 opt-out + 大字 stderr 警告 + 5s 倒计时
- **CSRF 防御 = `Authorization` header 本身**(浏览器跨域 form-submit 不自动加)

## 安全:V0.3 主要风险

详 `prd.md §9` 威胁模型:

- ccteam 项目 session 跑 `--dangerously-skip-permissions` claude
- web 写动作(`/btw` / `inject_decision`)是 unsanitized prompt-injection 向量
- 组合「非 loopback bind + 无鉴权 + 写动作」= **LAN-wide remote code execution**
- 缓解:**默认开 token 鉴权**,用户可 `--no-auth` 显式 opt-out(stderr 大字警告)
- 用户原偏好「暂不考虑安全」改为 `--no-auth` 显式选项;PRD review 时是用户
  显式决策点(接受默认 / 推翻成无鉴权默认)

V0.4 deferred:OAuth / TLS / per-project ACL / 多用户 / 隧道集成。

## 跟其他文档关系

- 主仓 `CLAUDE.md` §一(baseline)/ §三 红线(ccteam-web 不 dep ccteam-cli;
  progress.jsonl SoT;不解析 tmux 输出)/ §六(易踩坑)— 实施 M5.0 / M5.4 落
- `docs/tech-design.md` §3.8 用户接口层(加 web layer)+ §6.4 channel layer
  (web 是新 channel)— PR #2-#5 增量补
- `docs/interfaces.md` §13(新章节 web routes + SSE wire + token 协议)— PR
  #2-#5 增量补
- `docs/dev-coupling-audit.md` F45(M5.0 write helper promote)— PR #1 加,
  PR #4 / #5 close
- `docs/v0-2/README.md` 加 V0.3 起始 pointer(本 doc-only PR 落)
- `docs/requirements.md` 痛点 7「进度永远不透明」+ 痛点 9「AI 团队需要人来主持」
  (部分)— PRD §1.1 映射

## 配套(M5.4 落)

- Cargo workspace.package.version `"0.2.2"` → `"0.3.0"`
- CLAUDE.md §一 baseline 表格回填(V0.3 milestone 行 + 测试数实测)
- `docs/v0-3/e2e-retro.md` 落档(M5.4 后,V0.2.2 e2e-retro 模板)
