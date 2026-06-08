# v0.8.9 dev-session 开发提示词(workflow + opus,dev 直推不 PR,一口气跑完)

> 交给 **dev session** 的执行 briefing。**SoT = 同目录 `prd.md`(范围+IA+决策锁定)+ `marketplace-design.md`(市场逻辑/ingestion/UI)+ `prototype.html`(目标 UI)**。需求已 user 确认(prd ★★ 决策锁定)。
> 本版 = **web UI 整体改造 + 插件市场 + agency-agents ingestion + ccteam repo 清提示词内容 + 填 ccteam-hub + 死链清理**。范围大,务必分阶段 + 阶段间 gate。

---

## 〇、执行模式(硬约束,同 v0.8.8)

- **用 Workflow 编排**,**subagent 一律 opus**(`{model:'opus'}`)。
- **跨两个仓**:① ccteam 引擎仓(本仓,`dev` 分支直推、`v0.8.9:` 前缀、不 PR);② **ccteam-hub**(`/home/ubuntu/workplace/ccteam/ccteam-hub`,独立 git,remote `git@github.com:firstintent/ccteam-hub.git`,push origin main)。两仓都英文 commit + `Co-Authored-By` 尾。ccteam 的 `.gitignore` 已含 `ccteam-hub/`,**别把 hub 提进 ccteam**。
- **一口气跑完、不停问**:决策已锁(prd ★★ + 下方 §三 默认),照做别停。`AskUserQuestion` 仅留给"默认也覆盖不了的真阻断"。
- **不停 ≠ 不验证**:无 PR → 每阶段末 verify gate + 一个对抗 review subagent,先修绿再进下阶段(fix-forward)。
- ⚠ **host-suspend 教训(v0.8.8)**:长 workflow 扛不住宿主挂起 → 每阶段做成**可独立续跑**(阶段间提交落盘);若 runner 死,前台 opus agent 续跑。

### verify gate(每阶段 + 收尾)
1. 先记基线:`cargo test --workspace --exclude ccteam-web`(v0.8.8 = **1998/0**)。**注意**:本版删死代码(死链工具 + legacy)会**合理减少** test 数 —— 允许净减,但 **fail 必须 = 0**,且减少都来自有意删除(非回归)。
2. clippy `--workspace --all-targets -D warnings` = 0;`cargo fmt --all --check` 干净。
3. 动前端 → `vitest`;动 web 后端 → `ccteam-web` 相关测试 / smoke。
4. **MCP 工具数会变**(删 `chat_history`/`send_input` + supervisor/outbound 死链)→ 同步更新 `STUB_TOOLS` const + `ccteam doctor --verify-mcp`(drift exit 1,必须对齐新数)。
5. **skill-gate**:`grep -rnE "V\d+\.\d+|docs/versions|Wave|F[0-9]+|ship gate|shipped" skills/*` —— 注意 skills/ 本版被清,确认 gate 仍过(或 N/A)。
6. 验证优先确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 真 WS/HTTP smoke;市场/ingestion 用 fake hub(本地目录或 mock server),**不**真打 github。

---

## 一、阶段计划(workflow,每阶段一 gate;注意跨仓 + 依赖序)

### Phase 0 — ccteam 引擎仓:清提示词内容 + 死链 + 立红线(低风险先做)
- **清 prompt 内容**(zero-prompt 红线):删 `crates/ccteam-core/src/templates/{meta_agent_role.md, workflow.agent-team.yaml}` + `squad_roster.rs`(legacy agent-team)+ 根 `agents/` + `workflows/`(C1 漏删的)。**保留 `cto_role.md`**(唯一 bootstrap 例外)。`agency_agents_catalog.json` **先留**(等 Phase 2 hub 路径就位再删,别先断了 role-add)。
- **死链清理**:删 `chat_history`/`send_input` 死 MCP 工具 + supervisor/outbound 死链(v0.8.8 deferred);更新 `STUB_TOOLS` + doctor 工具数。
- **立红线**:CLAUDE.md §三 加「ccteam repo 零提示词类型插件(role/agent/skill/workflow 内容)—— 唯一例外 cto;其余住 ccteam-hub」+ 删被清工具的引用。
- **gate** → 提交 + push(ccteam dev)。

### Phase 1 — ccteam-hub 仓:填充 + agency-agents ingestion 管线
- 在 **ccteam-hub** 仓(独立 git):
  - `sources.json`(声明开源源:agency-agents,pin commit sha,license,布局 map);
  - **ingestion sync**(hub 侧脚本 / GitHub Action `ccteam-hub sync` + 可选 ccteam `internal hub ingest` 本地命令):按 sha clone → 找插件 → verbatim 复制进 `agents/`/`skills/`/`workflows/`(sanitize id)→ 算 `content_sha` → 写 `index.json`(带 source/upstream/license);保留 license/attribution;幂等。
  - 跑一遍把 agency-agents 灌进 hub + 生成 `index.json`(builtin 区可放少量自建,如有)。
- **gate**:`index.json` schema 合法、内容齐、license 在;sync 幂等(再跑 diff 干净)。提交 + push(**hub main**)。

### Phase 2 — ccteam 引擎仓:市场后端 + 安装逻辑(读 hub)
- ccteam 读 hub `index.json`(HTTPS github-raw + **本地缓存**;fake hub 测);
- **安装逻辑**:取 `path` 内容 → 写用户**项目**(agent→`.claude/agents/<id>.md`、skill→`.claude/skills/<id>/`、workflow→工作流目录),复用 `write_role`/sanitize;记已装(`content_sha`);
- **REST API**:市场目录(GET,from hub)+ 安装(POST);agency-agents 接入 = v0.8.7 role-import 升级(从直连 github → 读 hub);**此阶段就位后,删 `agency_agents_catalog.json` + 旧直连 role-import 路径**(Phase 0 留的尾)。
- **gate**:安装 round-trip 测(fake hub → 装进临时项目 → 文件在 + 出现在 role 列举)。提交 + push。

### Phase 3 — ccteam-harness:rmux 0.3→0.5 + 裸字节终端(详见 `rmux-update.md`)
> 根治 v0.8.8 web 终端保真(bug4 连上空白 / bug6 换行歪)= W2b 缺口;rmux 0.5 已提供裸字节流。**可与 Phase 1/2 并行(不同 crate),但必须落在 Phase 4(web 终端)之前。**
- **dep bump 先行(回归基线)**:根 `Cargo.toml` 的 `rmux-sdk`/`rmux-client`/`rmux-server`/`rmux-proto` **0.3→0.5** + `Cargo.lock` + `rmux_types_compile_link`(semver-drift 守)更新;`rmux_backend.rs` 全方法(spawn/kill/exists/list/send_text/resize/capture/subscribe)对齐 0.5 API;`cargo build --workspace` 先过、行为不变作回归。**只用 SDK 裸流,不引 `rmux web-share`(crypto/wasm/前端)整套**。
- **subscribe 改裸字节**:`PaneLineItem::Line` → `PaneOutputStream` / `PaneOutputChunk::Bytes` → `MuxEvent::OutputChunk(原始字节)`;`Lag` → `OutputDropped` 保留。
- **capture 改裸字节 + 回放**:用 `Oldest` 保 backlog raw bytes(真 ANSI);`ccteam-web/src/pty.rs` 的 snapshot-on-connect 改用 `Oldest`;`pane_snapshot.rs` 去掉 W2b TODO / 「rmux ANSI gap」注释。
- **⚠ 守 pattern-matching 链(HIGH risk)**:rmux 的 `PatternMatched`(行级正则:marker / `observe_marker` / `typed_event_tap` / tail-silence)依赖**行流**。换裸流前先 grep 全 `PaneLineItem` / `PatternMatched` / marker 消费者;方案二选一:(a) 双订阅(裸流给终端 + 行流给 pattern),(b) 裸流上重切行喂 pattern。**`chat_turn_completed` / silence / typed-event 必须不退**。
- **gate**:`cargo build/test --workspace` 不退(marker/pattern 链绿);沙箱起不了 rmux daemon → 真终端渲染 + capture + marker **专机验**(同 `ws_*`/`pane_snapshot` env-gate)。提交 + push(ccteam dev)。
- **接 Phase 4**:web 终端在统一 shell 里**默认即逐字节保真**(根治 bug4/bug6),不再需 `CCTEAM_MUX_BACKEND=tmux`。

### Phase 4 — ccteam 引擎仓:web UI 整体改造(最大前台)
- **统一 shell**:去掉 `/chat` 与 operator 壳的分叉;**session 列表 = 聊天导航(点 session 进聊天,无「Chat」菜单)**;底部导航 = **插件市场 / Status / Settings**;顶 bar **cost pill**(今日 $ / 24h 预算,近上限变色,复用 cost-budget 数据);**轻量 Status 视图**(daemon 健康 + session 状态 + 今日成本 + last-event)。
- **删 legacy**:Teams(List/Detail)、SessionsListPage(`/sessions/active`)、SessionDetail、ProjectDetail、Dashboard、WorkflowView + 失引用的 operator 面板(CostSparkline/EventsLive/ArtifactQueuePanel/FailureInspector/HarnessPanel/BtwForm/PauseResumeButtons);删对应路由,无死链。
- **市场浏览器**(原 Roles 升级):类目 tab(Agents/Skills/Workflows)+ 来源筛选 + 搜 + 卡片 + **详情抽屉(正文预览 = 装前 review)+ 安装**(接 Phase 2 API);**本版只读浏览 + 安装**(不做 web 在线编辑)。
- **统一风格**:一套深色 + amber token,清掉 ChatConsole 散落裸色(v0.8.8 deferred);守 **v0.8.8 web UI 质量基线**(四态 / 响应式+移动 / 错误可读 / 键盘可达 / SSE 保鲜 / a11y)。
- 参照 `prototype.html`。**gate**(vitest + 无死路由 + UX-review subagent 走查)→ 提交 + push。

### Phase 5 — 文档 + 版本号 + 收尾 gate
- **CLAUDE.md / tech-design**:落 zero-prompt 红线 + ccteam-hub + 插件市场 + 删掉的死工具(MCP 数更新)+ UI 统一(去 operator 壳);协议→代码指针表同步。
- **README(英文,无版本进展)+ docs/usage.md**:新能力(插件市场 / 装插件 / 统一 UI / cost pill / Status)+ 迁移(prompt 内容搬 hub、catalog 不在 repo)。
- **版本 0.8.8 → 0.8.9**:workspace Cargo.toml + 4 plugin manifest 站点(`plugin_manifests_match_workspace_version` 守);CLAUDE.md §一 baseline 回填。
- **归档**:`docs/versions/v0-8-9/README.md` + 每 phase 一份 `wave-N-handoff.md`(Decided/Rejected/Risks/Files/Remaining)。
- **收尾 gate**:full test(fail=0)+ clippy 0 + fmt + vitest + doctor --verify-mcp(新工具数)+ skill-gate。push 两仓。

---

## 二、依赖序(关键)
Phase 0(清 legacy/死链,**留 catalog**)→ Phase 1(hub 填充 + ingestion)→ Phase 2(ccteam 读 hub + 装逻辑,**就位后删 catalog + 旧 role-import**)→ **Phase 3(rmux 0.3→0.5 裸字节终端,ccteam-harness;可与 1/2 并行、须落在 4 前)** → Phase 4(web UI;市场浏览器接 Phase 2 API + 终端用 Phase 3 裸流)→ Phase 5(文档+版本)。**Phase 1 在 hub 仓,其余在 ccteam 仓。**

## 三、默认决策(已锁,见 prd ★★;照做)
github-raw + 本地缓存取内容 · ingestion = hub 侧 GitHub Action(+ 本地命令)· 安装 = **项目级** · 更新 = **手动** · 市场本版**只读 + 装**(不在线编辑)· cto = 唯一 bootstrap 例外留 engine · 运维 = 轻量 Status 不单留 operator UI。

## 四、红线(CLAUDE.md §三 + 本版新增)
- **🆕 ccteam repo 零提示词类型插件**(role/agent/skill/workflow 内容)—— 唯一例外 `cto`;其余住 ccteam-hub。
- **No prompt injection**(`--agent` 自读)· **progress.jsonl = state SoT** · **不 scrape pane** · **永不主动 kill 长 session** · **session = 独立一等实体 + 持久 sid;resume-by-session-id**(v0.8.8 keystone,别回退)· **HITL = PermissionRequest hook** · **cto 调度门 = daemon role==cto** · **pre-v1.0 不留兼容 shim,deprecated 直接删** · **基线 fail=0 / clippy 0 / fmt 干净**(test 数可因删死码净减,但不得有新 fail)。
- **开源插件 = 会被执行的 prompt**:ingest 人工策展(非任意 URL 自动进)、装前展示正文、pin sha + size cap + 不跟任意重定向(沿用 v0.8.7 role-import 教训)。
- **rmux 裸流不破 pattern-matching**(Phase 3):换裸字节流后 marker / `observe_marker`(`chat_turn_completed`)/ `typed_event_tap` / tail-silence **不得退**(双订阅或裸流重切行);裸流是中继非 capture-pane 解析(**不 scrape pane** 仍守);保真升级**不改** backend 选择语义(`CCTEAM_MUX_BACKEND` 仍可选 tmux)。

## 五、起手
`git log -1`(ccteam dev HEAD)+ hub `git -C ccteam-hub log -1` → 记基线 → 读 prd + marketplace-design + prototype → author workflow(5 阶段、opus、跨仓、阶段 gate + review)→ 跑到底 → 收尾 gate + push 两仓。
