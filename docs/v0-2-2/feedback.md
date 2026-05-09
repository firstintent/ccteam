# V0.2.2 用户反馈原始记录

> 本文件保存 2026-05-08 用户首批 ccteam 实战反馈原文,作为 PRD §1 的引用源。
> PRD 把这些 issue 归类到 4 个 finding(F34-F37);本文件不做归类,只留原始视角。

---

## 1. 2026-05-08 — slug 命名草率

- **项目**: `dev-dex-ai`(本应为 `hermestrade-home`)
- **问题**:slug 由 `ccteam new` 自动生成,未参考用户确认的品牌名 HermesTrade
- **原因**:meta-agent 在收到品牌名之前就已派单,生成 slug 时仅有 brief 摘要 "预测市场+DEX" → `dev-dex-ai`
- **改进方向**:
  1. 派单前先确认项目名称 / slug
  2. 支持 slug 重命名(ccteam 目前不支持)
  3. 或在收到补充信息后重建项目

## 2. 2026-05-08 — slug 问题 #2

- **项目**: `dev-ccteam-ui-ccteam-1-2-session-subagent-3`(本应为 `ccteam-ui`)
- 用户明确说了项目名 "ccteam-ui",但 `ccteam new` 仍从 brief 全文生成了冗长 slug
- **结论**:需要支持 `--slug` 参数,或派单前让用户确认 slug

## 3. 2026-05-08 — API tool call hang 导致 auto-loop 无法恢复

- **项目**: `dev-dex-ai`
- **现象**:进入 `test-author` 后,`Read(code-review.md)` 的 PreToolUse 发出了但 PostToolUse 再也没回来
- **影响**:turn 没收尾 → 无 Stop 事件 → auto-loop 停在 iteration 1,永远不会重试
- **根因**:DeepSeek API 调用未返回,Claude 进程卡在 mid-tool-call
- **改进方向**:
  1. orchestrator 加 tool-call 超时检测(N 秒无 PostToolUse → capture-pane 检查 → escalate)
  2. auto-loop 不应只依赖 Stop 事件,应加时间维度的兜底(N 分钟无任何 progress 事件 → 强制重注入)
  3. 考虑给 Claude 进程加 `--timeout` 或 API 调用超时

## 4. 2026-05-08 — `/btw` send-keys 路由到 subagent

- **项目**: `dev-ccteam-ui`
- **现象**:`/btw` 注入 `test-author` prompt 时,`code-reviewer` subagent 仍在活跃,tmux send-keys 把 prompt 敲进了 subagent 的上下文
- **影响**:subagent 无工具权限,无法执行 test-author;主 agent 从未收到 prompt;同样无 Stop → auto-loop 卡死
- **根因**:send-keys 注入前没有检查当前是否有活跃 subagent,也没有确保 prompt 送到主 agent
- **改进方向**:
  1. 注入前检查是否有活跃 subagent,有则等待或先发 Ctrl+C
  2. 或改用 inbox 消息机制(让主 agent 主动读取)代替 tmux send-keys
  3. 注入后做送达确认(短时间内检查是否有相关 progress 事件)

## 5. 2026-05-08 — auto-loop 过度依赖 Stop 事件

- **关联**:以上两个 bug 的共同放大因素
- **现象**:auto-loop 仅监听 Stop 事件触发重注入,但在 mid-tool-call hang 和 prompt 路由错误场景下,永远不会产生 Stop
- **根因**:`auto_loop.rs::decide()` 的输入是 `last_assistant_text`,这个数据仅来自 Stop 事件的产出
- **改进方向**:
  1. auto-loop 加兜底定时器:N 分钟无 progress 事件 → 强制 capture-pane → 判断是否需要 re-inject
  2. 或把 progress 事件以外的信号(tool call 超时、subagent 异常退出)也纳入 auto-loop 的触发条件

## 6. 2026-05-08 — meta-agent 绕开 pipeline 自行调研

- **触发**:用户要求调研 Multica 项目
- **问题**:meta-agent 没有按 CLAUDE.md §2 走 product-research 团队,而是自己起了 `Agent` subagent 做 Web 搜索,直接出结论
- **违反**:§3 克制规则——"项目级请求默认走 ccteam new 派单,让对应团队 session 干活"
- **损失**:product-research 的 6-phase pipeline(kickoff → research → verdict → next-steps)没跑,缺少 verdict 结构化判断和可审计的调研记录
- **改进方向**:
  1. meta-agent 决策树加 self-check:收到"调研 X"类请求时,先问自己"这是问答还是项目请求?"
  2. 项目请求 → 走 product-research;纯问答(如"Multica 的 GitHub 地址是什么")→ 直接回答
  3. 边界不清时,问用户一句:是要我快速查一下,还是走正式产研流程?

---

## 用户给的开发流程要求

> 整理成一个 V0.2.2 的小需求进行开发,注意实现方案不一定参考以上的建议,实现方案从全系统来考虑设计,但是问题是真实的。
>
> 开发流程要求,先输出文档到 v0-2-2 文件下,然后启动开发,开始是用当前 session 派 subagent 去做,用 worktree 新分支,最后提 pr,主 agent review → fix → merge。
>
> 最终 cargo workplace 中版本也要改。把这些开发流程用简洁的语言记录在 CLAUDE.md。

(以上原话照录;PRD §7 / §10 / §11 把这套流程吸收到正式规约。)
