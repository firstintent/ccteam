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

---

## 八、自研 mux 原理拆解(building blocks 视角)

### 8.1 tmux 现状是怎么工作的

任何 mux(tmux / zellij / screen / wezterm-mux / Helvesec rmux)都是同一个三层模型,差别只在实现语言 + 协议 + UX:

```
┌─────────────────────────────────────────────────────────────────┐
│ Layer 3: Client Layer(human terminal OR agent RPC client)       │
│   tmux: `tmux attach` 拉出一个 terminal-inside-terminal         │
│   ccteam: 既有人 attach 也有 MCP RPC 调 send-keys/capture        │
└──────────────────────▲──────────────────────────────────────────┘
                       │ IPC(tmux 走 unix socket 自定义二进制协议)
┌──────────────────────▼──────────────────────────────────────────┐
│ Layer 2: Server / Multiplexer Daemon(detached, owns state)      │
│   • event loop(tmux 用 libevent,rmux 类用 tokio/epoll)         │
│   • HashMap<SessionName, Session>                               │
│     ├── PTY master fd                                           │
│     ├── child process handle (PID + waitpid future)             │
│     ├── vt screen state(光标位置/属性/scrollback ring)         │
│     ├── subscribers(broadcast 给 N 个 attached client)          │
│     └── per-pane metadata(title / cwd / size / 自定义 fmt 变量) │
└──────────────────────▲──────────────────────────────────────────┘
                       │ PTY master/slave (openpty / ConPTY)
┌──────────────────────▼──────────────────────────────────────────┐
│ Layer 1: PTY + Child Process(`claude` / `codex` / shell)        │
│   • slave 端是 child 的 stdin/stdout/stderr + ctty             │
│   • master 端是 daemon 读/写口                                  │
│   • SIGWINCH 通过 master 的 TIOCSWINSZ 同步                     │
└─────────────────────────────────────────────────────────────────┘
```

**核心抽象**:**daemon 替 child 持有它的 controlling TTY**。child 以为自己跑在终端里(`isatty(0)==true`,环境变量 `TERM=xterm-256color`,有 `winsize`),实际另一端是 daemon 的 fd。所有 keystroke 经 daemon master fd 写入 PTY slave;所有 child 输出从 master 读出 → daemon 喂给 vt parser 维护 screen state → 同时广播给所有 subscriber。

**detached 长 session 怎么实现**:daemon 是独立进程(double-fork 脱离 controlling terminal),client 退出不影响 daemon 持有的 PTY + child。这是 tmux 之于 nohup/screen 的核心价值。

### 8.2 自研最小骨架(rmux for ccteam 的 5 个零件)

按 8.1 模型映射到 Rust crate:

| 零件 | 角色 | 选型 | LOC 估 |
|---|---|---|---|
| **PTY 抽象** | 跨平台 master/slave 分配 | `portable-pty` (wezterm 出品,已在 ccteam deps,Windows ConPTY 原生) | 0(直接用)|
| **VT parser + screen** | 解释 ANSI/CSI/OSC,维护 cursor + cell grid + scrollback | `vt100` 或更全的 `alacritty_terminal` | 0(直接用)+ 200 行 wrapper |
| **Event loop** | 多 PTY fd + 多 client socket fd async 复用 | `tokio` (已在 ccteam deps)| 0 |
| **IPC 协议** | client ↔ daemon RPC,客户端 send-keys/capture/subscribe/attach | length-prefixed JSON-RPC over UDS;tarpc 也可 | 400 |
| **Attach client** | 把本机 TTY 转发到 daemon 的 PTY(raw mode + 键盘 + 鼠标 + resize)| `crossterm` + `tokio::select!` | 800 |

总计 ~3-4k LOC 落地一个**只为 ccteam 服务的最小 rmux**,比 §一.4 估算还小,因为我们**不需要 tmux 90 命令完整 CLI 兼容**(`Helvesec/rmux` 选择全兼容,我们不必)。

---

## 九、现有 tmux 的 14 个历史包袱(为什么 1992 年的 screen + 2007 年的 tmux 不适合 agent 时代)

| # | 包袱 | 根因 | 对 ccteam 的具体影响 |
|---|---|---|---|
| 1 | **字符串协议,无结构化输出** | tmux CLI 设计于 shell 脚本时代,`-F '#{format}'` 是字符串模板 | ccteam `display-message -p '#{pane_pid}'` parse 出来还要 trim/atoi;`-CC` control mode 是行协议,转义规则 corner case 多 |
| 2 | **PTY 字节流是唯一观测面** | tmux 不区分 child stdout / stderr / ANSI / 控制字符,全是 bytes;hooks 是 shell 命令不是 typed event | ccteam 红线"不解析 pane 输出"被迫只能靠 Claude Code hooks 的 fast event 通道,失去"Claude 输出了 X 关键词时通知我"这类能力(必须自己 grep)|
| 3 | **single-server 单进程隔离弱** | 所有 session 跑在一个 `tmux server`,libevent 单 reactor | 一个 session 的 escape 序列触发 vt100 解析 bug 可能拖累整个 server(罕见但发生过)|
| 4 | **无 per-session 资源限制** | 1992 年 screen 设计时没有 cgroup | 跑飞的 `claude --bg` 可以吃光机器内存;ccteam 只能在应用层 `budgets.max_cost_usd_per_24h`(费用,不是 CPU/mem)逼停 |
| 5 | **scrollback 是 RAM ring,不持久** | tmux 默认 2000 行 in-memory;`-S -10000` 也只是改 ring 大小 | 长跑 agent 输出 100MB 编译日志,tmux server RSS 飙;ccteam 已经用 transcript jsonl 绕开这个,但渲染时还要靠 capture-pane |
| 6 | **PID-based child tracking 易被 reuse 坑** | tmux 用 PID 跟踪 child;child 死 + fork 新进程占同 PID,tmux 无 waitpid 即时性 | ccteam F86 历史 bug 即此类(双校 tmux has-session + kill -0 已经在防);native daemon 持有 `Child` handle,从原理上免疫 |
| 7 | **C + libevent + ncurses + yacc 编译链** | 2007 年 BSD 风格 | 用户系统老 → `libevent < 2.x`/老 ncurses → 编译失败;macOS 上 brew 装 tmux 偶尔要重链;ccteam install 出过多次 tmux missing 工单 |
| 8 | **不支持 Windows 原生** | tmux 依赖 unix PTY,Windows 只能 WSL2 | ccteam CLAUDE.md 必须写"Windows 走 WSL2",一刀切掉相当比例的企业 Windows + macOS Boot Camp 用户 |
| 9 | **`-CC` control mode 协议不友好** | 1992 GNU screen 遗留 + tmux 改造的妥协 | termwiz `tmux_cc` 模块文档已警告"-CC probably only makes sense if you're going to run it in a pty";agent 集成普遍弃用 |
| 10 | **server 跨重启不存活** | tmux server 是用户级长进程,机器重启 session 全死 | NAS 长跑 agent 一旦机器升级重启全断;需要外部 tmux-resurrect 插件凑活 |
| 11 | **`.tmux.conf` 与编程使用者冲突** | tmux 设计 client 是人 | 用户 `.tmux.conf` 改了 prefix / 装了 tmux-resurrect 钩 attach 事件 → ccteam attach 进去键绑奇怪;ccteam 没法假设干净环境 |
| 12 | **格式串 mini-DSL 不可扩展** | `#{}` 格式串是 tmux 内嵌求值器 | 想加自定义元数据(如"这个 session 是哪个 ccteam role")只能塞 session 名字串,无 native key/value |
| 13 | **多 client 协调粗暴** | 第二个 attach 默认共享 keystroke,经常误触 | dev workflow 里两个窗口看同一 session 共敲键导致混乱;mosh 已修但 tmux 没改 |
| 14 | **socket 文件全局共享** | `/tmp/tmux-<uid>/default` 单 socket,多版本 tmux 互不识别 | `tmux 3.4 client` 连不上 `tmux 3.5 server`;ccteam 升级 tmux 后旧 session 不可见 |

**额外的"用了 1/4 surface 却背 4/4 维护负担"陷阱**:tmux 90 个子命令、formats DSL、layouts、windows、panes、buffers、hooks、commands sequence、key tables、mouse mode — ccteam 一个都不用,但全在 binary 里,任何一个的 bug 都可能波及 ccteam 真用到的 11 命令路径。

---

## 十、rmux 为 agent 设计的 12 个优势支柱(对照 §九 包袱逐个解)

> 业内已有 [`Helvesec/rmux`](https://github.com/helvesec/rmux)(Rust,tmux 兼容 CLI + typed SDK,Linux/macOS/Windows 原生)和 [`manaflow-ai/cmux`](https://github.com/manaflow-ai/cmux)(macOS 原生 + JSON-RPC socket)正在做这件事。下面是 ccteam 视角下 12 个 agent-first 设计支柱,部分支柱与 Helvesec rmux 重合(说明方向对),部分是 ccteam 独有(说明专属 ccteam 的 rmux 仍有价值)。

| # | 支柱 | 解决 §九 哪条包袱 | ccteam 具体收益 |
|---|---|---|---|
| 1 | **typed JSON-RPC over UDS,无 shell escape** | #1, #9, #12 | `send_keys`/`capture`/`subscribe` 是 typed Rust 函数,zero string templating;ccteam mcp-serve 直接走 RPC,删 ~200 行 escape/parse 胶水 |
| 2 | **structured event stream(typed event 而非 bytes)** | #2 | daemon vt100 parser 之上加 event detector:`process_exited(code)` / `bell_received` / `title_changed(s)` / `output_idle(threshold)` / `output_pattern_matched(regex_id)` 由 daemon push 给 subscriber;ccteam 可注册 "waiting for input:" 模式触发 plan_pending 事件,失去对 Claude Code fast event hooks 的死依赖 |
| 3 | **per-session 资源隔离** | #3, #4 | linux 用 cgroup v2 包 child(`memory.max` / `cpu.max` / `pids.max`);macOS 用 `rlimit` + `posix_spawn`;Windows 用 Job Object。`budgets:` 配置直接落 OS 层硬隔离,而非应用层软计费 |
| 4 | **append-only event log scrollback(磁盘 + 查询)** | #5, #10 | 输出按时间窗口分片落 `~/.ccteam/sessions/<name>/output.<n>.jsonl`,daemon RAM 只留 last N 行;查询走 mmap + binary search 按时间或行号;天然成为 transcript jsonl(F172 路径合并) |
| 5 | **child handle owned by daemon,零 PID-reuse race** | #6 | tokio `Child::wait()` future 是 race-free 的死亡通知;不再需要 `tmux has-session + kill -0 + pid match` 三重双校 |
| 6 | **零外部 native 依赖,静态链单 binary** | #7 | `cargo build --release` 出一个 ~2MB 静态 binary,无 libevent/ncurses;musl-static linux x64+arm64 + macOS notarized + Windows MSVC 一条 release.yml 搞定;install.sh 不再要 `apt install tmux` 步骤 |
| 7 | **Windows ConPTY 原生** | #8 | portable-pty 已封 ConPTY;ccteam Windows 用户脱离 WSL2,CLAUDE.md 红线松绑(也利于企业用户 onboarding) |
| 8 | **session 跨 host 重启可选 checkpoint** | #10 | daemon shutdown 时把 child env / cwd / 最后 N 条 event 落 snapshot;启动时按 `--restore` 标志选择性 respawn(注意:**TTY 状态本质不可恢复**,只能 respawn child 重跑;但对 `claude --resume <name>`(F172)路径完美 — claude 自带 session resume,rmux 只需重新拉起 claude 进程喂同 args)|
| 9 | **session metadata 是 typed key/value,非格式串** | #12 | `metadata: { role: "critic", workflow: "review-flow", spawned_by_msg_id: ... }` 是 daemon HashMap,RPC 可读可写;ccteam 不再用 session 名 `ccteam-chat-<slug>-<role>` 编码所有信息(F172 设计可简化)|
| 10 | **multi-client attach 仲裁可配置** | #13 | `attach --mode=observer`(只读)/`--mode=controller`(可写,独占);默认 observer 防止 dev 两窗口共敲;`ccteam attach` 走 controller |
| 11 | **socket 路径项目隔离** | #14 | UDS 走 `~/.ccteam/run/rmux.sock`,不与系统 tmux 共用 socket;升级 rmux 版本可强制 daemon restart 不影响 host;多 ccteam project 各自 namespace |
| 12 | **headless-by-default,attach 是次要 feature** | #2, #11 | tmux 设计假设主消费者是人;rmux 假设主消费者是 agent RPC,只在 `ccteam attach` 时才进入 TTY 模式 → 大量 tmux UX 代码(status bar / choose-tree / copy mode / mouse / `.tmux.conf` 求值器)整层删除;binary 体积 + 攻击面双降 |

### 10.1 不在支柱里的"次要好处"

- **可观测性**:daemon `/metrics` 出 prometheus(每 session CPU/mem/output rate / PTY buffer 高水位 / RPC latency p99),今天 ccteam web 4 面板可加 "mux session health" 第 5 面板
- **测试可注入**:`MuxBackend` trait + `MockBackend`,单元测试不依赖真 PTY,跑 cargo test 不需要 tmux 安装
- **`/exit /compact /new` 透传更干净**:无 prefix key 概念,这些指令是原样字节流,不会被 rmux 解释(tmux 的 `prefix + d` detach 在某些 ANSI 序列下偶尔误触)
- **支持 batch 操作**:`rmux send --to-all-matching 'ccteam-chat-*' '/compact'`一次性给所有 chat bot compaction,tmux 要 shell `for` 循环
- **支持 session fork**(可选):`rmux fork <name> <new_name>` 复制 env+cwd+history,跑双假设并行(agent A/B 测试场景)
- **不强加键绑哲学**:tmux `Ctrl-b` / screen `Ctrl-a` 都是 prefix 心智负担;rmux attach 默认 `Ctrl-]` (telnet/ssh 风格,大家都会),不与任何 shell 应用冲突

### 10.2 与 Helvesec/rmux 上游的选择对比

| 决策点 | Helvesec/rmux | ccteam 视角推荐 |
|---|---|---|
| tmux CLI 兼容 | 全 90 命令兼容(易迁移) | **部分兼容**(只兜底 11 命令)— ccteam 不需要 tmux 迁移群体 |
| binary 形态 | 独立 `rmux` daemon + CLI | **可选库化** — daemon 可折入 `ccteam` 进程(类 F130 IM supervisor 模式),省一个 binary |
| 复用上游? | — | **强推**先用 Helvesec/rmux 做 W3 spike;若 SDK 够用、license 兼容,直接依赖,自己只写 ccteam-side adapter,**砍 60% 工作量** |
| 协议 | typed SDK(Playwright-style) | RPC schema 上 ccteam 与之对齐(ensure_session / wait_for_text / snapshot 直接借)|

**新增推荐**:V0.7 doc-first kickoff 第一步先 spike Helvesec/rmux 是否能直接当依赖。如果能,Option A 估算从 4kLOC → ~1.5kLOC(只写 MuxBackend trait + adapter + ccteam attach 客户端),时间从 6-8 周 → 2-3 周。

---

---

## 十一、竞品全景(2026-05 X / GitHub 社区视角)

把 §十.2 的"业内已有 rmux + cmux"打开,补全 X 上 5 月活跃讨论的全部 8 个项目,按"如何对待 tmux"分两大阵营:

### 11.1 阵营 A:**Replace tmux**(自己写 PTY + daemon,弃 tmux)

| 项目 | 语言/平台 | 核心定位 | ccteam 复用价值 |
|---|---|---|---|
| **[Helvesec/rmux](https://github.com/helvesec/rmux)** v0.3.0 | Rust / Linux+macOS+**Windows ConPTY** | tmux-compatible CLI + Playwright-style typed SDK + daemon UDS。`ensure_session`/`wait_for_text`/`snapshot` 是一等公民 | **★★★★★** 直接当 ccteam V0.7 mux backend 依赖;X 社区评 "Claude Code agents just got their perfect terminal multiplexer" |
| **[manaflow-ai/cmux](https://cmux.com)** | Swift/AppKit (macOS) | Ghostty 核 + 垂直 tab + 浏览器嵌入 + JSON-RPC `cmux.sock` + agent 完成时闪烁提醒 | **★★** 只 macOS,无法跨平台;协议设计可参考 |
| **[bradwilson331/cmux-linux](https://github.com/bradwilson331/cmux-linux)** | Rust + GTK4 + Ghostty | cmux 的 Linux 端口 | **★★** 同上,且依赖 GTK4 太重 |
| **[amirlehmam/wmux](https://github.com/amirlehmam/wmux)** / **[openwong2kim/wmux](https://github.com/openwong2kim/wmux)** | Rust (Windows native) | "no WSL required" — Windows AI agent multiplexer + MCP browser automation | **★★★** Windows-only;若 Helvesec/rmux Windows 路径不稳可作 fallback 参考 |
| **[psmux/psmux](https://github.com/psmux/psmux)** | Rust (PowerShell + Windows Terminal + cmd.exe) | Windows-native tmux,PowerShell 优先 | **★** 偏 shell 集成,非 agent 目标 |
| **[shell-pool/shpool](https://github.com/shell-pool/shpool)** | Rust | "think tmux, then aim lower" — 极简 detach/attach,无 mux | **★** 太薄,缺 multi-session;但代码量小可作 daemon 起步参考 |

### 11.2 阵营 B:**Wrap tmux**(保留 tmux 作 runtime,加 orchestration + dashboard)

| 项目 | 语言 | 核心定位 | ccteam 关系 |
|---|---|---|---|
| **[mixpeek/amux](https://github.com/mixpeek/amux)** | Python(单文件 `amux-server.py`)| **最像 ccteam 的竞品** — tmux 之上加 web dashboard + 移动 PWA + 自愈监控(检测 rate-limit / context overflow 自动按键 resume)+ kanban + email + CRM | **★★★★★ 直接竞品**;**关键差异**:amux 走"parse ANSI-stripped tmux output"路径,**违反 ccteam 红线**;但拿到了 ccteam 用 hooks/MCP 拿不到的 rate-limit 自愈能力 |
| **[wavyrai/tmux-ide](https://github.com/wavyrai/tmux-ide)** (Thijs Verreck) | npm package(Node.js)| 声明式 `ide.yml` 定义 tmux pane layout + native Claude Agent Teams 支持 + 浏览器 dashboard(KPI/milestone/utilization/validation/timeline)+ Missions Mode 自治多代理 | **★★★ 横向 — 不同抽象层**;ccteam workflow.yaml 是 **role 拓扑**,tmux-ide ide.yml 是 **pane layout**,可叠加;若 ccteam 把 mux 层换成 rmux,可启发"声明式 layout" CLI 加进 ccteam-creator |
| **[nmamano/isomux](https://github.com/nmamano/isomux)** | Bun/TypeScript | "Office metaphor" 可视化 — agent 是动画角色,sleep/typing/waving + 跨 Claude+Codex provider mix + 共享 task board + cron + 对话分支 | **★★ 消费端 UX 启发**;ccteam web SPA 当前是 dashboard 风,isomux 的 skeuomorphic 风可作 V0.8+ web UI 参考 |
| **[l9c/tmux-agent-teams](https://github.com/l9c/tmux-agent-teams)** | Skill | "agent skill that enables agents to interact with Claude Code through tmux" — 走 Claude Code skill 接口 | **★** 与 ccteam skill 系统平行;ccteam-control 已覆盖 |
| **[Ark0N/Codeman](https://github.com/Ark0N/Codeman)** | Web UI | tmux + Claude Code/Opencode session 管理 webui | **★** 风格类似 ccteam web,但 Code 量小 |

### 11.3 阵营 C:**人类优先现代多路复用器**(非 agent 导向)

| 项目 | 与 ccteam | 备注 |
|---|---|---|
| **[Zellij](https://zellij.dev/)** 20k+ ★ | **不复用** | DHH 从 tmux 切过去,Rust + WASM 插件 + Web 客户端 + 浮动/堆叠/布局 + 内置 UI 提示。但**是 binary,不是 library**,嵌入它 = 把 tmux 依赖换成 zellij 依赖,问题平移 |
| **wezterm-mux** | **不复用** | wezterm 内嵌的 mux,需 wezterm 进程;但 portable-pty + termwiz 来自 wezterm,**零件级复用** |
| **tmux 自身** | 现状 | Anthropic Claude Code Agent Teams 官方推荐;生态最成熟;**作为 ccteam V0.6 backend 兜底保留** |

### 11.4 阵营 D:**官方未定**

| 项目 | 状态 |
|---|---|
| [Claude Code issue #31901](https://github.com/anthropics/claude-code/issues/31901) | Anthropic 官方在评估 Zellij 作为 Agent Teams 备选 backend;**上游一旦官方支持 zellij,ccteam 可能要回应** |

### 11.5 关键模式总结(社区共识 2026-05)

1. **agent-native mux 的 3 个共识原语**:`ensure_session(idempotent create-or-reuse)` + `wait_for_text(replace sleep+grep loops)` + `snapshot(structured pane state)` — 三个项目(rmux/cmux/amux 各自实现各异但功能一致)独立收敛到同一组 API,**说明 §十.2 typed event stream 设计支柱是行业方向不是 ccteam 独有想法**
2. **Windows 原生不再是奢望**:rmux/wmux/psmux 全部走 ConPTY,WSL2 在 AI agent 圈子明显被认为"不该是必需"
3. **dashboard + 移动端是标配**:amux PWA / isomux 浏览器 / tmux-ide dashboard / ccteam web SPA — 所有项目都有,差异只在 UX 风
4. **agent 间消息总线是分歧点**:ccteam 走 progress.jsonl + MCP;amux 走 watchdog 监听 pane;tmux-ide 走 shared task board;isomux 走 skill 命令(`/isomux-peer-review` `/isomux-all-hands`);**ccteam 的 progress.jsonl 唯一 SoT 是最严格的设计,值得保留**
5. **declarative config 是标配**:ccteam workflow.yaml + tmux-ide ide.yml + isomux office config + amux yaml — 全部声明式,**没有项目在用纯 CLI/imperative**

---

## 十二、对 ccteam 的战略影响 — V0.7 决策更新

§十.2 推荐的 "spike Helvesec/rmux 作 ccteam mux backend" **依然是首选路径**,但 §十一 的全景让我们必须明确 ccteam **战略定位**:

### 12.1 ccteam 在 2026-05 ecosystem 里的位置

```
                    Pane-level orchestration                Role-level orchestration
                    (单项目内多 pane 协调)                   (跨项目/多 agent 团队拓扑)
                            │                                      │
   Wrap tmux  ──────────────┼──────────────────────────────────────┼─────────
   (脆弱但兼容)      amux ●      tmux-ide ●                ccteam V0.6 ●
                       (PWA)        (yaml)                  (workflow.yaml + MCP)
                                                                    │
                                                            ↓ migrate via Option A
                                                                    │
   Replace tmux ──────────────┼──────────────────────────────────────┼─────────
   (干净但需建)              cmux ●                            ccteam V0.7 ●
                              wmux ●                          (workflow.yaml + MCP
                                                              + rmux backend)
                                       ↑
                                       │
                              Helvesec/rmux ● (纯 mux 引擎,无 role 概念)
```

**ccteam 独占的市场象限**:**Role-level orchestration**(workflow.yaml + 27 MCP 工具 + 跨项目记忆 + IM 集成)— rmux/cmux/wmux 都不做这一层,amux/tmux-ide 做但比 ccteam 弱。**Option A 走通后**(Replace tmux + 保留 role-level),ccteam 是唯一同时占两个象限的项目。

### 12.2 amux 给 ccteam 的最大启发(也是最大威胁)

amux **违反 ccteam 红线**(parse pane output),但拿到了 ccteam 拿不到的杀手能力:
- **自动检测 rate-limit** → fleet 内所有 blocked session 按 `1` resume
- **检测 context overflow** → 自动重启 session
- **解析 scrollback 提取 reset 时间** → 倒数完精准 steer 消息

ccteam 不能学 amux 做 pane scraping(红线在),但**这些能力用户真的需要**。Option A(daemon 持有 vt100 + 暴露 typed event)的最大正当性就在此:**让 daemon 内置 vt100 state machine 暴露 `rate_limit_detected` / `context_overflow_detected` / `bell_received` 类 typed event,ccteam orchestrator 订阅 typed event 而非 grep pane bytes** — **形式上没有"业务层解析 pane",事实上拿到 amux 同等能力**。这条路径让红线和能力同时成立,是 §十.2 第 2 支柱"structured event stream"的最强落地理由。

### 12.3 修订后的 V0.7 决策矩阵

| 选项 | 路径 | 工作量 | 收益 | 推荐度 |
|---|---|---|---|---|
| **V0.7-A1** | 依赖 Helvesec/rmux 作 binary,ccteam 走 MuxBackend trait + adapter | 1.5kLOC / 2-3w | Windows + typed event + 删 tmux dep | **★★★★★ 首选** |
| **V0.7-A2** | 自己写 native mux daemon,不依赖外部 | 4kLOC / 6-8w | 全控制 + 可深度集成 ccteam 进程内 | ★★★ 备选(若 A1 spike 不通)|
| **V0.7-B** | 学 amux 做 pane scraping,补 rate-limit 自愈,**不**换 mux | <1kLOC / 1w | 立刻拿到 rate-limit 自愈 | ❌ **拒** — 违反红线,且不解决 Windows |
| **V0.7-C** | 转 zellij | 中 | 跟 Anthropic 上游一致 | ⚠️ **等上游表态**(issue #31901 未决前不动)|
| **V0.7-D** | 维持 tmux,只补 Option C install.sh | 0.5d | 短期止血 | ✅ **作 V0.6.x patch 落地,与 V0.7-A1 不冲突** |

### 12.4 spike 计划(V0.7 W0 必跑,1 天预算)

V0.7 doc-first kickoff 之前先跑 3 个 spike 验证假设:

1. **Helvesec/rmux 当依赖可行性**(0.5d):`cargo add rmux-sdk` → 跑 `ensure_session` + `wait_for_text` + `snapshot` 三个 RPC → 是否覆盖 ccteam §一.2 全部 11 子命令?attach 路径是否 expose?license 是否兼容 ccteam(看 GitHub LICENSE 文件)?
2. **rmux + claude TUI 端到端兼容性**(0.5d):用 rmux 跑 `claude --resume <name>` 完整交互一遍(F172 V2 lossless 续接路径),vt100 渲染是否正常?ANSI 序列是否漏?有无 sixel/iterm-image escape 序列?
3. **typed event 设计稿**(0.5d):列 ccteam 想要的 typed event 集合(`rate_limit_detected` / `bell` / `idle_5s` / `process_exited` / `output_pattern_matched` 等),对照 rmux 已 expose 的是否够;不够要 patch 上游 OR 自己写

**Go/No-Go 标准**:3 个 spike 全过 → V0.7-A1;任意一个不过 → 退 V0.7-A2;Anthropic 在 issue #31901 表态用 zellij → 转 V0.7-C 重评。

---

## Sources

- [Helvesec/rmux GitHub](https://github.com/helvesec/rmux) — **已有 Rust 实现**,tmux 兼容 CLI + typed SDK + native Linux/macOS/Windows,v0.3.0 (2026-05-23);强推先 spike 复用
- [rmux.io](https://rmux.io/) — Helvesec rmux 官网 + SDK 文档
- [Show HN: Rmux Playwright-style SDK](https://news.ycombinator.com/item?id=48219918) — 社区讨论 + 设计动机
- [Rmux Review on andrew.ooo](https://andrew.ooo/posts/rmux-rust-terminal-multiplexer-agents-review/) — 第三方 review,实测可用度
- [manaflow-ai/cmux](https://cmux.com/) — macOS 原生竞品(Swift/AppKit),JSON-RPC socket
- [cmux for Linux (bradwilson331/cmux-linux)](https://github.com/bradwilson331/cmux-linux) — Linux 端口(Rust + GTK4 + Ghostty)
- [wmux Windows port (amirlehmam/wmux)](https://github.com/amirlehmam/wmux) — Windows 端口
- [How tmux Became the Runtime for AI Agent Teams](https://dev.to/battyterm/how-tmux-became-the-runtime-for-ai-agent-teams-gmi) — 行业趋势分析
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
- [libevent.org](https://libevent.org/) — tmux 底层 event loop 实现 + 已知 multithreading 限制
- [rmux.io official docs](https://rmux.io/) — Helvesec rmux SDK 完整文档
- [Show HN: Rmux Playwright-style SDK](https://news.ycombinator.com/item?id=48219918) — 社区设计动机讨论
- [Rmux Review on andrew.ooo](https://andrew.ooo/posts/rmux-rust-terminal-multiplexer-agents-review/) — 第三方实测 review
- [mixpeek/amux GitHub](https://github.com/mixpeek/amux) — **最像 ccteam 的竞品**,Python 单文件,parse pane output 拿到 rate-limit 自愈(违反 ccteam 红线但启发设计)
- [amux.io blog: Best Multi-Agent Orchestrators 2026](https://amux.io/blog/best-multi-agent-orchestrators-2026/) — 行业全景对比 Claude Squad / Conductor / Codex
- [Show HN: Amux tmux-based multiplexer](https://news.ycombinator.com/item?id=47104424) — amux 设计讨论
- [wavyrai/tmux-ide GitHub](https://github.com/wavyrai/tmux-ide) — Thijs Verreck 的 declarative `ide.yml` + Claude Agent Teams native + 浏览器 dashboard(npm 包)
- [tmux-ide official site](https://tmux.thijsverreck.com/) — Missions Mode 自治多代理介绍
- [nmamano/isomux GitHub](https://github.com/nmamano/isomux) — "Office metaphor" 可视化 — agent 动画角色 + Claude+Codex provider mix(Bun/TS)
- [isomux.com](https://isomux.com/) — isomux 官网
- [openwong2kim/wmux GitHub](https://github.com/openwong2kim/wmux) — Windows native AI agent multiplexer(无 WSL)+ MCP browser automation
- [amirlehmam/wmux GitHub](https://github.com/amirlehmam/wmux) — Windows cmux port
- [bradwilson331/cmux-linux GitHub](https://github.com/bradwilson331/cmux-linux) — cmux Linux 端口(Rust + GTK4 + Ghostty)
- [psmux/psmux GitHub](https://github.com/psmux/psmux) — Tmux on Windows PowerShell(Rust native)
- [shell-pool/shpool GitHub](https://github.com/shell-pool/shpool) — "think tmux, then aim lower" — 极简 detach/attach
- [l9c/tmux-agent-teams GitHub](https://github.com/l9c/tmux-agent-teams) — Claude Code skill 走 tmux 与 agent 交互
- [Ark0N/Codeman GitHub](https://github.com/Ark0N/Codeman) — Web UI 管理 Claude Code/Opencode tmux session
- [tmux-alternative GitHub Topic](https://github.com/topics/tmux-alternative) — 全谱 tmux 替代品索引
- [Zellij official site](https://zellij.dev/) — Rust modern tmux 替代,20k+ stars
- [Thijs Verreck tweet introducing tmux-ide](https://x.com/ThijsVerreck/status/2032034893383782744) — 原始发布推
- [Setting up Claude Code Agent Teams on Windows w/ WSL2 + tmux](https://ardalis.com/setting-up-claude-code-agent-teams-with-wsl2-and-tmux-on-windows/) — Anthropic 官方推荐 tmux 的实际部署痛点
- [Claude Code Agent Teams overview (cobusgreyling)](https://cobusgreyling.substack.com/p/claude-code-agent-teams) — Anthropic 官方多代理设计
