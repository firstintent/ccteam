# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace `0.9.6` 已落 **`dev` 分支**(owner 直驱 2026-07-21:多 vendor 编排发现面 T1/T2 + compare 全链退役 T4 + 全局文档 T3 + 吉祥物 logo;设计 SoT = gitignored `docs-local/notes/orchestrate-final.md`;opus/codex/kimi 三 agent 分工,fable5 规划 review 合并),**未合 main、未 tag、未部署**;`0.9.5` 仍是 main HEAD 版本。
- **在做**:A2A-W5(三场景真机 smoke + README/usage 重写 —— **注意:usage/orchestration 已在 dev 按 compare 退役后语义改写,W5 勿按旧语义重写**);P1-1(缩范围)/2/3 + P1-4/P2-1 待排 —— 队列 = `.loop/backlog.md`。
- **下一版**:v0.9.4(npm 分发)gated 等 owner 重启(PRD DRAFT 留 `docs-local/versions/v0-9-4/prd.md`)。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `cargo test --workspace --exclude ccteam-web --lib` = **1450/0**(8f4654e +2:TG picker 对齐/roleless 无 `· —` 占位;前 a646d45 +1 `stopped`/`killed` 终态回归)
- **`dev` 分支(v0.9.6)口径** = lib **1469/0**(1450 −9 compare/model 按设计删 +16 T1 面板 +12 T2 目录)· web **0 失败**(本机通过数 321–325 随 tmux 环境波动,差值 = 已知 `ws_*` env-flake 族)· vitest **389**(−2 compare 按设计)—— 合 main 时以此为新基线
- `ccteam-web` 全量 **320**(本机 317 过 + 3 `ws_*` env-flake,干净环境应全绿;+4 sessions_api e2e +2 lib)· vitest **388**(SPA,+10)· Playwright **7**
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
| **tag + 部署** | **HELD** —— push 到 `main` 不等于发布,等 owner 显式「部署」 |
| v0.9.6 compare 契约删除(REST `/compare`×2 + IM `/compare` + web tab) | **已签核消耗** —— owner 2026-07-21 会话拍板「compare 去掉,改会话内编排」,落 dev(T4) |
| v0.9.6 docs 写权一次性授权(kimi 改 usage/orchestration/tech-design/README) | **已签核消耗** —— owner 2026-07-21 指定 kimi 更新全局文档、fable5 review;仅本版有效,写权常态仍归规划会话 |
| v0.9.6 合 main | 待 owner 指示(当前只落 `dev`) |
| v0.9.4 动代码 | gated —— owner 暂缓(2026-07-17,v0.9.5 先行;v0.9.5 已于同日完成落 main,授权已消耗) |
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- (空 —— 已固化的住 `.loop/verify/README.md`「运行纪律」+ AGENTS.md §六;新教训先记这里或卡面「经验」行,蒸馏后移走)

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh`(队列结构校验)→ commit(英文)→ push `main`。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
