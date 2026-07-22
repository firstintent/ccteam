# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace **`0.9.7` 已合 `main`**(2026-07-22 owner merge PR #165,squash `825ae7d`)。V097 全三波:W1 daemon 生命周期核(Codex pid-detach)+ W2 systemd/launchd 退场 + W3 `ccteam update`/InstallChannel/版本外显;随车 2 个 ccm 提交(Grok 托管不 spawn stdio mcp-serve + idle 后台任务 /status 外显)。**tag+部署 = pre-release rc 先行人肉测**(release.yml 加 `--prerelease`(tag 带 `-` 即 pre-release,不进 /latest);已推 `v0.9.7-rc1` 触发工作流,owner `CCTEAM_VERSION=v0.9.7-rc1` 装测 → 满意后打 `v0.9.7` 正式 tag = latest)。
- **在做**:**V097 已发布(v0.9.7 正式 tag,latest)** —— 完结。A2A-W5 仍挂起;P1-1/2/3 + P2-1 待排(P1-4 D7 作废);V094 npm gated —— 队列 = `.loop/backlog.md`。
- **下一版**:v0.9.4(npm 分发)gated 等 owner 重启(PRD DRAFT 留 `docs-local/versions/v0-9-4/prd.md`)。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `cargo test --workspace --exclude ccteam-web --lib` = **1497/0**(v0.9.7:1472 +6 daemon 核心 +16 install_channel/version_check/preferences 定向 +3 随车 ccm 提交;W1+W2+W3 全 fake/tempdir 隔离)
- `ccteam-web` 全量 **0 失败**(本机通过数 321–325 随 tmux 环境波动,差值 = 已知 `ws_*` env-flake 族)· vitest **392**(SPA:−2 compare 按设计 +3 模板重设 +3 vendor 外显)· Playwright **7**(v0.9.6 未重跑,上轮口径)
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
| **tag + 部署** | **已消耗(v0.9.7 已发布)** —— owner 2026-07-22:「发release」→ pre-release `v0.9.7-rc1` 人肉测 →「发正式版」→ 正式 `v0.9.7` tag(commit `2922f7a`,= rc 同一构建)推送,工作流建 latest release 四平台;`/releases/latest` → v0.9.7,全体用户经 `install.sh`/`ccteam update` 可拿到。**此后常态回归「push main ≠ 发布」,下个版本 tag 仍需 owner 显式发话** |
| V097(v0.9.7 daemon 重构 + update)W0 拍板 | **已签核消耗** —— owner 2026-07-22「install.sh 检测 systemctl…你来调度进入开发,提交 dev,发 PR」;废 systemd/launchd 先期拍板 + D1–D8 按 PRD v4 默认全「是」消耗(**含 D2 `daemon stop --force` SIGKILL 例外,仅 daemon 自身,agent session 零碰**);merge PR #165 = owner 2026-07-22「已经合并」;`825ae7d` squash 落 main |
| v0.9.6 compare 契约删除(REST `/compare`×2 + IM `/compare` + web tab) | **已签核消耗** —— owner 2026-07-21 会话拍板「compare 去掉,改会话内编排」,落 dev(T4) |
| v0.9.6 docs 写权一次性授权(kimi 改 usage/orchestration/tech-design/README) | **已签核消耗** —— owner 2026-07-21 指定 kimi 更新全局文档、fable5 review;仅本版有效,写权常态仍归规划会话 |
| v0.9.6 合 main | **已签核消耗** —— owner 2026-07-21「review 后合并 main,让 dev 和 main 保持一致」;fable5 review 三提交(3e6bca1/9c5f895/86b9788)后 ff 合并 |
| AGENTS §三 init 布局红线行澄清(注明用户可选 `.ccteam/routing.md`,init 不种) | **已消耗** —— 随 owner ship 9c5f895 的语义校准,非新增红线 |
| v0.9.4 动代码 | gated —— owner 暂缓(2026-07-17,v0.9.5 先行;v0.9.5 已于同日完成落 main,授权已消耗) |
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- (空 —— 已固化的住 `.loop/verify/README.md`「运行纪律」+ AGENTS.md §六;新教训先记这里或卡面「经验」行,蒸馏后移走)

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh`(队列结构校验)→ commit(英文)→ push `main`。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
