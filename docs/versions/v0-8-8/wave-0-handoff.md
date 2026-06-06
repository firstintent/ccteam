# v0.8.8 Phase 0 handoff — 清理 + 独立 bug(C1 + B1 + B2 + B3)

> commit `038633c`(已 push origin/dev)。Gate:`cargo test --workspace --exclude ccteam-web` **1977/0** · clippy 0 · fmt 干净 · SPA vitest **120/120**(基线 111,+9 新用例)。
> 编排:Workflow `v088-phase0`(recon ×3 + impl:C1 全 opus 子代理)→ B1/B2B3 因 API 529 风暴落到主控收尾(见 Risks)→ 主控跑权威 gate + 对抗 review(opus,verdict=pass)。

## Decided(已定 + 落地)
- **C1 删除范围按 recon 实证收敛**(代码为 SoT):删 `teams/`、`skills/`(含 .gitkeep)、`examples/`、根 `config.yaml`(smoke artifact,+ `.gitignore /config.yaml`)、`workflows/qa-autoloop/`、`agents/explorer.md`、`tests/intent-corpus.yaml`、`scripts/host-probe/intent-accuracy.sh`(其唯一用途是测已删的 `skills/ccteam/SKILL.md` NL dispatcher,未进 CI)。`commands.rs` 删掉 `DEFAULT_WORKFLOW_YAML` 里悬空的 `# Examples: examples/workflows/*.yaml` 注释。
- **B1 / BUG-1**:`stop_project_chat_sessions` 改为接收注入的 `&dyn ProcessBackend`,经 `default_process_backend()` + `list_chat_sessions` + `backend.kill()` 枚举/杀,去 tmux-only → 默认 rmux backend 下 `project stop`/`rm` 真生效。slug 过滤用 `parse_chat_session_name` 第一元素(forward-compatible 到 F1 的 sid 改键)。harness 层加确定性测试(单 InProcBackend:dev-foo×2 + dev sibling + non-chat → 列/杀/absent + 幂等),CLI 层加 bridge smoke;`remove_test` 的 t15/t18 pin `CCTEAM_MUX_BACKEND=tmux`(它们 seed 真 tmux)。
- **B2 / BUG-2**:web 新建弹窗恢复「＋ 新建项目…」,走 **REST `POST /api/v1/projects`**(新 `createProject` helper,409 读 JSON error body 给人话),**不**走旧 WS `/newproject`;201 后复用既有 `createSession`(refreshSessions + navigate)自动选中起 session。前端 slug/path 校验镜像后端。
- **B3 / BUG-4**:role 字段改为从 `GET /projects/{slug}/roles` 拉的实时下拉(`listProjectRoles`),展示 role + description;`ROLE_SUGGESTIONS` 保留作 fallback(守 FIX-2)。UI 质量基线:四态、toastBus 人话错误、提交防重复、Esc/Enter。
- **B3 归 Phase 0**(reconcile):dev-prompt 的 Phase 2「B3 验证」实为 BUG-3 历史串台的验证(F1 交付);PRD 的 B3=BUG-4 role 下拉与 F1 无关、与 B2 同改 NewSessionModal,故并入 Phase 0 一次做完(减少同文件多阶段重复改)。

## Rejected(否决 + 因由)
- **删 `workflows/`(整目录)**:否。`crates/ccteam-flow/tests/dev_flow_template_parses.rs` 是 LIVE cargo test,读 `workflows/dev-flow/workflow.yaml` 并 panic-if-missing → 删它会退基线。只删了无引用的 `workflows/qa-autoloop/`,保留 `workflows/dev-flow/`。
- **删 `agents/`(整目录)**:否。`crates/ccteam-cli/src/commands.rs` 有 `include_str!("../../../agents/__lead.md")`(硬 build 依赖,支撑 LIVE `ccteam init --mode agent-team`,temp-rename 实证 build break)。只删了无引用的 `agents/explorer.md`,保留 `agents/__lead.md` + `agents/README.md`(后者仍被 doc-comment 引用)。
- **改 `config_path`/`from_env` 修 config.yaml CWD 泄漏**:否(不 rabbit-hole)。`CcteamPaths::from_env()` 行为本就正确(从不回退 CWD;泄漏是某次 smoke 显式 `CCTEAM_HOME=<repo>` 所致,非代码缺陷)→ `git rm` + `.gitignore` 兜底足矣。

## Risks(残留 + 监控)
- **529 风暴**:Phase 0 workflow 的 impl:B1 / impl:B2B3 / verify / review 四个子代理撞 API 529 Overloaded。实查:impl:B1 的文件编辑其实已落盘(529 死在最后 emit StructuredOutput 时),tree 编译通过 + 新测全绿,故 B1 保留采用;impl:B2B3 未落任何编辑(纯失败)→ 主控用单个 opus Agent 按 recon spec 重做 B2/B3。**教训**:workflow 子代理 529 可能留"已编辑未汇报"的半成品,需 git status 实证再决定保留/重做。后续阶段(F1/web)继续用 workflow,但保留主控收尾兜底。
- **eslint +1**:B2/B3 引入 1 处 `react-hooks/set-state-in-effect`(role-fetch loading 态),与既有 13 处同形(per-sid history seed 等),**非 CI-gated**(CI 只跑 cargo fmt/clippy/test;release 跑 npm ci + cargo build)。不阻塞;若后续要清零 eslint 一并处理。
- **CLAUDE.md doc drift**(review 唯一 low):§四 仍把 `skills/.gitkeep` 描述为"项目自有 skill 扩展位",§三 ship-gate 引用 `skills/*/SKILL.md`(grep 仍 0 命中、通过)。skills/ 已整删 → **Phase 4 文档同步**修正。

## Files(改了什么)
- 删:`teams/` `skills/` `examples/` `config.yaml` `workflows/qa-autoloop/` `agents/explorer.md` `tests/intent-corpus.yaml` `scripts/host-probe/intent-accuracy.sh`(+ `.gitignore` 加 `/config.yaml`)。
- B1:`crates/ccteam-cli/src/commands.rs`(stop_project_chat_sessions 注入 backend + 两调用方 + CLI smoke + 删 Examples 注释)、`crates/ccteam-harness/src/lib.rs`(harness 确定性测试)、`crates/ccteam-cli/tests/remove_test.rs`(t15/t18 pin tmux)。
- B2/B3:`crates/ccteam-web/web/src/lib/dashboardApi.ts`(createProject)、`…/lib/sessionsApi.ts`(listProjectRoles + RoleSummary)、`…/pages/ChatConsole.tsx`(NewSessionModal 重写)、`…/lib/{dashboardApi,sessionsApi}.test.ts`(+9 vitest)。

## Remaining(留给后续阶段)
- **CLAUDE.md §四 / §三** 对 skills/.gitkeep 的过时描述 → Phase 4。
- BUG-4 的"空 role roleless"项 → F2(Phase 2,依赖 F1 session 身份)。
- 其余全部 = Phase 1+(F1 keystone → B3-verify/B4/B5/F2/F3 → F4/F5 → docs)。
