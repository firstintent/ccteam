# v0.8.8 dev-session 开发提示词(workflow + opus subagents,dev 直推不 PR,一口气跑完)

> 这是交给 **dev session** 的执行 briefing。**SoT = 同目录 `prd.md`(范围+设计+开放问题)+ `bug.md`(每条 bug 的根因 file:line)**。本文给执行约束 + 阶段计划 + 已拍板的默认决策 + 红线 + gate。
> 需求收集/文档由另一会话产出;**本提示词的执行者 = 你(dev session),负责写代码 + 跑 workflow + 提交**。

---

## 〇、执行模式(硬约束)

- **用 Workflow 编排**:你自己 author 一个 Workflow 脚本(按 §二 的阶段),`agent(...)` 派子任务。
- **subagent 一律 opus**:每个 `agent()` 传 `{ model: 'opus' }`。
- **dev 分支直接开发 + 提交,不开 PR**:在 `dev` 上干;每个 wave/finding 一个 commit,`v0.8.8:` 前缀,英文 commit、中文文档/agent prompt;commit 尾部带 `Co-Authored-By: Claude ...`;每完一阶段 `git push origin dev`(推前 `git fetch && git rebase origin/dev`,可能有并行会话)。
- **一口气跑完、中途不停问**:开放问题已在 §三 给了**默认决策**,照默认做,**不要停下来等人**。`AskUserQuestion` 仅在出现默认决策也覆盖不了的真·阻断(如某 vendor API 行为与假设矛盾)时才用,否则一路到底。
- **不停 ≠ 不验证**:没有 PR 评审,所以**质量靠 workflow 内部 gate 兜底** —— 每个阶段末尾必跑 verify gate(下方)+ 派一个对抗 review subagent 复核该阶段产物,先修绿再进下一阶段(fix-forward,别带病前进)。

### verify gate(每阶段末 + 收尾必过,不退基线)
1. **先记录当前 dev 基线**:`cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` —— 记下 `pass fail`,作为**不可退基线**(收尾必须 ≥ 它且 fail=0)。
2. `cargo clippy --workspace --all-targets -- -D warnings` = 0。
3. `cargo fmt --all -- --check` 干净(改完即 `cargo fmt --all`)。
4. 动了 `ccteam-web` 前端 → `vitest`(SPA)green;动了 web 后端 → 相关 `ccteam-web` 测试 / smoke。
5. 验证优先**确定性 fake**(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 真实 WS/HTTP smoke;**不 scrape pane**。

---

## 一、任务总览(全部 v0.8.8 scope)

读 `prd.md` 拿全貌。一句话:**把 session 从「= role」升成「独立一等实体(持久 id)」**(F1,根治串台 BUG-3)+ **补全 web**(F4 config / F5 role 页 / F3 status 重写 / B5 终端)+ **批修 v0.8.7 遗留 bug**(B1-B5)+ **清理**(C1)+ 全程守 **web UI 质量基线**(prd §二)。

依赖关系(决定阶段顺序):**F1 是主干** —— BUG-3 由它根治,B4(session ls)/B5(web 终端)/F2(roleless)/F3(status 列会话)都建立在「按 session 解析」之上。C1 / B1 / B2 与 F1 无关、可先并行做掉。

---

## 二、阶段计划(workflow 形状,每阶段一 gate)

### Phase 0 — 清理 + 独立 bug(并行,低风险热身)
- **C1 清理**(prd §三 C1):删 `teams/`、`skills/`、`examples/`、根 `config.yaml`(+ 加进 `.gitignore`)、`workflows/`、`agents/`、`tests/intent-corpus.yaml` + 清 `scripts/host-probe/intent-accuracy.sh` 里对已删 `skills/ccteam/SKILL.md` 的 stale 引用。**保留 `.agents/plugins/marketplace.json`**。删前每个再 grep 一遍确认无运行期引用(**别碰 `~/.ccteam/teams` 那个运行期目录**)。
- **B1 / BUG-1**:`stop_project_chat_sessions`(commands.rs)改经 `default_backend()` 枚举+kill,去 tmux-only。
- **B2 / BUG-2**:web「＋新建」恢复「＋新建项目…」(name+path),走 REST `POST /api/v1/projects`(不走旧 WS `/newproject`)。
- **gate** → 提交 + push。

### Phase 1 — F1 独立 session 模型(headline;主干;可再分子任务)
落 prd §三 F1。按 §三默认决策实现:
- **session = 一等实体 + 持久 id**:每会话一个 `s<N>`(项目内单调、**持久化进 state**,扛 daemon 重启、N 不复用);session 记录存 `{id, role(可空), vendor, permission_mode, created, pane_name, native_session_ref}`。
- **role 降为属性**;**去掉 `(项目,role)` dedup** → 同 role 可并存多 session。
- **pane / tmux-rmux 命名 + turns 按 session id**:pane 名 `ccteam-chat-<slug>-<sid>`(不再含 role);turns 落 `<project>/.ccteam/chat/<sid>/turns.jsonl`;`TurnRecord` 加 `session_id`(或目录即隔离)。
- **resume-by-id 改成按 session id**(红线保留,只是粒度从 (项目,role) → session);空闲释放/扛重启语义不变。
- **gateway**:create 不再 dedup;`session_resolve(sid)` 返回 role/vendor/pane/project_dir;`/role <role>` = 改**当前 session** 的 role 属性并原地 re-spawn(同 sid、换 `--agent`)。
- **IM 寻址**:`/new [role] [hitl]` 铸新 session(回其 `s<N>` 句柄、设为 current);`/use s<N>` 切;`/sessions` 列本项目 sessions(id/role/vendor/status)。
- **迁移(pre-v1.0 无兼容)**:旧的内存 `s{n}` + 按 role 的 turns 不向后兼容 → 不写迁移;升级 = 文档「清 `~/.ccteam` + 各项目 `.ccteam` → 重 `ccteam init`」;旧 per-role 历史丢弃(可接受)。
- **gate(F1 专项)**:新增测试 —— 同 role 起两个 session 各自独立 turns/历史(**不串**);daemon 重启后 sid 稳定可 resume;`session_resolve` 正确。BUG-3 串台消失。

### Phase 2 — 建立在 F1 上(并行)
- **B3 验证**:per-session 历史端点 `GET /sessions/{sid}` 按 sid/会话目录取 → 不再回整份 per-role;前端 ChatConsole 仍 per-sid（已隔离），确认 seed 不串。
- **B4 / BUG-5**:`session ls` + `status` 的活性/vendor/sid 从 **gateway session map** 取(修 codex 误报 not running)+ 加 vendor 列;backend 名枚举只留标 orphan。
- **B5 / BUG-6**:`pty_ws::handle_session_ws` 经 gateway 把 `sid → pane`(`ccteam-chat-<slug>-<sid>`)再 subscribe(去掉退回项目级 pane 的 TODO);`send_keys`/`resize_window` 改 `default_backend()`;模块 doc 更新。**"像本地终端"完整保真**(裸 ANSI)= rmux 裸字节订阅(W2b)子项:本版**至少**做到稳定连住 + 可输入/输出/resize;裸字节保真若工作量大,降级为"行文本可用 + 留 TODO/issue"并在收尾说明(别假装做完)。
- **F2 roleless**:role 空 → spawn 跳过 `--agent`;create 路径"仅非空 role 才校验存在"(保留 FIX-2);turns 用 sid(已 F1)。
- **F3 status 重写**:列所有项目 + 各自 sessions(role/vendor/status/sid/last-event);删 recent events;web 两行 `web token: <hex>` + `web url: http://<LAN-ip>:7331/?token=ccteam:<hex>`(LAN ip = 第一个非 loopback 私网 IPv4);保留 STUCK/OK(项目级)。
- **gate** → 提交 + push。

### Phase 3 — web 功能 + UI 质量基线(并行)
- **F4 web config 模块**:web 设置页 + API(web-token 门后),配 **Telegram + Lark**;复用 CLI 校验器(`run_config_set_im_token`/`run_config_set_lark_creds`)。**秘密绝不回显明文**(只 masked + "已配置" + 末几位);telegram chat_id 走异步 UX(填 token → 提示"去给 bot 发条消息" → 轮询抓到);lark 直接验。配完尽量热生效(reload creds),做不到则提示重启。
- **F5 web role 浏览页**:浏览**已装**项目 role(`GET /projects/{slug}/roles` 列表 + 详情);**只读**(本版不开 web 编辑;catalog 在线浏览/装 = 可选 stretch,有余力再加 `GET /catalog/roles`)。
- **全程守 prd §二 web UI 质量基线**:复用 Tailwind 主题、四态(loading/empty/error/success)、错误可读、响应式+移动、键盘可达、SSE 保鲜、a11y;每个 web 项收尾派一个 UX-review subagent 走查。
- **gate**(含 vitest + UX 走查)→ 提交 + push。

### Phase 4 — 文档 + 版本号 + 收尾 gate
- **改红线**:F1 把 `session = role` keystone + 「单 (项目,role) pane」dedup 改了 → 重写 **CLAUDE.md §〇/§三** + `docs/tech-design.md` 相应段(session = 独立一等实体 + 持久 id;role 是属性;resume-by-session-id);**协议→代码指针表**同步。
- **用户面**:`README.md`(英文、不夹版本进展)+ `docs/usage.md` 把新能力融进**当前能力**描述(独立 session / web config / role 页 / status 新格式)。
- **版本**:workspace `Cargo.toml` `0.8.7 → 0.8.8`;CLAUDE.md §一 baseline 回填(新 test 数 / clippy 0)。
- **归档**:`docs/versions/v0-8-8/` 落 `README.md`(冻结里程碑)+ 每 phase 一份 `wave-N-handoff.md`(Decided/Rejected/Risks/Files/Remaining 五段)。
- **收尾 gate**:full `cargo test --workspace --exclude ccteam-web`(≥ 基线、fail=0)+ clippy 0 `-D warnings` + `cargo fmt --all --check` + vitest;`ccteam doctor --verify-mcp` 不 drift;skill-gate(`grep -rnE "V\d+\.\d+|docs/versions|Wave [0-9]|F[0-9]+|ship gate|shipped" skills/*` 0 命中——注意 skills/ 本版被删,确认 gate 仍过)。push。

---

## 三、已拍板的默认决策(照做;user launch 前可改,改了我同步 prd)

| 开放问题 | **默认** |
|---|---|
| F1 session 持久 id 形态 | 项目内单调 `s<N>`、**持久化进 state**(扛重启、不复用);别依赖 claude 原生 UUID(`--name` 是 title 非 id) |
| F1 pane / turns key | pane `ccteam-chat-<slug>-<sid>`;turns `.ccteam/chat/<sid>/turns.jsonl` |
| F1 `/role` 语义 | 改**当前 session** 的 role 属性 + 原地 re-spawn(同 sid),**不**新开 |
| F1 IM 多会话寻址 | `/new [role]` 铸新(回 `s<N>`)、`/use s<N>` 切、`/sessions` 列 |
| F1 迁移 | 无兼容,清旧 + 重 init;旧 per-role 历史丢弃 |
| F3 会话行字段 | role(空→`-`)+ vendor + status + sid + last-event;STUCK/OK 保留 |
| F3 LAN ip | 第一个非 loopback 私网 IPv4 |
| F4 范围 | 仅 telegram + lark;**不**做 slack/discord/web-token 轮换 UI;**不**做 TLS(文档警示 LAN 明文) |
| F4 telegram chat_id | 异步:填 token → "去 DM bot" → 轮询抓 |
| F5 范围 | 仅浏览**已装** role + 只读;catalog 在线装 = 可选 stretch |
| B5 终端保真 | 至少稳定连 + 可交互;裸 ANSI(W2b)工作量大则降级"行文本可用 + 留 TODO",收尾说明 |
| C1 范围 | 删 teams/skills/examples/根config.yaml/workflows/agents/tests-intent-corpus + host-probe stale 引用;**留 .agents/** |

---

## 四、红线(CLAUDE.md §三,任何 wave 不得破)

- **No prompt injection**:role 行为住 `.claude/agents/<role>.md`,`--agent` 自读;不注入。roleless = 不带 `--agent`(合法,非注入)。
- **`progress.jsonl` = state SoT**;chat 原文走 `<project>/.ccteam/chat/<sid>/turns.jsonl`(F1 后按 sid)。
- **不解析终端输出**(读 transcript jsonl + hooks;`tmux capture-pane`/screenshot 只读调试)。
- **永不主动 kill 长 session**:F1 去 dedup 后,create 不再因撞 (项目,role) 而 kill 旧 pane;`/role` re-spawn 是合法 turn;`project stop`/`rm` 是用户显式命令。
- **HITL 边界 = `PermissionRequest` hook**(per-session skip|hitl);hitl spawn 走 `--permission-mode default`(绝不 skip)。
- **cto 调度门 = daemon `role==cto` 硬门**(ambient `_caller_role`,不取 caller args)。
- **ccteam 不生成/桥接项目 `CLAUDE.md`/`AGENTS.md`**;**core 零 team 名字面量**。
- **pre-v1.0 不留技术债**:不写 backwards-compat shim;deprecated 直接删;breaking rename 不留 alias。
- **基线不退**:test pass ≥ 基线、fail=0;clippy 0;fmt 干净 —— 否则该阶段不算完成。

---

## 五、起手 30 秒
`git log -1`(确认 dev HEAD)→ 记基线(§〇 gate#1)→ 读 `prd.md` + `bug.md` → author workflow(§二 五阶段、opus subagents、阶段间 gate + review)→ 跑到底 → 收尾 gate + push。
