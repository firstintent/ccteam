# Rust 重写 tmux / 内嵌 tmux 可行性调研

> **类型**:`docs/research/`(扩展研究,不更新,按需加载) — 见 `docs/README.md` §三类
> **作者**:claude session on `claude/rust-tmux-rewrite-feasibility-pD3EB`
> **基线版本**:V0.6.7(2026-05-25)
> **问题来源**:用户询问 — "调研重写一个 rust 版本的 tmux 可行性,或者把 tmux 内嵌到 ccteam 的可行性"
> **结论先行**:**Option A(Rust 原生 mux,daemon + UDS + portable-pty + vt100)可行,推荐为 V0.7 minor 候选**;Option B(内嵌 tmux 二进制)劣于现状,不推荐;先做 Option C(install.sh 自动装 tmux)做 V0.6.x patch 兜底。

---

## 一、ccteam 当前 tmux 使用 surface 摸底

### 1.1 调用集中度(好消息:边界小)

| 文件 | LOC | 角色 |
|---|---|---|
| `crates/ccteam-core/src/tmux.rs` | **472** | 唯一抽象层 `TmuxSession` struct + 几个游离 `capture_pane_*` 函数 |
| `crates/ccteam-web/src/pty.rs` | **368** | Web SSE 用 `tmux pipe-pane` 单向 FIFO 订阅(F56) |
| `crates/ccteam-cli/src/commands.rs` | (~10 行直调)| `ccteam attach` 走 `tmux attach -t <name>`、`ccteam peek` 走 `capture-pane -p` |
| `crates/ccteam-core/src/execution/codex_exec.rs` | (~2 行)| Codex 适配器 fallback `send-keys` |

**总 surface**:~840 LOC Rust + ~12 行散落,集中度高,**没有散落的 `Command::new("tmux")` 在业务逻辑**。

### 1.2 真实使用的 tmux 子命令集合(全集 11 个)

| 子命令 | 用途 | 替代难度 |
|---|---|---|
| `new-session -d -s <name> -x 200 -y 50 <argv>` | 创建 detached 长 session | ★★ 中(daemon fork + PTY allocate)|
| `send-keys -t <name> <text>` / `Enter` | 注入 keystroke 到 pane stdin | ★ 易(PTY master write)|
| `has-session -t <name>` | 检查 session 存活 | ★ 易(daemon UDS query)|
| `kill-session -t <name>` | 正常关闭 session | ★ 易(SIGTERM child + cleanup)|
| `list-panes -t <name> -F '#{pane_pid}'` | 取 child PID 用于 `kill -0` 活性双校 | ★ 易(daemon 自知 child PID)|
| `display-message -p -t <name> '#{pane_pid}'` | 同上 | ★ 易 |
| `display-message -p -t <name> '#{pane_width}/#{pane_height}'` | screenshot 渲染前查屏幕尺寸 | ★ 易(daemon 持有 PtySize)|
| `capture-pane -p -t <name> -S -<N>` | 取末尾 N 行文本 | ★★ 中(需 scrollback ring + vt100 dump)|
| `capture-pane -p -e -t <name>` | 同上但保留 ANSI(给 vt100 + imageproc 渲染 PNG)| ★★ 中(同上,保留 raw bytes)|
| `pipe-pane -t <name> -o 'cat > <fifo>'` | Web 单向 FIFO 流(F56)| ★★ 中(daemon 广播 stdout to N 个 broadcast::Sender)|
| `attach -t <name>` | 用户 `ccteam attach` 交互重连 | ★★★★ **重点** — 见 §3.3 |
| `tmux -V` | 版本检测 | ★ 易(不需要)|

**核心红线已编码**:`capture-pane` 出来的字节**仅用于 vt100 渲染 PNG 和 CLI 显示**,**不入业务状态机**(状态机唯一 SoT 是 `progress.jsonl` + Claude Code 官方 hooks)。这条红线让"换底层"风险大幅降低 — 只要保证 stdin/stdout 字节流相同,业务面零感知。

### 1.3 测试现状

- `ccteam-core/tests/tmux_test.rs` 直调真 tmux,`skip_if_no_tmux()` 失活
- **无 `CCTEAM_TMUX_BIN` env override**(对比 `CCTEAM_CLAUDE_BIN` / `CCTEAM_CODEX_BIN` 有)
- 单元/集成测试对 tmux 是黑盒依赖。换底层需补一个 `MuxBackend` trait 让测试 inject fake

### 1.4 现有 Rust 生态资产(已在 Cargo.toml,**复用不需引入新依赖**)

| crate | 当前用途 | 重写时复用点 |
|---|---|---|
| `vt100 = "0.15"` | F38 screenshot 渲染 | 直接做 in-memory 终端状态机,`capture` 时 dump 任意行数 |
| `portable-pty = "0.8"` | F56 占位声明,**当前未实际使用** | WezTerm 抽象,跨 unix/macOS/**Windows ConPTY** 一致 API |

也就是说,**底层零件全部已在 build tree 内**,主要工作是组装 daemon + 协议 + attach client。

---

## 二、tmux 本体特性 ccteam 用了哪些 / 不用哪些

| tmux 特性 | ccteam 是否依赖 | 替代成本 |
|---|---|---|
| **detached long session(survive client disconnect)** | **依赖**(mode 3 chat 24/7 跑 `claude` TUI)| ccteam-mux daemon 必须实现 |
| **PTY 分配 + child process supervision** | 依赖 | portable-pty 覆盖,小 |
| **多 client attach**(一个 session 多终端窗口同时看)| `ccteam attach` 单用户,但同 session 可能被 dev 用 `tmux attach` 多窗口观察 | 中等 — daemon 广播 stdout + 处理多 client stdin 仲裁 |
| **scrollback ring** | screenshot + peek 取末 N 行(实际 N≤200)| vt100 自带 scrollback,小 |
| **pane splits / windows / layouts** | **不用**(每 session 单 pane)| 不实现 |
| **mouse / copy mode** | 不用 | 不实现 |
| **status bar / 自定义 key bindings** | 不用 | 不实现 |
| **`.tmux.conf` 用户配置融合** | **看场景**:`ccteam attach` 进去后用户期望自己的 tmux 键绑生效(`prefix + d` 等)| **重写后丢失** — 需 README 明确说明 |
| **session 名 namespace** | 用 `ccteam-chat-<slug>-<role>`(F172)区分 | daemon 内部 HashMap 即可 |
| **`tmux -CC` control mode** | **不用**(ccteam 走 CLI 子命令,不走 control mode)| 不实现 |
| **server/client 协议跨 socket 共享** | tmux 的 default socket `/tmp/tmux-<uid>/default` 全局共享 | ccteam-mux daemon 走 `~/.ccteam/run/mux.sock` 私有,清晰 |

**结论**:ccteam 用了 tmux 的 ~25% 表面积,且不复用任何 tmux **用户层** UX。重写最大风险点是 §三的 `ccteam attach`。

---

## 三、三种方案逐项评估

### Option A:Rust 原生 mux(自己写一个 ccteam-mux)

**架构骨架**:

```
┌─────────────────────────────────────────────────────────────┐
│ ccteam-mux daemon (新 binary, single process per user)       │
│ ─────────────────────────────────────────────────────────── │
│ • tokio runtime                                              │
│ • HashMap<SessionName, MuxSession>                           │
│   ├── child: portable_pty::Child                             │
│   ├── master: Box<dyn MasterPty> (PtyMaster file)            │
│   ├── parser: vt100::Parser (scrollback + screen state)      │
│   ├── stdout_tx: broadcast::Sender<Bytes>                    │
│   └── attached_clients: Vec<ClientId>                        │
│ • UDS server @ ~/.ccteam/run/mux.sock                        │
│   ├── StartSession { name, argv, cwd, size } → ()            │
│   ├── SendKeys { name, bytes } → ()                          │
│   ├── HasSession { name } → bool                             │
│   ├── KillSession { name } → ()                              │
│   ├── Capture { name, lines, ansi } → Bytes                  │
│   ├── Subscribe { name } → stream<Bytes>  (Web pipe-pane)    │
│   └── Attach { name, client_size } ⇆ bidi  (ccteam attach)   │
│ • 自动启停:首次 SendKeys/StartSession 时 systemd-style       │
│   socket-activation 由 ccteam CLI 唤起;空闲 N 分钟无 session │
│   时自杀(可选)                                              │
└─────────────────────────────────────────────────────────────┘

ccteam orchestrator / web / cli  ─UDS protocol─►  daemon
                                                    │
                                                    ▼
                                          spawn `claude` TUI in PTY
```

**实现 LOC 估算**(基于 wezterm-mux / mosh 参考):

| 模块 | 估 LOC | 备注 |
|---|---|---|
| `MuxBackend` trait + `TmuxBackend` 现状包装(过渡用)| 200 | 抽象层,让测试 inject |
| `NativeBackend` daemon main loop + tokio scheduler | 400 | |
| PTY child supervisor(spawn / waitpid / size resize)| 300 | portable-pty 之上薄包装 |
| vt100 screen + scrollback ring + capture helpers | 200 | vt100 自带,主要做 lines/ansi 切片 |
| UDS RPC 协议 + serde 编解码 | 400 | tarpc / 自卷 length-prefixed JSON |
| `Subscribe` broadcast 桥(替代 pipe-pane FIFO) | 200 | tokio::sync::broadcast |
| **`Attach` client**(替代 `tmux attach`,**最重**)| **800** | crossterm raw mode + keystroke 转发 + resize 信号 + 退出热键(`Ctrl-b d` 等价) |
| 单元/集成测试 | 1500 | 2× 实现 LOC,含 PTY fixture |
| **总计** | **~4000 LOC** | 约 V0.4.0 一个 wave 体量 |

**时间估算**:1 个 minor 版本(V0.7)wave-by-wave 推,~6-8 周专注 + cross-platform CI 调试。

**Pros**:
1. **彻底消除安装摩擦** — 不再依赖系统 tmux(V0.6.6 F166 install.sh 痛点的根治)
2. **解锁 Windows 原生支持** — portable-pty ConPTY 路径已经成熟;不再"Windows 走 WSL2"(CLAUDE.md 红线松绑)
3. **协议清晰可测** — UDS RPC 是 typed,不再 shell escape `send-keys`(F172 V2 设计里的痛点)
4. **零 SHELL injection 风险** — 现状 `send-keys` 走字符串拼接,有过 escape edge case;改 typed bytes 直接 PTY write
5. **可观测性提升** — daemon 内 metrics(每 session CPU/mem、PTY buffer 高水位、attach client 数)直接出 prometheus / `/.ccteam/run/mux-stats.json`
6. **与现有架构一致** — daemon + UDS 模式与 cost ledger / IM supervisor / web SSE 是同一套路
7. **复用已在 deps 的 vt100 + portable-pty** — 零新依赖,workspace 体积不涨
8. **支持 daemon 跨 ccteam 重启存活** — 现状 tmux 已经这样;ccteam-mux daemon 走 unix double-fork 或 systemd user unit 即可
9. **代码替换尺度可控** — surface 11 命令对应 ~10 个 RPC method,**红线"业务面零感知"** 一次到位

**Cons & 风险**:
1. **`Attach` client 是真正的 hard problem** — 必须正确处理:键盘转发(包括 `Ctrl-c` 不能误吞)、SIGWINCH resize、终端模式切换(raw/cooked)、退出热键、TERM 环境变量传递、UTF-8 多字节边界、bracketed paste。**估计 60% 的实现风险集中在此**
2. **多 client 同 session attach 仲裁** — tmux 的"两个窗口看同一个 session"是经典 dev 流程。daemon 需广播 stdout + 仲裁 stdin(默认所有 client stdin 都送 PTY,可能造成"两人同时敲"乱)
3. **用户 `.tmux.conf` 习惯丢失** — `prefix + d` detach 之类的肌肉记忆要重新学。需在 `ccteam attach --help` 明确文案
4. **终端兼容性长尾** — `claude` TUI 用了什么 escape sequence,我们的 vt100 0.15 是否全支持?需 reference 实测。已知 vt100 不实现 sixel graphics、不实现某些 DEC private modes。Claude TUI 主要用 alacritty/iterm2 子集,大概率 OK,但需 regression test 兜底
5. **Windows ConPTY 已知 quirk** — portable-pty 上游缺 PSEUDOCONSOLE_RESIZE_QUIRK 等 flag(见 Sources);可能需要 fork 或 patch
6. **多 OS CI 矩阵成本** — 现在测 linux + macOS,加 Windows 后 CI 时间 +50%
7. **维护负担** — 一旦上线,任何 PTY/终端 bug 都是我们的 issue,不能甩锅"系统 tmux 版本太老"
8. **失去 tmux 社区生态** — `tmuxinator` / `tmux-resurrect` / 用户自己的插件全失效(ccteam 用户不太可能依赖,但需文档说明)

**风险缓解**:
- **Phased rollout**:V0.7.0 引入 `MuxBackend` trait + `NativeBackend` 走 feature flag,默认仍 `TmuxBackend`;V0.7.1 flip 默认,tmux 留作 fallback;V0.8 移除 tmux 路径
- **Attach 客户端先跳过**:V0.7.0 daemon-side 完整,但 `ccteam attach` 仍走 tmux(只对没装 tmux 的用户禁用 attach,要装 tmux 才能 attach)。V0.7.x patch 才补 Native attach
- **Cross-platform 增量**:V0.7.0 仅 linux+macOS;Windows 放 V0.8

---

### Option B:内嵌 tmux 二进制(vendor + ship)

**做法**:在 GH Releases 打包预编译 tmux 到 `ccteam` tarball,运行时设 `CCTEAM_TMUX_BIN=<libexec>/tmux`。

**Pros**:
1. **零代码改动** — 仍走当前 `TmuxSession`,业务面无感
2. **去除安装步骤** — `install.sh` 不再需要 `apt install tmux`
3. **版本锁定** — 不再被用户系统的老 tmux 拖累(`-X` flag 之类的 version-dependent 行为)

**Cons**:
1. **不解决 Windows** — tmux 本质不跑 Windows;还是要 WSL2,这是 ship-blocker 类问题
2. **打包复杂度爆炸** — tmux 依赖 libevent + ncurses(还要 yacc/bison build-time)。要打 musl static 4 个 target(linux-{x64,arm64} musl + macOS-{x64,arm64})得各自配 cross-toolchain
3. **二进制膨胀** — tmux ~500KB,libevent + ncurses static 加 ~1.5MB,**ccteam release tarball 体积涨 4 倍**(V0.6.7 当前 ~600KB → ~3MB)
4. **PATH 冲突 / 调试地狱** — 用户 `which tmux` 显示系统的、ccteam 用的是 vendored 的、`ccteam attach` 启的 vendored、用户自己 `tmux attach` 走系统的 — **两个 tmux 看不到对方的 session**(socket 不同)。文档要解释这个差异点
5. **License 摩擦小但需声明** — tmux 是 ISC 兼容 MIT,需在 release notes / `THIRD_PARTY_LICENSES` 加条目
6. **每次 tmux 上游升级要同步 build pipeline** — 维护负担
7. **不解锁任何新能力** — 纯粹"少一步 apt install",ROI 低
8. **与 ccteam 红线"不 vendor Claude / Codex"在风格上不一致** — CLAUDE.md §三"vendor 红线"明确不 vendor 外部 binary,内嵌 tmux 违反风格

**结论**:**Option B 是最差选项** — 拿到了 Option A 的工程负担、却没拿到 Option A 的收益(Windows + 协议清晰 + 可观测)。

---

### Option C:status quo + install.sh 自动装(最便宜兜底)

**做法**:V0.6.x patch 在 `install.sh` 检测 tmux,自动 `brew install tmux` / `apt install tmux` / `pacman -S tmux`,失败时给 graceful 引导。

**Pros**:
1. **0.5 天工**
2. **不引入任何新 surface**
3. **解决"`ccteam doctor` 报 tmux missing"的入门痛点**

**Cons**:
1. **不解决 Windows**
2. **sudo 提示对部分用户(restricted 公司机)是死锁**
3. **NixOS / 老 CentOS / 偏门发行版** 包管理器不一致,脚本要维护一组分支

**结论**:**作为 Option A 落地前的兜底**,合并到 V0.6.8 / V0.7.0 早期。

---

### Option D:混合 — `MuxBackend` trait + tmux/native 双 backend(渐进策略)

实际上这是 Option A 的推荐落地形态(见上 Phased rollout)。**不是独立方案**,是 Option A 的 risk-mitigation 包装。

---

## 四、参考的现有 Rust 资产对比

| 方案 | 适不适合 ccteam | 备注 |
|---|---|---|
| **portable-pty** (wezterm) | ✅ **核心零件** | 跨 unix/macOS/Windows ConPTY 一致 API;已在 deps |
| **vt100** | ✅ **核心零件** | 已在 deps;screen + scrollback 抽象足够 |
| **alacritty_terminal** | ⚠️ 备选 | 更全(支持 sixel 等),但更重;vt100 应足够 |
| **termwiz** (wezterm) | ⚠️ 备选 | 含 `tmux_cc` control-mode client;如果选 Option A 但保留 tmux 兼容,可用它替代 CLI 子命令更稳 |
| **tmux_interface / tmux_lib** | ❌ 不适 | 仍走 `Command::new("tmux")` CLI,只是更 typed,**不解决任何根本问题** |
| **Zellij** (binary) | ❌ 不适 | Rust 写的,但是个**完整 multiplexer 产品**,不能作为 library 嵌入;走它 = 把 tmux 依赖换成 zellij 依赖,问题平移 |
| **r3bl_tui** | ❌ 不适 | TUI framework,不是 multiplexer |
| **Tmux Automation MCP Server** | ❌ 不适 | 一个外部 MCP 服务,ccteam 自己就是 MCP server,不可能在内部嵌 |

---

## 五、决策矩阵

| 维度 | 现状 (system tmux) | Option A (Rust native) | Option B (vendor tmux) | Option C (auto-install) |
|---|---|---|---|---|
| 实现成本 | 0 | **~4kLOC / 6-8w** | ~1w 打包工程 | ~0.5d |
| 安装摩擦 | 中(`apt install`)| **无** | 无 | 低 |
| Windows 原生 | ❌ 不支持 | ✅ **支持**(ConPTY) | ❌ 不支持 | ❌ 不支持 |
| 协议清晰度 | 低(shell escape)| **高**(typed RPC)| 低 | 低 |
| 可观测性 | 弱(grep tmux ps)| **强**(daemon metrics)| 弱 | 弱 |
| 终端兼容风险 | 0(用 tmux)| **中**(自维 vt100 路径)| 0 | 0 |
| 维护负担 | 0(系统包)| 中 | 高(cross-compile)| 低 |
| Release tarball 体积 | ~600KB | ~700KB(+100KB Rust deps)| ~3MB | ~600KB |
| 与 ccteam 红线吻合 | OK | **强吻合**(daemon+UDS 同套路)| 弱(vendor 风格冲突)| OK |
| 用户 `.tmux.conf` 习惯 | ✅ 保留 | ❌ 丢失 | ✅ 保留 | ✅ 保留 |
| 多 client attach | ✅ tmux 原生 | ⚠️ 需实现 | ✅ 保留 | ✅ 保留 |

---

## 六、推荐路径

### 短期(V0.6.8 patch,0.5 天)
**Option C**:`install.sh` 加 tmux 自动检测 + 装,失败给 actionable 引导。**今天就能做**,无技术风险。

### 中期(V0.7.0 minor,6-8 周专注)
**Option A 走 phased**:

| Wave | 交付 | 验收 |
|---|---|---|
| W1 doc-first | PRD + dev-plan(本文是种子)+ MuxBackend trait 设计 + UDS 协议草案 | architect/cc-expert/codex-expert 三方评审过 |
| W2 trait + 现状包装 | `MuxBackend` trait + `TmuxBackend` 现状重构入 trait + 所有调用点改走 trait + 测试 baseline 不退步 | 1639/1 保持 |
| W3 NativeBackend daemon + RPC | daemon binary + UDS server + 5 个 RPC(Start/Send/Has/Kill/Capture)+ vt100 scrollback + portable-pty 集成 | 单元 + 集成测试 mock UDS 全通 |
| W4 broadcast 替代 pipe-pane | `Subscribe` RPC + tokio broadcast + Web pty.rs 改写走 native | web 现有 e2e 通过 |
| W5 feature flag + 双跑 | `CCTEAM_MUX_BACKEND={tmux,native}` env;默认仍 tmux;ccteam-creator 启 NativeBackend 探针 | mode-3 chat 端到端在 native 下跑 24h 不挂 |
| W6 attach client | crossterm raw 模式 + bidi UDS attach RPC + 退出热键 | dogfooding by team for 1w |
| W7 doc-syncer + ship gate | tier-1 docs sync + 用户面 docs 写新 backend + version bump | grep clean + clippy 0 + cargo fmt clean |

### 长期(V0.8+)
- flip default to native
- 删 TmuxBackend(`ccteam attach` 走 native)
- Windows CI 打开
- 内部 attach 协议可以演化成 web terminal(浏览器里 attach `ccteam-chat-foo-bar` session)

### 明确**不推荐** Option B(vendor tmux 二进制)
理由:工程负担大于 A,收益小于 A,不解决 Windows,违反 ccteam vendor 红线风格。

---

## 七、未决问题(下次 doc-first 评审要回答)

1. **是否要支持多 client attach 仲裁**?如果"单 attach 独占"可接受,Native attach 实现成本砍半
2. **daemon 启停模型**:socket-activation? systemd user unit? 还是 ccteam 进程内 spawn(类似 F130 IM supervisor 折入)?
3. **跨用户安全**:UDS 走 `~/.ccteam/run/mux.sock` 默认 0600 即可,不需 auth
4. **Codex `codex app-server` UDS 路径** vs ccteam-mux UDS 是否复用同 daemon? — 当前 Codex 适配器有自己的 UDS,合并要看 codex 协议是否能复用 mux 通用 RPC,可能不行(Codex app-server 是 codex 自己的协议)
5. **`ccteam attach` 用户文案**:detach 热键是 `Ctrl-b d` 兼容 tmux 习惯,还是另选(`Ctrl-]`,telnet 风)?投票 / 调研
6. **vt100 0.15 对 Claude TUI 实测兼容度** — 需要 W1 spike 实测一遍 `claude` TUI 全交互,看有没有 escape 序列不支持

---

## Sources

- [Zellij Modern Alternative to tmux 2026](https://petronellatech.com/blog/zellij-terminal-multiplexer-guide-2026) — Zellij 现状,WASM 插件,terminal-emulator-agnostic
- [Claude Code Native Zellij support issue #31901](https://github.com/anthropics/claude-code/issues/31901) — 上游 Anthropic 在评估 zellij 作为 agent teams 备选
- [portable-pty crate](https://lib.rs/crates/portable-pty) — WezTerm 跨平台 PTY 抽象
- [alacritty_terminal crate](https://crates.io/crates/alacritty_terminal) — 备选完整终端 emulator library
- [vt100 crate](https://docs.rs/vt100) — ccteam 已在用,文档明确"可用于实现类 tmux 程序"
- [PTY and Process Management — wezterm DeepWiki](https://deepwiki.com/wezterm/wezterm/4.5-pty-and-process-management) — portable-pty 生产实践
- [ConPTY portable-pty win module](https://github.com/wez/wezterm/blob/master/pty/src/win/conpty.rs) — Windows ConPTY 实现细节 + 已知 quirk
- [tmux Control Mode wiki](https://github.com/tmux/tmux/wiki/Control-Mode) — `-CC` 协议(本调研评估为不复用)
- [tmux-interface-rs](https://github.com/AntonGepting/tmux-interface-rs) — Rust over tmux CLI wrapper(评估为不解决问题)
- [termwiz tmux_cc module](https://docs.rs/termwiz/latest/termwiz/tmux_cc/index.html) — WezTerm 的 tmux control-mode client
- [tmux Installing wiki](https://github.com/tmux/tmux/wiki/Installing) — libevent + ncurses + yacc 依赖
