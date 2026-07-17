# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace `0.9.2`;v0.9.0–0.9.2(A2A 底座)+ v0.9.3 增量(四 vendor MCP 对称注册,未单独 bump)已落 `main`,**未 tag、未部署**。
- **在做**:A2A-W5(三场景真机 smoke + README/usage 重写)——卡在 `.loop/backlog.md`。
- **下一版**:v0.9.4(npm 分发 / daemon 管理 / 自更新)PRD DRAFT 待 owner 拍板(`docs-local/versions/v0-9-4/prd.md`)。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `cargo test --workspace --exclude ccteam-web --lib` = **1408/0**
- `ccteam-web` 全量 **310/0** · vitest **376**(SPA)· Playwright **7**
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
| **tag + 部署** | **HELD** —— push 到 `main` 不等于发布,等 owner 显式「部署」 |
| v0.9.4 动代码 | gated —— 等 owner 拍板 PRD |
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- (空 —— 已固化的住 `.loop/verify/README.md`「运行纪律」+ AGENTS.md §六;新教训先记这里或卡面「经验」行,蒸馏后移走)

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh <开工 base sha>` → commit(英文)→ push `main`。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
