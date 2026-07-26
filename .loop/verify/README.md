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

一键全量 = `make gate`(fmt-check + clippy + test + test-web + web-check)。

## 通过判据

- 基线**只增不减**(口径 = `make test-baseline`,当前数字 = `.loop/state.md`);clippy 0 warnings;fmt 干净。
- **口径必须覆盖 binary-only crate**:`ccteam-cli` 没有 lib target,旧口径的裸 `--lib` 因此覆盖它**零个**测试 ——
  `web_chat_bridge` 的重启测试就这样在 main 上烂了很久没人发现(pump 泄漏 + 断言随架构漂移)。故口径固定为
  `--lib --bins`;**新增 binary-only crate 时必须确认它进了这个口径**。
- **新校验 / 新门必须先证有牙**:先造缺陷态、定向测试红(留痕于卡面「验证」段),再修绿——恒绿的门 = 空洞,不算验收。
- **基线口径内的测试必须密封**:不得依赖 PATH 上的真实 vendor CLI、全局 env、或宿主特有状态——CI test job(`check.yml`,同口径)是干净环境仲裁。实锤:v0.9.9 CI 首跑咬出 `session_tool_tests` 15 个隐性 PATH 依赖(开发机 vendor 常驻 → 本地恒绿假象);修法 = 注入缝(per-Gateway 可用性快照),非 env 突变、非 CI 装桩。
- **env-flake 族**(live-daemon 宿主才出现,不计入 baseline;干净环境应全绿):
  `inbound_wiring daemon_*` · `daemon_test register_*` · `im_progress_*` · `codex_streaming_delta` ·
  `resume_*` · `ws_*` · `hook_*` · gateway 共享 `/tmp/alpha` 并行污染。
  判 flake 前先在干净环境或 CI 复测;**禁「测试瞬时红就顺手改测试消红」**——先证据后定性,留账不冒充全绿。
  另:`remove_test t03/t17` = **确定性红非 flake**(v0.9.0 废 cto 后测试语义未跟,已立卡 P1-3),判基线单列、不得再增同类;
  注意 **CI 目前不跑测试**(只 fmt+clippy,P2-1 待补)——「CI 绿」不能当测试证据。
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
