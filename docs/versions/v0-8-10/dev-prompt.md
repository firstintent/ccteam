# v0.8.10 dev-prompt — 核心流程生产级(STABILITY + 高质量 UX)施工书

> 给**实现 v0.8.10 的 dev session**。**spec 权威 = `docs/versions/v0-8-10/prd.md`**(D1–D9 双轴);本文只做**编排**(phase 序列 + 工作方式 + 诚实门 + 完成定义),不重述 spec。本文作者只写文档、不开发;你(dev session)实现。
> PRD 已经过一轮对抗式 review(`prd-review.html` 是可视化审阅),里面的 `纠 audit` 注解就是上一轮被修正的过度声明 —— **照 PRD 现稿做,别回退到更早的 D1–D7 版本**。

---

## 0. 起手必读

读 `prd.md` 全文,死记这五处(它们是你的"完成"与"边界"的唯一权威):
- **§七 验收 rubric(A1–A5 + B1–B6)= 完成定义**。
- **§九 OUT 表 + 集合-不增长 guard**(+ 仅 §六.0 承认的 **2 个 micro-exception**:模型 warn、每-session 活动态只读 label)。
- **§十 phase 编排建议**(D1 自成一 phase 先建;D6 宜与 D1 同期/更早;UX 不开平行 track,盖在 D4/D5/D6 上)。
- **§八 基线**(实测 1907/0 @ `9480ecc`;CLAUDE.md §一 记的 1898 过期,本版顺手对齐)。
- **§六.0 IN/OUT 裁决表**(每个 UX 条目当场跑铁律 sharp test)。

**铁律(prd §铁律)**:本版**只硬化 + 打磨已存在 surface,零新增用户可达能力**。每个改动跑一遍 sharp test 「本版后,用户有没有一件以前做不到、现在能做的新事?」 **有 → OUT(出局),除非是 §六.0 的两个承认例外。**

---

## 1. 工作方式(与 v0.8.8 / v0.8.9 一致)

- 用 **Workflow 编排**,subagent 一律 **opus**。
- 在 **`dev` 分支**开发;**每个 Phase 完成即 `commit` + `push`**(不开 PR)。push 让进度扛得住 **host-suspend / runner-death**(PRD §十 已点名这个 hazard,v0.8.8 实战吃过亏)。
- 每 Phase 一份 `docs/versions/v0-8-10/wave-N-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段)。
- **每 Phase 验收门**:`cargo test --workspace --exclude ccteam-web` ≥ 本 Phase 起跑实测 pass 数 · `ccteam-web` 测试不退 · `vitest` 不退 · `cargo clippy --workspace --all-targets -- -D warnings` 0 warning · `cargo fmt --all -- --check` 过 · `npm run lint` 0 warning。**不过门不进下一 Phase、不发 PR。**

---

## 2. Phase 0 —— 重基线 + 重锚点(必做第一步,PRD §八 强制)

dev 在**并行 commit**(另有 session 在推 v0.8.9 fix / v0.8.10 PRD),行号会漂。起手必须:
1. `git rev-parse origin/dev` + `git pull` 到最新。
2. 重测基线:`cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` → 记下你的起跑 pass 数(PRD §八 实测 1907/0,以**你重测**为准)。
3. 重 grep PRD 里**全部** `file.rs::symbol` 锚点(§二/§四/§六/§九),确认仍在、更新行号。
4. 重列 **D7** 当前**仍未修**的核心环路 bug —— `bug1/2/3/5/6` 已在 dev 修(见 §五,**不重复 scope**),只收"重 grep 后仍开的"。
5. 产出 = 当前锚点表 + 起跑基线数 + 残留 backlog,写进 `wave-0-handoff.md`。

---

## 3. Phase 序列(建议,依 PRD §十;PRD §七/§九/§十 是权威,phase 边界你可微调,只要门 + 完成定义不破)

- **P1 · D1 soak harness + D6 通知可靠**(§十:D1 先建、D6 同期 → 先拿到确定性基线)。
  - D1:**复用既有 8 个 harness**(`claude_tui_reattach_test.rs` / `claude_tui_resume_test.rs` / `rmux_backend_reconnect.rs` / `rmux_backend_session_roundtrip.rs` / `codex_app_server_test.rs` / `smoke_rmux_sdk.rs` …)**扩展**断言,别 greenfield;**故障注入两层**:① **[CI-fake,确定性,硬门]** kill-daemon-mid-turn / 杀 pane(Claude)/ 杀 `codex exec`+`mcp.sock`(Codex)/ 杀 app-server socket / 空闲释放再唤醒 / 断网(`CCTEAM_*_BIN` fake + monotonic-clock fake);② **[real-machine,best-effort]** host-suspend + 长跑(搭脚本,见 §4)。
  - D6:**先 stress 跑出精确 flake 测试名**,判定它是**产品 reliability 缺陷**(→ 修产品)还是**测试计时 race**(→ 修测试确定性)—— 两情形都 in-scope,**不预设是产品洞**;修 outbound-ledger flake(= §八 那个间歇 fail=1)+ file-send registry gap,并发下测;并入"同 cwd ≥2 session + Task 子 agent 时回复只投自己 chat"。**0-flake 是 D6 出口门,不是 P1 之前每 phase 的前置**(PRD §七 A5)。

- **P2 · D2 / D3 / D4 收三类病根**(§七 A3 named guard):
  - **D2** backend-literal guard:`tmux` 字面只许白名单文件(`tmux_backend/` + `tmux_ops.rs` + `core/src/tmux.rs` + `default_backend` selection + `*/tests/*`),PATH 注入假 tmux → 白名单外被调即 panic;覆盖 `capture_pane`/peek 路径。
  - **D3** session 身份:`events/tail_loop/marker/cursor` 四处 key 对 roleless 取 sid + `chat_session_reset*` 三 builder 事件带 sid + 同 role 多 session/同 cwd 多 session 不串;transcript 发现**优先靠 ccteam 自己的 active-session marker、不加深 Anthropic transcript schema 耦合**(守 R6)。
  - **D4** 单一 **file-backed stall SoT**:watchdog 判定落 `progress.jsonl`,CLI `stall_verdict` 与 web Status 读**同一真相 + 同一 `ccteam-core` 分类函数**(同 fixture 喂两侧断相等);**不造新 RPC / 新 config key / 新采集端点**。

- **P3 · D5 边界可靠性**:§五.x 边界表每条给 **{timeout(< 播报截止), retry+backoff, 幂等键}**;**D5 矩阵 4 项**(≥2 sid restart 重挂 / recreate 失败诚实 reset 带 sid / ledger 重放幂等 at-least-once 不变 at-least-twice / 入站断网不丢不重)绿,并入 D1 CI-fake 注入。

- **P4 · D8 / D9 UX**(盖在 D4/D5/D6 surface 上,**不开平行 track**):
  - **D8** 失败分级表(§六.1)每个 user-facing 失败态 → **每通道(IM / active SSE)恰好一条**人话(中文 + 现象 + 下一步)信号;**每个 new-emit 行带反假阳测试**(健康但安静 session N 秒内不误报);种子复用 `claude_tui.rs::MarkerSilenceWatch`(F187 WARN),别新发明检测器。
  - **D9** ① 上手(`init` 'next:' 块 + `cto_role.md` fresh-user 节 + usage 对齐,三处一致;唯一规范步骤序列见 §七 B3)② 模型 warn(**新建** `is_claude_family` 前缀匹配,`vendor==Claude ∧ ¬is_claude_family` 在 spawn 路径 emit warn-once;**绝不复用** `pricing.rs::warn_unknown_model_once`)③ 可观测性(StatusView 补"working/疑似卡/最近活动 X 前"**只读 label**,与 CLI 读同一 SoT)④ 错误文案(**核心环路 only**,正则白名单 next-step ∧ 黑名单 jargon/stack-trace;eslint 3 warning 用**真依赖**修非抑制)。

- **P5 · D7 死代码 + 残留 backlog + docs/version**:
  - 按 **§九 死代码处置表**逐项删 —— **每行带真实 caller 集 + 删除验收(`cargo build --workspace` clean 且 `cargo test` ≥ 基线)**;**注意已标注的 3 处 P0 误判**(`render/write_project_settings_agent_team` 有 live test caller、`CHAT_BOT_MARKER_STUCK` 在 live match arm、`marker_reporter` 是外科式删-保留 `silence.observe`);`--restart-team`(ccteam-flow,推后)本版不动。
  - 清 Phase 0 重列的残留核心环路 bug。
  - 版本 bump `0.8.9 → 0.8.10`(workspace `Cargo.toml`)+ CLAUDE.md §一 baseline 对齐 **1907/0** + `docs/tech-design.md`(刻入 §四.z 的空项目 scaffold carve-out,防误删)+ `README.md`(英文、不含版本进展)+ `docs/usage.md`(supported-model matrix + 上手序列)+ `docs/versions/v0-8-10/` 归档 + 最终 handoff。

---

## 4. 诚实门(关键 —— 不许在沙盒里冒充真机绿)

- **CI-fake 切片 = 你能交付的硬门**(确定性,进 CI,**必须全绿**)。
- **nas-box005 真机短 smoke(≤1h:一轮 restart + suspend + netdrop,默认 rmux × Claude)= 另一条硬门,但沙盒跑不了**(WSL2 / inotify-busy 跑不出 marker rendezvous / transcript 震荡类 bug)→ 你把它做成**可一键运行的脚本 + no-silent-failure checklist**,标「**nas-box005 待跑**」;**绝不**拿 fake 绿冒充真机绿、**绝不**在没跑真机短 smoke 时宣布 tag-ready。
- **真机长跑(M≥50 循环 / 真 24h)+ Codex 真机长跑 = best-effort,非 tag-blocker**(PRD §七 / Codex 是架构 best-effort tier)。**市场 install 真机验证 = best-effort,blocked-on ccteam-hub 公开**(私库无法经 HTTPS github-raw 拉;hub 未公开则该项不阻 tag,显式标注、不静默当 IN);**终端真机验证 = IN**。

---

## 5. 完成定义 + 砍序

- **完成 = PRD §七 A1–A5 全绿 且 B1–B6 全过(CI-fake 部分)+ 真机短 smoke 脚本就绪待跑**;真机长跑 / Codex 长跑 / 市场-install 真机 = best-effort 标注。
- **timebox 砍序(PRD §十,host-suspend 吃时间时)**:**第一个砍 = 纯视觉末端打磨**(§六.5 终端/市场/cost-pill 视觉);**最后才砍 = D8「失败可见」+ D1 CI-fake 硬门**(它们是 reliability 戴 UX 帽子,不是 cosmetic)。
- **一口气做完 P0 → P5 中途不要停。** 完成回报:各 Phase 起跑基线 / CI-fake soak 结果 / **专机(nas-box005)待跑项清单** / 版本号 / §九 删除项的 build+test 验收结果。

---

## 6. 启动 /goal(贴给 dev session)

```
/goal 实现 ccteam v0.8.10「核心流程生产级:STABILITY + 高质量 UX」。

先读透 docs/versions/v0-8-10/prd.md(spec 权威,D1–D9 双轴 + §七 验收 rubric + §九 OUT-gate)
与 docs/versions/v0-8-10/dev-prompt.md(施工编排:Phase 0–5),严格按 dev-prompt 执行。

四条最容易踩的纪律,务必守:
① 只硬化/打磨已有 surface、零新增用户可达能力(仅 prd §六.0 两个承认例外);每个改动跑
   sharp test「用户有没有一件以前做不到、现在能做的新事」,有=OUT。
② Phase 0 先 git rev-parse origin/dev + pull + 重测基线 + 重 grep prd 全部 file.rs::symbol
   锚点(dev 并行 commit、行号会漂)+ 重列 D7 残留 bug(bug1/2/3/5/6 已在 dev 修、不重复)。
③ 诚实门:CI-fake 切片必须全绿(你能交付的硬门);nas-box005 真机短 smoke 沙盒跑不了 → 做成
   可一键脚本+checklist 标「专机待跑」,绝不拿 fake 绿冒充、绝不在没跑真机 smoke 时宣布
   tag-ready;真机长跑/Codex/市场-install = best-effort 非 blocker。
④ Workflow 编排 + subagent 一律 opus;dev 分支、每个 Phase 完成即 commit+push(不开 PR,抗
   host-suspend);每 Phase 门 = cargo test ≥ 起跑实测(prd §八 1907/0)+ clippy 0 + fmt +
   eslint 0,不过不进下一 Phase。

一口气做完 Phase 0→5 中途不要停;完成 = prd §七 A1–A5 全绿 且 B1–B6 全过(CI-fake 部分)
+ 真机脚本就绪;回报各 Phase 基线 / CI-fake soak 结果 / 专机待跑清单 / 版本号。
```
