# 门禁地图(`.loop/verify/`)

> **「完成」的定义 = 可执行命令的退出码,不是任何会话的文字声称。**
> 可执行门禁的家 = **根 `Makefile`**(同一事实一个家,此处不复制脚本);本文件 = 「改动面 → 跑什么」
> 映射 + 通过判据 + 运行纪律。本目录唯一脚本 = `writeback.sh`(**队列结构校验**;治理写权执法 = 声明 + Fable 5 复核,**不做脚本硬防护**,AGENTS.md §五)。
> 维护者 = 规划(控制)会话(改门禁 = 改「完成」的定义)。

## 改动面 → 必跑

| 改动面 | 必跑 | 说明 |
|---|---|---|
| **任何收口(最低门)** | `cargo fmt --all -- --check` + `.loop/verify/writeback.sh` | fmt 是 CI required;writeback 见其头注 |
| Rust(非 ccteam-web) | 最低门 + `make check` + `make test` | clippy `-D warnings`;test = workspace 除 web,`--no-fail-fast` |
| 记基线数字时 | `make test-baseline` | 确定性口径(`--lib --bins`,排 `tests/*.rs` env-flake);**命令的家 = Makefile,勿在 `.loop/` 复制** |
| `crates/ccteam-web/src` | 上行 + `make test-web` | web 的 WS/PTY 测试需真终端 |
| SPA(`crates/ccteam-web/web`) | 最低门 + `make web-check` | vitest + tsc |
| docs / `.loop/` only(零代码 diff) | 最低门即可(免 cargo test) | 免跑失效条件:换机 / 换 toolchain / 动依赖解析后首轮必真跑 |
| `.github/workflows/` | CI 自证;须 SSH push(gh token 无 workflow scope,AGENTS.md §六) | |
| **性能敏感面**(progress 读写路径 / journal facade / ProgressProjection / gateway 锁范围 / status·projects·sessions 热端点) | 上行 + `make perf-gate` | perf-v1 门禁(2026-08-17 立):`CCTEAM_PERF_GATE=1` + release 编译,生成式 fixture(~177MiB/100 万行,含 torn UTF-8 + 尾部坏行;50 live/380 stopped;单会话 1 万 turns),普通 CI/test 零感知。判据:status p95<100ms 且投影零摄取字节(与文件大小无关的结构证明)· 每调用读放大 <10MiB · status 进行中 health p99<10ms · session list(50 live)p95<50ms · `tail_valid(200)`<50ms · 10k-history 默认页 <100ms · gateway 锁持有 p99<5ms。基准实测(88 核 dev 机,2026-08-17):25.43ms / 0B / 0.008MiB / 0.14ms / 42.32ms / 1.07ms / 1.47ms / 0.176ms |

一键全量 = `make gate`(fmt-check + clippy + test + test-web + web-check)。

## 通过判据

- 基线**只增不减**(口径 = `make test-baseline`,当前数字 = `.loop/state.md`);clippy 0 warnings;fmt 干净。
- **口径必须覆盖 binary-only crate**:`ccteam-cli` 没有 lib target,旧口径的裸 `--lib` 因此覆盖它**零个**测试 ——
  `web_chat_bridge` 的重启测试就这样在 main 上烂了很久没人发现(pump 泄漏 + 断言随架构漂移)。故口径固定为
  `--lib --bins`;**新增 binary-only crate 时必须确认它进了这个口径**。
- **新校验 / 新门必须先证有牙**:先造缺陷态、定向测试红(留痕于卡面「验证」段),再修绿——恒绿的门 = 空洞,不算验收。
- **`writeback.sh --selftest` 在 macOS 下自己坏**(2026-07-30 实锤):BSD sed 吃不下脚本里的 GNU sed 语法
  (`sed: 1: "/T1 示例卡/,/^$/{…": extra characters at the end of d command`),且**退出码仍 0**(自检失败静默)。
  本机替代实证 = 真实构造非法状态词跑一次无参 `writeback.sh` 应 RED(同日实测:状态词写成 `待排(watch 卡…)`
  → RED「状态词不在闭合集」→ 改回闭合集 `待排` → GREEN,比 selftest 更强的有牙证据)。修 selftest 属工具债,未立卡。
- **基线口径内的测试必须密封**:不得依赖 PATH 上的真实 vendor CLI、全局 env、或宿主特有状态——CI test job(`check.yml`,同口径)是干净环境仲裁。实锤:v0.9.9 CI 首跑咬出 `session_tool_tests` 15 个隐性 PATH 依赖(开发机 vendor 常驻 → 本地恒绿假象);修法 = 注入缝(per-Gateway 可用性快照),非 env 突变、非 CI 装桩。
- **env-flake 族**(live-daemon 宿主才出现,不计入 baseline;干净环境应全绿):
  `inbound_wiring daemon_*` · `daemon_test register_*` · `im_progress_*` · `codex_streaming_delta` ·
  `ws_*` · gateway 共享 `/tmp/alpha` 并行污染(perf-v1 收口(2026-08-17)再实锤两名成员:`gateway_resumes_dead_session_on_next_turn`(pending-turn 文件被并行测试先 drain,返回 `pending-drain:s1` 兜底值)与 `gateway_status_shows_real_vendor_resume_uuid` —— 两者都 `Gateway::new(fake,"alpha","/tmp/alpha")` 字面共享路径 + 撞 s1 sid 命名空间,隔离必绿、序列化 baseline 全绿;长线修法 = 这族测试改 tempdir,候选卡未立;v0.10.4 DSH2-RUST 收口(2026-08-25)再扩一名成员:`concurrent_replacement_discards_stale_resume_without_touching_newer_entry`——同模式同病,首轮全量 exit 0、二轮闪红、隔离 3/3 绿,与该波 dsh 物化改动零交集,修法归 TEST-HYG-1)(v0.10.0 ship 实锤扩容一族:`gateway::tests` 里 `turn_answer_carries_context_echo_for_focused_im_session` 与 `turn_answer_context_echo_omits_role_when_roleless` 在 PR #180 CI 连续两次独立触发(每次各一,从未同时红)——后者自己的代码注释已明写病根:`FakeAdapter::events()` 跨身份共享一个 `Notify`,并行跑多个 session 的 pump 可能把唤醒错发给还在等待的另一个测试的 session(**纯测试替身竞态,非生产 bug**);前者是 `context_echo_line` 的 `title` 参数(同 turn 内对 meta.json 的 best-effort 读)与「首条消息自动生成标题」异步写之间的窄窗口序竞态。两者均与本次改动(零 touch `gateway.rs`/`ccteam-im`)无关,本机隔离/全量套件复现不了、CI 独立重跑即绿,判定同族,不改测试/不改 gateway.rs——真正修法是给 `FakeAdapter` per-session `Notify`,留作 dev 会话候选卡)。
  (2026-08-18 v0.10.3 A/B 收口再实锤:`gateway_resumes_dead_session_on_next_turn` 在本机 `make test-baseline` 全并行下已**近确定性红**(pristine origin/dev 双复现 + dev 会话 3/3),隔离恒绿;已升级立卡 TEST-HYG-1,勿再仅登记。同日全并行下另见三张一次性负载面孔,均隔离/独立复跑绿:`delegation_budget_gate_denies_and_emits` ×2、`gateway_plain_message_submits_to_current_session_and_echoes`、`gateway_status_acl_is_own_only`(以上 gateway::tests 同族)+ ccteam-cli `web_chat_bridge::web_chat_newproject_scaffolds_registers_and_cd_works` ×2(两次 panic 点不同 622/882,cli bin 独立 3/3 绿)——非 gateway 族的 web_chat 面孔若再现升独立条目。)
  **hot-config 同秒改写族(候选,2026-08-19 RESTART-1 收口 1 次实锤)**:`gateway::tests::remote_fake_host_one_turn_resume_and_host_stamp` 在全并行 lib 跑一次红(`plan_resume_dead_session` 未拒 rebind —— 测试在同一秒内两次写 `config.yaml`,疑 `HotConfig` 按 mtime 缓存读到旧绑定);隔离 3/3 绿、全量重跑 2/2 绿、与当次改动零代码交集;未改测试。再现时先查 mtime 粒度,修法候选 = 测试里改 config 后 touch 成不同 mtime 或 HotConfig 比对 `(mtime,len)`。
  **env-mutating lib 测试对(2026-08-18 A 会话 pristine 树 3/10 复现,归 TEST-HYG-1)**:`ccteam-harness` 内
  `model_catalog::env_resolution_prefers_ccteam_home_and_falls_back_to_home` 与 `dsh_acp::spawn_spec::tenant_web_seed_refreshes_unmodified_files_from_operator_home` 同进程互踩 `HOME`/`CCTEAM_HOME`(正是 AGENTS §六「env-mutating 测试放 integration」红线的违例),成对红、`--test-threads=1` 恒绿。
  判 flake 前先在干净环境或 CI 复测;**禁「测试瞬时红就顺手改测试消红」**——先证据后定性,留账不冒充全绿。
  **`resume_*` / `hook_*` 已于 2026-07-28 出族(根因定死、非 flake)**——两族都是「宿主态泄漏」的具体形态,登记在册反而掩盖了确定性缺陷:
  ① `claude_stream_json_test resume_failure_*` 读 progress 路径漏了 `state/` 段(**永远读空文件**,处处确定性红);
  它 panic 后不再执行自己的 `remove_var("FAKE_SJ_DIE_ON_RESUME")`,泄漏的 fault 开关又杀掉下一只 serial 的 resume spawn
  → **一只真红伪装成两只 flake**。修法:路径修正 + 所有 fault 开关一律在 `setup()` 清(panic-safe)。
  ② `hook_script_test` 三只(两红一**挂死**)= 进程继承了 ccteam 托管会话自己的 `CCTEAM_HOOKLESS=1`,
  `hook.sh` 据此 `exit 0` → 桩从不被调用、一次性 listener 无人连接。修法:spawn 前 `env_remove` 整族 `CCTEAM_*` 会话变量。
  **教训**:「只在 live-daemon 宿主红」≠ flake —— 先问「宿主给了它什么」(env / 端口 / 真实 home),十有八九是测试没密封。
  另:`remove_test t03/t17` = **确定性红非 flake**(v0.9.0 废 cto 后测试语义未跟,已立卡 P1-3),判基线单列、不得再增同类;
  ~~注意 CI 目前不跑测试(只 fmt+clippy,P2-1 待补)~~ **已作废(2026-07-30 核 `check.yml` 实况)**:
  `check.yml` 三 job = `fmt` + `clippy -D warnings` + **`test`(`cargo test --workspace --exclude ccteam-web --lib --bins --locked`
  = 与 `make test-baseline` 同口径)**,故「CI 绿」**是**基线口径的干净环境证据(与本文件上方「CI test job 是干净环境仲裁」一致;
  旧句陈旧,会误导后来会话放弃现成仲裁)。真正的限制在**触发面**:`on: push[main] + pull_request[main]` ——
  **推 dev 本身不触发任何 job**,只有开着 dev→main PR 时才跑(AGENTS §五「周期开始即开 draft PR」正是为此)。
  ⇒ 无 PR 期间推 dev 的提交**零 CI 覆盖**,基线数字只有本机口径,须在 PR 开启后回填仲裁值。
- **macOS 宿主两族**(2026-07-26 ae24cb3 review 实锤,均先于该 commit、Linux CI 不受影响;修卡 = TEST-MACOS-1):
  ① `ccteam-core roles::list_library_skills_is_recursive_hidden_safe_and_sorted` —— **在 baseline 口径内**,TMPDIR 形状敏感:
  scanner `fs::canonicalize` 把 `/var/*` tempdir 解析成 `/private/var/*`,测试却按字面 tempdir 断言 path;默认 shell
  (`TMPDIR=/var/folders/…`)确定性红、TMPDIR 已 canonical 的会话绿(state.md「本机全绿」与新会话红并存的成因)。判基线单列。
  ② `ccteam-harness codex_app_server_test` 9 只 `SUN_LEN` 红 —— macOS UDS socket 路径超长(长 TMPDIR + tempdir 嵌套),
  测试基建问题;在 `tests/*.rs`,**不在 baseline 口径**。

## 运行纪律(教训固化区;新教训从卡面「经验」行蒸馏进来)

- **控制会话需要 telegram/MCP 存活时,勿在主仓跑 cargo**:control 会话的 MCP 跑在 `target/debug/ccteam` 上,
  cargo build/test/clippy 重建二进制即掉线(实锤 ~25min 断联)。重活进独立 worktree 跑;docs-only 改动不需重跑门禁。
- **测试隔离必须同时 pin `HOME` + `CCTEAM_HOME`**(只指 HOME 不够,实锤 fixture 污染真实 registry;AGENTS.md §六)。
- **输出过滤后读**(token 纪律):test 只看 `test result` 行、clippy/lint 看尾行;红了再放宽定位;大文件 grep 定位后按段读。
- **等待 = 条件轮询 + 显式超时,禁裸 sleep**;进程/e2e 类门:同一命令连续 3 次全绿 + 前后进程零残留才算稳定绿。
- **SPA Sidebar 每工作区有 WS_SHOW 行数上限**——扩 vendor/session 测试行须跨 project 摆放,否则被折叠断言假红(V095 经验)。
