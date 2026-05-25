# ccteam 故障排查手册

> 主入口都在 Claude session 内:`/ccteam`(总入口)、`/ccteam-scan`(摸底)、`/ccteam-team`(临时 team)、`/ccteam-creator`(新建项目)、`/ccteam-control`(运行时)、`/ccteam-im-setup`(TG 配置)、`/ccteam-advise`(双 LLM 投票)。**诊断**走 **CLI**:在任意终端跑 `ccteam doctor`(详细诊断,80% 卡点它自动检出);Claude session 内可以 `Bash("ccteam doctor")` 一键调用。`--verify-mcp` 自检 MCP 表面 27 active / 0 stubs,`--check-cost-orphan` 对账 ledger 与 progress.jsonl,两条都适合放 CI gate。还卡再查本手册。
>
> 进阶 fix:`docs/claude-code-tool-surface.md` 看平台原语,`docs/orchestration-patterns.md` 看拓扑选择,`docs/tech-design.md` 看架构权威。

---

## A. 安装 / 初始化(15 条)

### A1. `ccteam doctor` 报 "claude CLI not found"
**原因**:系统 PATH 里没有 `claude` 可执行文件,或刚装好但当前 shell 没 reload。
**修复**:1) 终端 `which claude` 确认;2) 没装就 `npm i -g @anthropic-ai/claude-code`;3) 装了找不到就 `hash -r` 或重启 Claude session。
**相关**:A2(版本过低)。

### A2. `ccteam doctor` 报 "claude version too old, need ≥ 2.1.139"
**原因**:ccteam 依赖 agent-view / `--bg` 等较新特性,旧版没有。
**修复**:`claude update` 升 stable → 重启 Claude session → 重跑 `ccteam doctor`。
**相关**:A1。

### A3. `/plugin install ccteam@claude-plugins-official` 失败
**原因**:网络 / GitHub rate-limit / 已装同名 plugin 冲突。
**修复**:1) `/plugin list` 看是否同名 → `/plugin remove ccteam` 重装;2) 走代理 / 国内镜像;3) 仍失败:手动 `git clone` 到 `~/.claude/plugins/marketplaces/`。
**相关**:A4 / A5。

### A4. `/mcp` 列表里没 `mcp__ct__*` 工具
**原因**:MCP 没注册到 `.mcp.json` 或 `~/.claude.json`,或 plugin 装完没 reload。
**修复**:1) `ccteam doctor --install-mcp`;2) `/reload-mcp` 或重启 Claude session;3) 项目级 `.mcp.json` 残缺 → 删后重 `ccteam doctor --install-mcp`。
**相关**:D1。

### A5. `~/.claude.json` 损坏,Claude 一启动就崩
**原因**:手改错 JSON / 编辑器加 BOM / 半截 plugin install 写垃圾。
**修复**:1) `cp ~/.claude.json ~/.claude.json.bak`;2) `jq . ~/.claude.json` 看哪行错;3) 实在修不了删掉重启 Claude(会生空 stub,要重登录)。
**相关**:A3。

### A6. `ccteam doctor` 报 "supervisor not running"
**原因**:Claude Code 后台 supervisor(管 `--bg` session 的常驻进程)没启或被杀。
**修复**:1) 终端 `claude agents` —— 打开 agent view 时 supervisor 自启;2) 仍失败 `pkill -f "claude.*daemon"` 后再开。
**相关**:B1。

### A7. `/ccteam-creator` 起新项目报 "Team already exists"
**原因**:上次新建留下的 `~/.claude/teams/<name>/` 残留,或 `~/projects/<slug>/` 已存在。
**修复**:1) 重选另一个 slug;2) 想用同名:手动 `rm -rf ~/.claude/teams/<name> ~/projects/<slug>` 后重起,或 `ccteam remove <slug> --purge` 一并清 `imd/registry/`。
**相关**:A11。

### A8. 跟 BotFather 要 TG bot token 拿不到
**原因**:TG 没注册 / @BotFather 没回 / 超出 20 bot 上限。
**修复**:1) TG 搜 `@BotFather` 发 `/newbot`;2) 没回信先发 `/start`;3) 超 20 用 `/mybots` 删旧的。
**相关**:A9 / A10。

### A9. 拿到 token 但不知道 chat_id
**原因**:bot 加群后必须有人 @ 它一次,bot 才能拿到 chat_id。
**修复**:1) 群里 `@your_bot hi`;2) Claude session 里 `/ccteam-im-setup`,它读 bot 最近消息列出 chat_id;3) 选一个绑到 workflow.yaml 的 env。
**相关**:A8。

### A10. `/ccteam-im-setup` 报 "token invalid"
**原因**:复制粘贴带空格 / 引号 / 换行,或 token 被 BotFather revoke 过。
**修复**:1) @BotFather → `/mybots` → 选 bot → "API Token" 重新复制;2) 粘贴时确保单行无空格;3) 仍失败 BotFather `/revoke` 重生成。
**相关**:A8。

### A11. 想跑 `ccteam new ...` shell 命令,提示 command not found
**原因**:推荐**不走 shell**,所有创建在 Claude session 里 `/ccteam-creator` 完成。
**修复**:在 Claude session 内 `/ccteam-creator`,跟向导走完即可。**不需要装 ccteam 二进制**(除非你要跑 `ccteam start` daemon)。
**相关**:A3。

### A12. 项目 `.mcp.json` 跟其他 MCP server 冲突
**原因**:你装了 `serena` / `omc` 等同时注册项目级 `.mcp.json`,合并后字段互踩。
**修复**:1) `ccteam doctor` 自动合并不覆盖;2) 手查 `.mcp.json` 里 `mcpServers.ccteam` 是否齐全;3) 仍冲突:`ccteam` 仅 user-global 注册,其他保留项目级。
**相关**:A4。

### A13. 跨设备 — 一台机器装了在另一台没有
**原因**:`~/.claude/plugins/` 和 `~/.claude.json` 不在 git 范围。
**修复**:1) 新机器重跑 `/plugin install ccteam@claude-plugins-official`;2) workflow.yaml 入项目 git 共享;3) bot token 走 env,**不入 git**。
**相关**:C3。

### A14. WSL 行为怪
**原因**:WSL 文件系统 watch 偶尔丢事件;Windows 路径 ≠ Linux 路径。
**修复**:1) 项目放 WSL 的 `~` 下而非 `/mnt/c/`;2) 用 WSL 原生 npm 装 claude,别走 Windows npm。
**相关**:A1。

### A15. macOS Gatekeeper 拦 codex 二进制
**原因**:从 GitHub Releases 下载的 codex 没签名。
**修复**:System Settings → Privacy → "Allow anyway";或 `brew install --cask codex`(已签)。
**相关**:E1、A16。

### A16. macOS Gatekeeper 拦 ccteam 二进制(`install.sh` 装的 prebuilt)
**原因**:GitHub Releases 的 macOS 二进制未走 Apple notarization;首次跑会被 quarantine 属性拦,报"cannot be opened because the developer cannot be verified"。
**修复**:`xattr -d com.apple.quarantine ~/.local/bin/ccteam`(或装到的目录)。或退到从源码 build:`cargo install --git https://github.com/firstintent/ccteam ccteam-cli`(本地编译的 binary 没有 quarantine 属性)。
**相关**:A15。

### A17. `install.sh` 报 "checksum FAILED" / "not listed in SHA256SUMS"
**原因**:1) 下载途中网络损坏(常见);2) GH Release 上传不全(罕见 — release CI 漏 SHA256SUMS 行);3) 中间人篡改(理论)。
**修复**:1) 重跑 `curl ... | sh`;2) 走 GH Releases 页面手动下载对应 tarball,校 `shasum -a 256 ccteam-*.tar.gz` 对照 `SHA256SUMS` 文件;3) 仍失败 → 提 issue 附下载链接 + 校验对照。
**相关**:无。

### A18. `install.sh` 报 "unsupported platform: linux-aarch64"
**原因**:linux-arm64 prebuilt 已发,你的 install.sh 是老版没识别 aarch64。
**修复**:重跑最新 install.sh(`curl -sSL ...install.sh | sh`),它会下 `linux-arm64` musl static binary。
**相关**:A16。

### A19. `ccteam init` 在 ccteam 源码目录被拒
**原因**:防自指 — 默认不允许在 ccteam repo 自身里装 ccteam 项目(会形成循环 hook setup)。
**修复**:开发 / self-host / dogfood 场景明确要在源码目录跑,加 `--force`:`ccteam init --force`。
**相关**:无。

### A20. creator skill 写完但 bot 不工作 / SessionStart hook 报 "not under any ccteam project"
**原因**:老版 creator 没自动调 init 流程,缺 `<project>/.ccteam/state.json`。
**修复**:升到 V0.6.8+ — `chat_register_bot` MCP 自动调 bootstrap_project_at_dir + install_hooks。手动修:`cd <project> && ccteam init --force`(在已经有 workflow.yaml 的项目里 refresh state.json 安全)。
**相关**:A19 / B11。

---

## B. 聊天运行时(15 条)

### B1. TG 群 @bot 后 bot 完全不回
**原因**:5 种 — bot 进程崩 / token 失效 / 网络不通 / SessionStart hook 链断(hook.sh 缺失 或 env propagation 中断)/ tail loop 等不到 `active-session-id` marker。
**修复**:1) `/ccteam-control show-bots` 看每个 bot 状态;2) status "stopped" → `/ccteam-control restart-bot <name>`;3) "running 但不回" → daemon 日志看 `tail_marker_missing` WARN(60s+ marker 没写)= hook 链断;4) 等 60s 触发 F196 marker self-heal 自动重 spawn session;5) 看 turn watchdog `chat_turn_running_long` event(90s)/ `chat_turn_timeout`(180s)— bot 还在算或真卡死;6) 手动 force:`ccteam doctor --install-hooks` + `ccteam stop && ccteam start`。
**相关**:A6 / A20 / B10 / B11。

### B2. bot 回了但内容像"失忆了"
**原因**:bot 对话历史被清(compact / new session)或平台升级后旧会话失效。
**修复**:1) `/ccteam-control show-bot <name>` 看 "memory reset" 时间;2) 这是正常,告诉 bot 上下文重新讲;3) 想少触发:看 B6。
**相关**:B6 / B11 / D2。

### B3. bot 回得超慢(>30s)
**原因**:模型本身慢(Opus)/ 上文太长缓存未命中 / 后端限流。
**修复**:1) `/ccteam-control show-bot <name>` 看 last_turn_latency;2) workflow.yaml 切到 sonnet;3) 强制 compact:`/ccteam-control bot-compact <name>`。
**相关**:B6 / C4。

### B4. 群里两个 bot 互 @ 卡死循环
**原因**:bot-to-bot 链超过 hop_limit(默认 3)之前没拦。
**修复**:1) ccteam 第 3 跳自动 escalate;2) workflow.yaml 调低 `hop_limit: 2`;3) 立即打断:群里发 `@your_bot stop`。
**相关**:B5。

### B5. escalate 后没人提醒我
**原因**:默认 escalation 只写 ccteam 内部日志,不主动推 TG。
**修复**:1) workflow.yaml 加 `on_hop_escalate: notify_tg`;2) `/ccteam-control show-escalations` 看全列表。
**相关**:B4。

### B6. 想让 bot 记得更多 / 更少历史
**原因**:默认 50 turn 触发自动 compact,可调。
**修复**:workflow.yaml 改 `agents.<role>.compact_every_turns: 100`(多记)或 `: 20`(少记,省钱)。
**相关**:C4 / C9。

### B7. 编辑过 / reply 引用的 TG 消息 bot 看不到
**原因**:TG bridge 只处理 plain text 新消息(编辑事件不入 inbox)。
**修复**:编辑后重新 @bot 一次;reply quote 不传入,需在新消息复述要点。
**相关**:B8。

### B8. bot 看不到图片 / 文件
**原因**:chat 模式当前只支持 text;富媒体在 backlog。
**修复**:1) 图片 OCR 后粘贴文字;2) 文件放服务器 + URL 让 bot fetch。
**相关**:B7。

### B9. (reserved)

### B10. bot 突然回乱码 / 内部错误
**原因**:模型 API 抽风(rate limit / overload / 内容审核)。
**修复**:1) 等 30s 再试;2) `/ccteam-control show-bot <name>` 看 last_error;3) 持续报错 → 看 C2。
**相关**:B3 / C2。

### B11. 想让 bot 完全忘记重来
**原因**:希望 bot fresh 起。
**修复**:`/ccteam-control bot-new-session <name>`,下一句开始全新上下文。
**相关**:B2 / B6。

### B12. 多 bot 同名 / 串号
**原因**:workflow.yaml 两个 role 用同 `bot_name`,或同 TG bot 注册到多 workflow。
**修复**:1) `/ccteam-control show-bots` 看冲突;2) workflow.yaml 改名;3) 一个 TG bot 只能属一个 workflow。
**相关**:A8。

### B13. bot 回复夹了 `<thinking>` / 内部标记
**原因**:模型未屏蔽 thinking 段,或 system prompt 没拦。
**修复**:1) 通常 ccteam 自动剥;漏的请报 issue;2) workflow.yaml 加 `strip_thinking: true`。
**相关**:B10。

### B14. 群里多人同时 @bot,谁先谁后
**原因**:bot 单 session 串行处理 turn。
**修复**:1) 默认 FIFO;2) 想真并发:workflow.yaml 拆多 role 各起 session,@ 分别 routing。
**相关**:B3 / `docs/orchestration-patterns.md` §3 Parallelization。

### B15. bot idle 时偶尔自己开口
**原因**:没装 schedule trigger / 平台内部 retry 串出。
**修复**:1) `/ccteam-control show-bot <name>` 看 last_event;2) 若 retry:平台兜底,不动;3) schedule 触发但你没要:workflow.yaml 删 `trigger: schedule`。
**相关**:C7。

---

## C. 成本 / 配额(10 条)

### C1. 24h budget 触底,bot 全停
**原因**:cost-cap 红线 — workflow 24h cost 撞 `max_cost_usd_per_24h`。
**修复**:1) `/ccteam-control show-cost` 看哪个 role 烧得多;2) workflow.yaml 调高 budget 或换便宜模型;3) 等 24h reset 自动恢复。
**相关**:C5 / C7。

### C2. Claude 报 "rate_limit" / "billing_error"
**原因**:你的 Anthropic 账号月度额度触顶 / 信用卡过期。
**修复**:1) https://console.anthropic.com 查 usage / billing;2) ccteam 自动 retry,持续 fail 会 emit `budget_exceeded`;3) 加额度或换 API key。
**相关**:E2 codex auth。

### C3. `/ccteam-control show-cost` 数字跟 console 对不上
**原因**:ccteam 按 turn token × 单价累计;console 算 billing cycle。短期 5-15% 偏差正常。
**修复**:长期偏差大(>30%)才查;通常 console 是权威。
**相关**:C1。

### C4. 同 workflow 两次跑 cost 差 10×
**原因**:通常 prompt cache 命中 vs 不命中(cache hit 约 10% 价)。
**修复**:1) workflow.yaml `use_cache: true`(默认 on);2) 短间隔重跑命中率高;3) 跨日 cache TTL 失效正常。
**相关**:B6 / C8。

### C5. 想给单个 bot 设硬上限,不动整个 workflow
**原因**:cost cap 默认 workflow 级。
**修复**:1) workflow.yaml `agents.<role>.max_cost_usd_per_day: 5`(per-role);2) 平台层附加 `--max-budget-usd` 硬终止兜底。
**相关**:C1。

### C6. Codex 跟 Claude cost 字段对不上
**原因**:Codex 用 token usage 报告,Claude 用 Anthropic billing API,精度不同。
**修复**:`/ccteam-control show-cost --vendor codex` 单独看;两 vendor 不直接加和,各算各。
**相关**:E5。

### C7. schedule-trigger 的 role 把 budget 烧光
**原因**:`trigger: schedule` 每 N 分钟跑一次,没 cap 容易爆。
**修复**:1) workflow.yaml `interval: 30m` 拉长;2) 该 role 加 `max_cost_usd_per_day`;3) 暂停 `/ccteam-control pause <slug>`。
**相关**:C1 / C5。

### C8. Cache 不命中,prompt token 暴涨
**原因**:system prompt 含动态字段(时间戳 / cwd)每次都变。
**修复**:ccteam 默认带 `--exclude-dynamic-system-prompt-sections`;若仍不命中,workflow.yaml 别在 system prompt 里塞 "今天日期" 这类。
**相关**:C4 / `docs/claude-code-best-practices.md` §6.2。

### C9. compact 完反而更贵了
**原因**:`/compact` 本身要一次 model call,短期成本高但后续 turn 省。
**修复**:1) 只在历史 >5k token 时调 `/ccteam-control bot-compact`;2) workflow.yaml `compact_every_turns` 调高减少触发。
**相关**:B6 / C4。

### C10. 想完全 dry-run 不花钱
**原因**:验证 workflow 而不调真实模型。
**修复**:1) `/ccteam-control dry-run <slug>` — 只校验 yaml + 列将 spawn 的 role,不调 model;2) 单 bot:`CCTEAM_DRY_RUN=1` env。
**相关**:C7。

---

## D. Claude Code 平台层(5 条)

### D1. `/mcp` 显示 `ct` server "disconnected"
**原因**:`mcp-serve` 子进程没起或被 kill;`.mcp.json` 路径不对。
**修复**:1) 重启 Claude session 通常自愈;2) `ccteam doctor --install-mcp` 重写注册;3) `claude --debug "mcp"` 看启动错误。
**相关**:A4。

### D2. `claude -p --resume <sid>` 报 "session not found"
**原因**:session id 失效(平台升级 / 用户清过 `~/.claude/projects/`)。
**修复**:1) ccteam 自动起新 session 并 emit `chat_session_reset`;2) 历史聊天不可恢复,告知 bot 重讲 context。
**相关**:B2。

### D3. 自定义 `.claude/agents/<role>.md` 不生效
**原因**:agent 文件只在 session 启动时扫一次;新加文件需重 spawn。
**修复**:1) `/ccteam-control restart-bot <name>` 重 spawn;2) frontmatter 有 yaml 错会被静默忽略 — `ccteam doctor` 会 lint。
**相关**:`docs/claude-code-tool-surface.md` §1.2.4。

### D4. PreToolUse / PostToolUse hook 不 fire
**原因**:`.claude/settings.json` 里 hook 路径错或脚本没 +x。
**修复**:1) `chmod +x .claude/hooks/*.sh`;2) `claude --debug "hooks"` 看 fire 痕迹;3) `--bare` mode 下 hook 全跳。
**相关**:`docs/claude-code-best-practices.md` §4.5。

### D5. `/<skill>` 没出现在补全
**原因**:`~/.claude/skills/` 目录是 session 启动后建的,监听没挂上。
**修复**:1) 关掉 Claude session 重开;2) `/reload-plugins`(tmux-attached session 可用);3) 仍不见 → `/plugin list` 检查 ccteam plugin 状态。
**相关**:A3 / `docs/claude-code-tool-surface.md` §1.2.4。

---

## E. Codex 集成(5 条)

### E1. workflow.yaml 标 `vendor: codex` 但 spawn 报 "codex not found"
**原因**:Codex CLI 没装在 PATH。
**修复**:1) `npm i -g @openai/codex` 或 `brew install --cask codex`;2) `ccteam doctor` 自动检测并降级用 claude;3) 想坚持 codex:装完重启 Claude session。
**相关**:E2 / A15。

### E2. codex auth 缺 / 过期
**原因**:`codex login` 没跑过,或 OAuth token 过期。
**修复**:终端 `codex login`,按提示 OAuth;`ccteam doctor --check-codex` 验证。
**相关**:E1。

### E3. codex 报 "sandbox denied"
**原因**:codex 默认 `--sandbox read-only`,不允许写文件。
**修复**:1) workflow.yaml `agents.<role>.codex_sandbox: workspace-write`;2) 危险场景才用 `danger-full-access`,且仅在容器里。
**相关**:见 [advanced/multi-llm-codex.md](advanced/multi-llm-codex.md) Codex sandbox 表。

### E4. codex 比 claude 慢 3-5×
**原因**:Codex 默认走 OpenAI Responses API,首 token 延迟比 Anthropic 高;且 ccteam 还没接 Codex 的 prompt cache。
**修复**:1) 高频对话场景用 claude;2) batch / advise 一次性场景可接受。
**相关**:B3。

### E5. 同 workflow 混 Claude+Codex,cost 报告分裂
**原因**:两 vendor 计费单位、token 价格不同,目前不合算。
**修复**:`/ccteam-control show-cost --vendor claude` 和 `--vendor codex` 分别看;workflow 总额按各自 budget 独立 cap。
**相关**:C6。

---

## F. Daemon / 运维(4 条)

### F1. `ccteam start` 不退 / Ctrl+C 没反应
**原因**:不应发生。daemon 实现 SIGINT / SIGTERM graceful drain(上限 5 秒);超过 5 秒未退 = bug。
**修复**:
1. **优先**:`kill -TERM $(cat ~/.ccteam/ccteam.pid)` 或 Ctrl+C 再等 5 秒
2. 仍卡 → 收集 `ccteam doctor --full` + 跑 `ps -ef | grep ccteam` 看孤儿进程,贴 GitHub issue
3. **最后兜底**(会留孤儿 pidfile / 可能伤进行中的 turn 状态):`kill -9 $(cat ~/.ccteam/ccteam.pid) && rm -f ~/.ccteam/ccteam.pid`
**相关**:F2 / F3。

### F2. 升级 ccteam 后 chat bot 失联 / context 像丢了
**原因**:不应发生。`ccteam start` 启动时探测每个已注册 bot 的 `ccteam-chat-<slug>-<role>` tmux session,若 session + pane 内 claude 进程都活 → 自动 reattach(bot context 不丢);若 session 在但 pane 死 → kill stale + spawn `claude --resume <name>`,Anthropic 官方 CLI 直接 reload full API-level context(模型脑子里还有上次的东西)。
**修复**:
1. 终端 `tmux ls | grep ccteam-chat-` 看 session 是否存在
2. 存在但 ccteam 没 reattach → 看 daemon 日志(`~/.ccteam/logs/ccteam-imd.log`)有无 reattach / resume 行
3. 日志显示 `chat_session_reset` event with `reason="resume_failed_fallback_to_fresh"` → `--resume` 失败(session jsonl 不存在 / 用户清过 `~/.claude/projects/` / Anthropic schema 升级)── bot 已退到 brand-new session,context 真丢;用 `mcp__ccteam__chat_history` 抓上轮 `turns.jsonl` 让 bot 自己重读上下文,或在 IM 端直接 paste 一句 summary
4. session 不存在 + 无日志(进程被 OOM-killed + `~/.claude/projects/` 也清空)→ ccteam 起新 session,context 确实丢,同上 step 3
**验证 lossless 恢复**:发条 `刚才那个 X 怎么样?` 风格 follow-up,bot reply 若能直接引用早 turn 内容 = 真 lossless;若 reply 显示"对不起请重述"= fallback 已走。
**相关**:F1 / B11。

### F3. `ccteam mcp-serve` 跑起来 stdout 空 / 看不到 prompt
**原因**:**正常行为**。`mcp-serve` 走 stdio JSON-RPC,**stdout 留给 protocol frame**,所有 tracing / log 走 stderr。
**修复**:
- 想看 log:**不要** `2>/dev/null` 屏蔽 stderr。`ccteam mcp-serve 2>&1 | tee debug.log` 才能同时看 RPC + log
- 想纯 RPC view:`ccteam mcp-serve 2>/dev/null` — stdout 仍能解析(JSON-RPC 不被污染)
- 想加 verbosity:`RUST_LOG=debug ccteam mcp-serve` 走 stderr
**相关**:见 [advanced/customize-workflow.md](advanced/customize-workflow.md) MCP 内部章节。

### F4. `chat_reset` 后 bot 还在回老 context
**原因**:不应发生。`chat_reset` 归档 `turns.jsonl` + 清磁盘 cursor + daemon 内存 cursor 同步重置(三者原子)。
**修复**:
1. 看 `<ccteam_root>/imd/registry/<slug>/<role>.json` 里 `cursor` 字段是否 0
2. 看 `<project>/.ccteam/chat/<bot>/turns.jsonl` 是否空(原内容应在 `archive/turns-<unix-ms>.jsonl`)
3. tmux session 内 claude 进程仍持着旧 context — `mcp__ccteam__chat_unregister_bot` 再 `chat_register_bot` 一次,强制 spawn 新 claude 进程
**相关**:B11 / §4.7 (user-manual.md)。

### F5. `ccteam doctor --check-cost-orphan` 报 WARN
**原因**:24h 内某 vendor 的 `agent_done` event 数 ≠ ledger row 数 ── 某 spawn 路径绕过了 `<ccteam_root>/cost-budget.json` ledger 写入。常见原因:(a) 用户自加 custom adapter 没接 ledger hook;(b) 外挂 bash 直跑 `codex exec` / `claude --print` 绕开 `CodexExecAdapter` / `ClaudeTuiAdapter`;(c) ccteam 自身新增 spawn 路径忘接(请提 issue)。
**修复**:
1. WARN 提示哪个 vendor 漏算 → 看自己的 workflow.yaml / skill 有没有手写 bash spawn(用 MCP `mcp__ccteam__advise_vote` 替代)
2. ccteam built-in 路径漏 → 看 `<project>/.ccteam/progress.jsonl` 里漏的 `agent_done.vendor` 字段对应哪个 adapter,提 issue
3. 长期对账失败 → CI 用 `ccteam doctor --check-cost-orphan` exit code 守 invariant(0 = OK / 1 = orphan,可放 nightly job)
**相关**:C6 / §4.8(user-manual.md)。

### F6. `ccteam doctor --verify-mcp` 报 STUB found
**原因**:不应发生(0 STUB 是 ship gate invariant)。当某 MCP tool 注册了但 dispatch fn 返 `NotImplemented` 时触发。
**修复**:
1. 看输出 per-group breakdown,找具体哪个 group 有 stub
2. `cargo clean && cargo build` 排除编译产物错位
3. 仍报 → 跑 `ccteam doctor --verify-mcp --json | jq .unexpected_stubs` 取 stub 列表 → 提 issue 附完整 doctor 输出
**相关**:A4 / §4.8。

---

## 仍未解决?

1. 在终端跑 `ccteam doctor --full` 收集所有诊断信息(Claude session 内 `Bash("ccteam doctor --full")` 亦可)
2. 仍卡:把诊断输出贴 GitHub issue,或 ccteam 用户群 @ 维护者
3. 进阶 fix:`docs/claude-code-tool-surface.md` 看平台原语;`docs/dev-coupling-audit.md` 看历史已修类似问题;`docs/tech-design.md` 看架构权威
