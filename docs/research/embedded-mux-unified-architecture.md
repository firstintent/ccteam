# 嵌入式 mux 统一 mode 1/2/3 终极架构实现方案

> **类型**：`docs/research/`(扩展研究,不更新,按需加载) — 见 `docs/README.md` §三类
> **作者**：claude session on `main`(基线 V0.6.7,2026-05-26)
> **问题来源**：用户 — "请从终极架构深度思考,如果能够用嵌入mux统一mode1 2 3架构那就最好了。写一个实现方案文档"
> **前置阅读**:`docs/research/rust-tmux-rewrite-feasibility.md`(同目录,该篇结论 Option V0.7-A1 推荐依赖 rmux,本篇在其基础上推进到"统一 3 模式"维度)
> **结论先行**:**可行,且应做** — 用 rmux 作为 Cargo 依赖 + `ccteam mux daemon` re-exec 子命令,把 mode 2 / mode 3a / mode 3b 三类长跑子进程**全部装入 mux 会话**;mode 1 in-proc 保留 in-process 不进 mux(性能);MuxBackend trait 是唯一执行面接口,vendor adapter 全部从"各自定制 process supervision"统一到"共享 mux session 之上薄薄一层 vendor 协议"。这是一个 V0.8 minor 主线 epic 量级的工作(~6-8w),**不**与 V0.7 主线(IM 覆盖 / HumanApproval / chat memory)冲突 — 二者可平行 wave-by-wave 推。

---

## 一、问题陈述:今天 ccteam 有 3 套子进程供养机制

V0.6.7 的"模式 × vendor"双轴(`docs/versions/v0-6-0/README.md §五`)在 vendor 维度已对齐(Claude / Codex trait),但**模式维度的子进程供养是 3 套独立机制**:

| 模式 | vendor 实现 | 子进程供养机制 | 红线交付的边界 |
|---|---|---|---|
| **Mode 1 in-proc** | — | 不 spawn,纯 Rust 函数调用 | 无 |
| **Mode 2 bg** | `ClaudeBgAdapter` / `CodexExecAdapter` | `std::process::Command + spawn`,捕获 stdout/stderr,等 child 退出 + 读 jobs.jsonl/progress.jsonl | 文件系统(jobs.jsonl, progress.jsonl) |
| **Mode 3a chat(Claude TUI)** | `ClaudeTuiAdapter` | `tmux new-session -d` 起长 session + send-keys 注入 + 读 transcript jsonl byte-offset | tmux session + ccteam-owned `turns.jsonl` |
| **Mode 3b chat(Codex app-server)** | `CodexAppServerAdapter` | `codex app-server` 起独立 UDS,自管 supervisor | Codex 自己的 UDS + progress bridge |
| **Mode 4 human-approval(V0.6.1 F124)** | wrapper layer | 不直接 spawn 子进程,只读写 progress.jsonl::plan_decision | 文件 + IM round-trip |

**实际症状**:
1. **mode 2 bg 没有 crash-resilience** — ccteam 进程崩溃,bg child 还在跑但 ccteam 重启后能否"reattach"靠 jobs.jsonl 是否完整,逻辑分散在 `session_recovery.rs`
2. **mode 2 bg 没有 peek/screenshot** — 现状 screenshot 工具只对 mode 3 tmux session 工作;mode 2 在跑啥用户看不到
3. **mode 3a 与 mode 3b 用两套独立 PTY/UDS 抽象** — `claude_tui.rs` 走 tmux,`codex_app_server.rs` 走 UDS,二者无 shared resource accounting
4. **统一资源/cost ledger 在应用层硬拼** — `ccteam_cost` 用 vendor token usage 累计;没有 per-process CPU/mem 隔离(原 feasibility 调研 §九 包袱 #4 已记录)
5. **Windows 全断** — mode 3a 依赖 tmux,tmux 不跑 Windows,**ccteam 在 Windows 上的 mode 3 完全不可用**(CLAUDE.md 红线"Windows 走 WSL2")
6. **vendor adapter 平均 ~300-500 LOC** — 每个 vendor × 每个 mode 都得自己实现 supervisor / stream subscribe / 退出回收
7. **进 IM 后的"截图当前 agent 状态"是 mode-3-only feature** — F35/F38 screenshot 只对 tmux 工作

**根因诊断**:子进程供养机制是横切关注点(cross-cutting concern),但 ccteam 把它沿 mode 维度拆成了三份,各模式 vendor adapter 各自重新发明 process supervision。**这是技术债务,不是设计选择**。

---

## 二、终极架构愿景:嵌入式 mux 作为唯一执行面

### 2.1 核心 thesis(一句话)

**所有长跑子进程(mode 2/3a/3b)都跑在 rmux session 里;ccteam 不直接 `Command::spawn` 长跑子进程,只通过 `MuxBackend` trait 操作 session;mode 1 in-proc 是 `MuxBackend::InProc` 的 zero-cost variant**。

### 2.2 统一带来的 7 个深层红利

| # | 红利 | 落地体现 |
|---|---|---|
| 1 | **统一 vendor adapter 形态** | `ClaudeBgAdapter` / `CodexExecAdapter` / `ClaudeTuiAdapter` / `CodexAppServerAdapter` 共享 `MuxBackend::spawn_session` 一个入口,各 adapter 只剩 vendor-specific 协议层(argv 构造 + 事件翻译) — 估每个 adapter 砍 40-60% LOC |
| 2 | **universal peek/screenshot/attach** | F38 screenshot 工具今天只对 mode 3 tmux 工作;统一后 `ccteam peek <job_id>` 对 mode 2 bg agent 也出 PNG,因为每个 session 都有 vt100 screen state |
| 3 | **universal crash-resilience** | mode 2 bg 今天靠 `session_recovery.rs` 拼接 jobs.jsonl 反推状态;统一后 mux daemon 作 sibling process 持有 child handle,ccteam 重启 → 重连 → `process_exited(code)` typed event 直接告知是否还在跑 |
| 4 | **universal cost / resource ledger** | rmux daemon 可包 cgroup v2 (linux) / rlimit (macOS) / Job Object (Windows) → ccteam-cost 不只是 vendor token 费用,而是真实 CPU-second + RSS-peak;F84 auto-disable 触发可基于 OS 层硬上限 |
| 5 | **universal typed event 取代轮询** | mode 2 bg 今天靠 tail jobs.jsonl 轮询;mux daemon 可 emit `process_exited(code)` / `idle_30s` / `output_pattern_matched(regex_id)` typed event;ccteam orchestrator 是事件驱动而非轮询(原 feasibility §10 支柱 #2 落地) |
| 6 | **Windows 原生解锁** | rmux 已 ConPTY first-class(`spec/feature-inventory-v1.yaml` 全 windows: pass);mode 3 在 Windows 直接跑,CLAUDE.md "Windows 走 WSL2" 红线松绑 |
| 7 | **F38 screenshot / F35 silence-classifier / `/peek` MCP 跨模式收敛** | 三个 user-facing surface 都从"对 mode 3 工作"变成"对所有 mode 工作",无需各模式重写 |

### 2.3 终极架构分层图

```
┌────────────────────────────────────────────────────────────────────────┐
│ L4. ccteam orchestrator (workflow.yaml + ArtifactWatcher)               │
│     • progress.jsonl reader(7 类业务事件 + chat_session_reset 等)       │
│     • mode 1 in-proc 逻辑直接在此层(不进 mux)                          │
│     • mode 2/3 长跑子进程经 L3 trait 操作,不直接 Command::spawn        │
└────────────────────────────────────┬───────────────────────────────────┘
                                     │ 调用 trait 方法
                                     ▼
┌────────────────────────────────────────────────────────────────────────┐
│ L3. MuxBackend trait (ccteam-core 新模块)                               │
│     pub trait MuxBackend {                                              │
│       async fn spawn(spec: MuxSessionSpec) -> Result<MuxSessionHandle>; │
│       async fn send_keys(h: &Handle, bytes: &[u8]) -> Result<()>;       │
│       async fn capture(h: &Handle, opts: CaptureOpts) -> Result<Snap>;  │
│       async fn subscribe(h: &Handle) -> EventStream;                    │
│       async fn kill(h: &Handle) -> Result<()>;                          │
│       async fn exists(name: &str) -> Result<bool>;                      │
│       ...                                                               │
│     }                                                                   │
│                                                                         │
│     impl: InProcBackend(mode 1)  •  RmuxBackend(mode 2/3)              │
│           TmuxBackend(legacy,V0.7-V0.8 transition retain)              │
└─────────────────┬──────────────────────────┬───────────────────────────┘
                  │                          │
                  │ in-proc, no IPC          │ UDS / Named Pipe RPC
                  │                          ▼
                  │            ┌─────────────────────────────────────────┐
                  │            │ L2. rmux daemon(sibling process)        │
                  │            │     • by `ccteam mux daemon` re-exec    │
                  │            │     • portable-pty(Linux/macOS/Win)     │
                  │            │     • vt100 screen + scrollback         │
                  │            │     • typed event broadcaster           │
                  │            │     • per-session resource hooks(opt)   │
                  │            └────┬──────────────┬───────────┬─────────┘
                  │                 │              │           │
                  │           ┌─────▼────┐  ┌──────▼─────┐  ┌──▼────────────┐
                  │           │ PTY +    │  │ PTY +      │  │ PTY +         │
                  │           │ claude   │  │ claude     │  │ codex         │
                  │           │ --bg     │  │ (TUI)      │  │ app-server    │
                  │           │ (mode 2) │  │ (mode 3a)  │  │ (mode 3b)     │
                  │           └──────────┘  └────────────┘  └───────────────┘
                  │
       ┌──────────▼────────────┐
       │ L1. mode 1 in-proc 逻辑│
       │ pure Rust fn,无 IPC    │
       └────────────────────────┘
```

**关键不变量**:
- **progress.jsonl 仍是业务状态 SoT** — mux daemon 是 *upstream* event source,不是 *parallel* SoT;ccteam-core 监听 mux event → 翻译成 progress.jsonl 业务事件
- **CLAUDE.md §三 "不解析 pane output" 红线形式上保留、实质增强** — ccteam 业务代码不 grep bytes,而是注册 pre-built regex pattern 给 daemon;daemon emit `output_pattern_matched(regex_id)` typed event。**形式上没有"业务层解析 pane",事实上能拿到 amux 同等的 rate-limit 自愈能力**(原 feasibility §12.2 已论证)
- **ccteam-mux daemon 是 sibling process,不是 ccteam-imd in-process task** — 因为 mode 3 长 session 需 survive ccteam 重启;daemon 走 `ccteam mux daemon --socket <path>` re-exec,镜像 rmux 自己的 `run_hidden_daemon` 模式

---

## 三、各模式具体落地方案

### 3.1 Mode 1 in-proc — 保持原样,但走 trait 接口

**保留 in-process 调用,不进 mux pane**。理由:
- mode 1 是同步 / 短任务 / 无 PTY 需求(典型如 `mcp__ccteam__workflow_show` 这种 typed RPC 返回)
- 强行装进 mux session 会增加 IPC 跳数,损失意义
- 但需保留**接口统一**:`InProcBackend` 实现 `MuxBackend` trait,内部直接 `tokio::spawn` 一个 future,把"session"语义降级成"任务 handle"

落地:
```rust
// crates/ccteam-core/src/mux/inproc.rs
pub struct InProcBackend;

impl MuxBackend for InProcBackend {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionHandle> {
        // spec.argv 解释成 callable Rust fn(by registry lookup)
        // 直接 tokio::spawn,不经任何 IPC
        Ok(MuxSessionHandle::InProc { task: tokio::spawn(...) })
    }
    // send_keys / capture / subscribe 视情况实现 no-op 或 internal channel
}
```

**红线对齐**:mode 1 的"无 IPC"性能不变;只是上层 vendor adapter 用同一个 trait 调度。

### 3.2 Mode 2 bg(Claude --bg / Codex exec)— 装入 mux PTY pane

**关键设计点**:rmux 0.3.1 **PTY-only**(无 headless process-mode,§已 grep 验证 `spec.rs` `ProcessSpec` 不带 stdout-only flag)。这意味着 bg 子进程必须在 PTY 里跑,stdout/stderr 会 merge。**但这对 mode 2 bg 不构成问题**,因为:

1. mode 2 bg 的**主要 I/O 是文件**(jobs.jsonl + progress.jsonl + child working dir 产物),不是 stdout
2. PTY 的 merged 流只用作**典型事件源**:`process_exited(code)`、`output_idle(30s)`、可选 `output_pattern_matched(<regex_id>)`
3. jobs.jsonl 仍由 child 自己写;ccteam 仍读 jobs.jsonl 作为业务 SoT;只是**进程供养**从 ccteam-side `Command::spawn` 转到 mux-side spawn

落地:
- `ClaudeBgAdapter::spawn_bg_job(...)` 从 `Command::new("claude").arg("--bg")...` 改成 `mux.spawn(MuxSessionSpec { name: "ccteam-bg-<job_id>", argv: [...], detached: true, ... })`
- session 名规范化:`ccteam-bg-<job_id>` 与 mode 3 的 `ccteam-chat-<slug>-<role>` 平行
- `session_recovery.rs` 简化:不再拼 jobs.jsonl 反推 child 状态,直接 `mux.exists("ccteam-bg-<job_id>")` + `mux.subscribe(...)` 接 `process_exited` 事件
- `tmux_test.rs` 的 mock 模式扩展到 mode 2 — 单元测试不需要真正 spawn child,`MockBackend` 注入

**收益**:mode 2 bg agent 跑 1 小时的时候,用户可以 `ccteam peek <job_id>` 看截图 → mode-3-only 的 F38 screenshot **自动**对 mode 2 也工作。

### 3.3 Mode 3a chat(Claude TUI)— 平迁,session 命名沿用

**最简单的迁移**。今天 `ClaudeTuiAdapter` 用 tmux,改用 `MuxBackend` trait 后:
- 后端从 `TmuxBackend` 换 `RmuxBackend`,session 名 `ccteam-chat-<slug>-<role>` 字面不变
- F172 V2 `claude --resume <name>` 路径继续工作 — argv 完全相同,只是供养底层从 tmux 换 rmux
- ccteam-owned `turns.jsonl` 仍是对话原文 SoT,不变
- `transcript_tail.rs` / `turns_mirror.rs` 继续读 Anthropic 内部 transcript jsonl(因为 claude TUI 还在写)

**测试关键路径**:
- W0 spike 已确认:rmux vt100 0.15 对 Claude TUI escape sequence 是否全覆盖
- 红线"永不主动 kill 长 session" 在 RmuxBackend 上的实现:`kill_session` 只在 `budget.exceeded` 或 `ccteam stop` 显式触发,空闲不 kill

### 3.4 Mode 3b chat(Codex app-server)— **设计权衡点**

Codex 的 `codex app-server` 是 JSON-RPC over UDS,**本质不是 PTY 应用**。两个选项:

**Option A:Codex 进 rmux PTY pane,UDS 作 sibling channel**
- argv = `codex app-server --socket <ccteam-owned-uds-path>`
- Codex 进程在 mux 里供养,但它自己 bind 自己的 UDS,JSON-RPC 协议仍走那条 socket
- PTY 流(stdout/stderr)只用作:进程存活探测 + idle/exited 事件
- 优点:统一供养、统一 attach 入口、统一资源 ledger
- 缺点:多套了一层 PTY,Codex 的 log 可能被 PTY ANSI escape 噪声化(可忽略,因为业务面读 UDS 不读 PTY)

**Option B:Codex 维持独立 supervisor,与 rmux 平行**
- 沿用 `codex_app_server.rs` 现状
- 优点:零迁移风险
- 缺点:**unification 破坏** — vendor adapter 仍是 3 套形态,§2.2 红利打折

**推荐 Option A**,理由:
1. Codex 进程在 PTY 中跑无额外开销;Codex CLI 本来就支持 TTY mode 输出(只是更常用 JSON-RPC)
2. 统一收益(crash-resilience / peek / cost ledger)对 mode 3b 同样适用
3. W0 spike 验证一次 Codex-in-PTY 行为正常即可(预计 0.5 天)

### 3.5 Mode 4 human-approval — 不动,但 wait state 可用 mux typed event

V0.6.1 F124 `mode: human-approval` block 走 `progress.jsonl::plan_decision`(IM round-trip 写)。**这部分不动** — human-approval 不涉及子进程供养。

**可选优化**:`plan_timeout` 今天靠 ccteam 内部 timer + watchdog;统一架构后可让 `RmuxBackend::subscribe` 暴露 typed `human_approval_timeout(plan_id)` event(如果对应 session 的 idle 超阈值)。**非必须,不在 V0.8 主线**。

---

## 四、`MuxBackend` trait 完整设计

### 4.1 trait 定义(放 `crates/ccteam-core/src/mux/mod.rs`)

```rust
//! Unified execution surface for mode 2 / 3a / 3b child supervision.
//! Mode 1 in-proc has zero-cost `InProcBackend` implementation.

use anyhow::Result;
use bytes::Bytes;
use std::path::PathBuf;
use std::pin::Pin;
use tokio_stream::Stream;

#[derive(Debug, Clone)]
pub struct MuxSessionSpec {
    pub name: String,                 // e.g. "ccteam-bg-job42" / "ccteam-chat-foo-critic"
    pub argv: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: (u16, u16),             // pty cols/rows, default (200, 50) for compat
    pub kind: MuxSessionKind,
}

#[derive(Debug, Clone, Copy)]
pub enum MuxSessionKind {
    BgJob,        // mode 2 — short-ish, expect exit
    ChatLong,     // mode 3a/3b — long-lived, survive disconnect
    InProc,       // mode 1 — never IPC
}

#[derive(Debug, Clone)]
pub struct MuxSessionHandle {
    pub name: String,
    pub backend: BackendKind,
    pub pid: Option<i32>,             // child pid if known
}

#[derive(Debug, Clone, Copy)]
pub enum BackendKind { InProc, Tmux, Rmux }

#[derive(Debug, Clone)]
pub enum MuxEvent {
    Started { pid: i32 },
    OutputChunk { bytes: Bytes },
    OutputIdle { duration_secs: u32 },
    PatternMatched { regex_id: String, captured: String },
    ProcessExited { code: i32 },
    PaneResized { cols: u16, rows: u16 },
    DaemonReconnected,                // for handle-survives-restart story
}

pub type MuxEventStream = Pin<Box<dyn Stream<Item = MuxEvent> + Send>>;

#[async_trait::async_trait]
pub trait MuxBackend: Send + Sync {
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionHandle>;
    async fn exists(&self, name: &str) -> Result<bool>;
    async fn send_keys(&self, h: &MuxSessionHandle, text: &str) -> Result<()>;
    async fn send_enter(&self, h: &MuxSessionHandle) -> Result<()>;
    async fn capture(&self, h: &MuxSessionHandle, lines: usize, ansi: bool) -> Result<Vec<u8>>;
    async fn pane_dims(&self, h: &MuxSessionHandle) -> Result<(u16, u16)>;
    async fn subscribe(&self, h: &MuxSessionHandle) -> Result<MuxEventStream>;
    async fn register_pattern(&self, h: &MuxSessionHandle, regex_id: &str, regex: &str) -> Result<()>;
    async fn kill(&self, h: &MuxSessionHandle) -> Result<()>;
    async fn list_sessions(&self) -> Result<Vec<MuxSessionHandle>>;
}
```

### 4.2 三个 impl

| impl | 用途 | 启动 |
|---|---|---|
| `InProcBackend` | mode 1 | 直接 `tokio::spawn(future)`,handle 是 `JoinHandle` 的薄包装 |
| `TmuxBackend` | V0.7-V0.8 transition retain | 现 `crates/ccteam-core/src/tmux.rs` 472 LOC 全部重构入 trait,行为零变化 |
| `RmuxBackend` | V0.8 起 default | 依赖 `rmux-sdk`,launcher 调用 `Command::new(std::env::current_exe()?).args(["mux", "daemon", "--socket", ...])` |

backend 选型走 `CCTEAM_MUX_BACKEND={tmux,rmux,inproc-test}` env override,产品默认在 V0.8 起 flip 到 `rmux`。

### 4.3 vendor adapter 改造样例(`ClaudeBgAdapter`)

**Before**(精简伪代码):
```rust
let child = Command::new("claude").arg("--bg").args(...).spawn()?;
// 自己起 tail thread 读 jobs.jsonl
// 自己起 watchdog 等 child.wait()
// 自己处理 ccteam 重启 reattach 走 session_recovery.rs
```

**After**:
```rust
let handle = mux.spawn(MuxSessionSpec {
    name: format!("ccteam-bg-{}", job_id),
    argv: vec!["claude".into(), "--bg".into(), ...],
    kind: MuxSessionKind::BgJob,
    ...
}).await?;

mux.register_pattern(&handle, "claude_rate_limit", r"rate limit exceeded.*reset at (.+)").await?;

let mut events = mux.subscribe(&handle).await?;
while let Some(ev) = events.next().await {
    match ev {
        MuxEvent::ProcessExited { code } => break,
        MuxEvent::PatternMatched { regex_id: "claude_rate_limit", captured } => {
            // F-finding 自愈逻辑,而非 grep pane bytes
        }
        _ => {}
    }
}
```

vendor adapter 不再持有 `Child`、不再起 watchdog、不再起 tail thread — 全由 mux daemon 负责。

---

## 五、`ccteam mux daemon` 隐藏子命令

### 5.1 设计

镜像 rmux 自己 `src/main.rs::run_hidden_daemon` 模式:
- `ccteam` 顶层 CLI 加 hidden 子命令 `mux daemon`(`--hidden` 不显示在 `--help`)
- 当 `RmuxBackend` 通过 `rmux-sdk::Rmux::builder().launcher(...)` 启动 daemon 时,launcher 闭包调:
  ```rust
  Command::new(std::env::current_exe()?)
      .args(["mux", "daemon", "--socket", &socket_path.to_string_lossy()])
      .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
      .spawn()
  ```
- `ccteam mux daemon` 函数体直接调 `rmux_server::ServerDaemon::new(config).bind().await.wait().await`
- daemon 进程独立于 ccteam 主进程生命周期(detached + setsid),ccteam 重启时 sessions 不死

### 5.2 socket path 约定

| OS | path |
|---|---|
| Linux/macOS | `~/.ccteam/run/mux.sock`(owner-only 0600) |
| Windows | per-user Named Pipe `\\.\pipe\ccteam-mux-<user>` |

与 rmux 默认 `/tmp/rmux-{uid}/default` **不共用**,完全 ccteam owned。避免与用户自己装的 rmux server 串扰(red line "socket 路径项目隔离",原 feasibility §10.11)。

### 5.3 与 `ccteam-imd` 关系

`ccteam-imd`(V0.6.1 F130 IM supervisor)是 ccteam **主进程内** 的 tokio 任务,守 14+ IM 平台;**进程级别它跟 ccteam orchestrator 同生死**。

`ccteam mux daemon` 是 **sibling 独立进程**,跟 ccteam orchestrator 不同生死(它要 survive orchestrator 重启)。

二者**不冲突**,各管各的领域。daemon 起停由 `RmuxBackend::connect_or_start` 自动处理 — ccteam orchestrator 第一次需要 mux 时拉起 daemon,空闲 N 分钟无 session 时 daemon 可选自杀(`--idle-timeout` flag,默认 off 因 mode 3 chat 是 24/7)。

---

## 六、wave-by-wave 实施计划(V0.8 主线 Epic)

> 不是 V0.7 主线 — V0.7 已锁定 IM 覆盖 / HumanApproval / chat memory(CLAUDE.md §一)。本 epic 作 **V0.8 主线**,~6-8 周专注 wave-by-wave 推。

### W0 spike(1 周)

| spike | 预算 | 标准 |
|---|---|---|
| **S1** rmux 作 Cargo dep 工作树膨胀检查 | 0.5d | `cargo add rmux-sdk rmux-client rmux-server` → workspace test 时间 / clippy 时间增量 < 20%;ccteam release binary < 4MB(单 binary) |
| **S2** Claude TUI 在 rmux 下 escape sequence 全覆盖 | 1d | `claude --resume <name>` 全交互(包括 `/compact /new /clear`)在 RmuxBackend 下 byte-for-byte 等效于 tmux;sixel/iterm-image 序列若有不支持,记录 followup |
| **S3** macOS verification(rmux 上游 macos: skipped) | 1d | macOS arm/x86 跑 rmux-sdk integration tests + Claude TUI 一遍;失败 → 给上游提 issue,自己 patch 路径评估 |
| **S4** Codex `app-server` in PTY 行为 | 0.5d | `codex app-server` 在 mux PTY 下 bind UDS 正常,JSON-RPC 双向工作;PTY stdout ANSI 噪声不影响 UDS |

**Go/No-Go**:
- 全过 → 进 W1
- S1 不过(workspace 膨胀大)→ fallback "bundle rmux 二进制" 路径(原 feasibility §6 短期路径)
- S2 不过(Claude TUI 有不支持 escape)→ 退 V0.9,先给 rmux 上游 PR
- S3 不过(macOS 不工作)→ 上游修通前不动 default flip
- S4 不过 → mode 3b 暂走 Option B(独立 supervisor),mode 2/3a 先统一

### W1:`MuxBackend` trait + `TmuxBackend` 现状包装

- 新建 `crates/ccteam-core/src/mux/{mod.rs,inproc.rs,tmux_backend.rs}`
- 现 `crates/ccteam-core/src/tmux.rs` 472 LOC 全部重构入 `TmuxBackend impl MuxBackend`
- 全部 caller(`commands.rs` / `mcp_serve.rs` / `claude_tui.rs` / `pty.rs` web SSE)改走 trait 注入
- `CCTEAM_MUX_BACKEND` env 引入(默认 `tmux`)
- **验收**:cargo test 1639/1 保持;clippy 0 warning;ccteam-cli / mcp / web e2e 全通

### W2:`RmuxBackend` impl + `ccteam mux daemon` 子命令

- `Cargo.toml` 加 `rmux-sdk = "0.3"` / `rmux-client = "0.3"` / `rmux-server = "0.3"`(workspace-pin 同 minor)
- `crates/ccteam-cli/src/commands.rs` 加 hidden subcommand `mux daemon --socket <path>` → `rmux_server::ServerDaemon::new(...).bind().wait()`
- `crates/ccteam-core/src/mux/rmux_backend.rs` 用 `rmux-sdk::Rmux::builder().launcher(...).connect_or_start()` 起 daemon
- `MuxBackend` 全部方法在 RmuxBackend 上实现
- **验收**:`CCTEAM_MUX_BACKEND=rmux` 下 `cargo test --features rmux-backend` 全通(mode 3a Claude chat 端到端)

### W3:mode 2 bg 进 mux

- `ClaudeBgAdapter` + `CodexExecAdapter` 改 spawn via `MuxBackend::spawn`
- `session_recovery.rs` 简化 → 改用 `MuxBackend::exists` + `MuxBackend::subscribe` 拿 ProcessExited
- jobs.jsonl 仍由 child 写、ccteam tail(因为这是业务文件 contract,不是 process state)
- F38 screenshot 工具自动支持 mode 2(`ccteam peek <job_id>` 出 PNG)
- **验收**:V0.6 现有 bg 测试全通;新增 `cargo test mode_2_in_mux_*` 覆盖 crash-resilience 路径

### W4:typed event → progress.jsonl 桥

- `MuxBackend::register_pattern` 在 daemon side 注册 regex,daemon emit `PatternMatched { regex_id, captured }`
- `ccteam-core::progress::translator` 模块新增:`MuxEvent` → `progress.jsonl::{turn_done, agent_complete, rate_limit_hit, ...}`
- 删 / 折叠 现有轮询逻辑(尤其 jobs.jsonl idle 探测)
- 红线核对:**ccteam 业务代码不 grep bytes**,只注册 regex_id;regex 来源是 `crates/ccteam-core/src/mux/patterns.rs` 静态常量列表(类似 `STUB_TOOLS` 模式),不是 vendor adapter 字符串拼接
- **验收**:`amux` 同等的 rate-limit 自愈在 RmuxBackend 下走通;红线 grep("CLAUDE.md §三") 不触发

### W5:`ccteam attach` 走 rmux-client

- `crates/ccteam-cli/src/commands.rs::attach` 改用 `rmux-client::attach` 库 API(不 shell out)
- 跨 mode 工作:`ccteam attach <slug>` 走 chat session;`ccteam attach --job <job_id>` 走 bg session
- Windows attach(rmux-client Windows ConPTY 路径)首次跑通
- **验收**:Linux + macOS + Windows attach 各跑一遍 mode 3a + mode 2 dogfood

### W6:Codex app-server 进 mux + 全平台 CI

- `CodexAppServerAdapter` 改走 `MuxBackend::spawn`,argv 加 `--socket <ccteam-owned-uds>`
- GitHub Actions 矩阵加 Windows runner(`windows-latest`)+ macOS arm runner(`macos-14`)
- **验收**:三平台 baseline 全通;CI 时间增量 < 50%

### W7:doc-syncer + ship gate + V0.8.0 tag

- tier-1 docs sync(CLAUDE.md §一 baseline + tech-design / interfaces / dev-coupling-audit / claude-code-tool-surface)
- 用户面 docs(README + quickstart + user-manual + advanced/mux.md 新写)
- 版本归档 `docs/versions/v0-8-0/README.md`
- `workspace.package.version = "0.8.0"`,commit prefix `v0.8.0:`
- Ship gate 第 12 项检查通过

### V0.8.x patch:flip default + 老路径退化

- V0.8.0 默认仍 `CCTEAM_MUX_BACKEND=tmux`(保 V0.6 行为)
- V0.8.1 patch 收集 dogfood 反馈 → flip 默认到 `rmux`
- V0.8.2 patch 标记 `TmuxBackend` deprecated

### V0.9:删 `TmuxBackend`

- 按 CLAUDE.md §五"pre-v1.0 不留兼容 shim",V0.9 直接删 `tmux.rs` + `TmuxBackend` impl
- `CCTEAM_MUX_BACKEND` env 退化为 dev-only test injection switch(`rmux` / `inproc-test`)
- 系统 tmux 依赖正式从 install.sh 移除;Windows 不再要 WSL2

---

## 七、CLAUDE.md §三 红线逐条对照

| 红线 | mode 1 in-proc(InProcBackend)| mode 2 bg(RmuxBackend BgJob) | mode 3a chat(RmuxBackend ChatLong) | mode 3b chat(RmuxBackend ChatLong) |
|---|---|---|---|---|
| **文件系统是控制平面** | — | 守 — jobs.jsonl + progress.jsonl 仍是文件 SoT | 守 — turns.jsonl + progress.jsonl | 守 — Codex UDS + progress.jsonl |
| **progress.jsonl 唯一 state SoT** | — | 守 — mux event 翻译进 progress.jsonl,不分叉 | 守 | 守 |
| **No prompt injection** | 守 | 守 — argv 传 prompt 不经 send_keys | 守 — `.claude/agents/<role>.md` 静态读 | 守 |
| **每次 spawn = fresh 1M context** | — | 守 — RmuxBackend::spawn 必起新 session,argv 决定 vendor 行为 | N/A(chat 复用 context 是 feature) | N/A |
| **永不主动 kill 长 session** | 守 | 守 — bg 自然退出;ccteam 不强 kill | 守 — `kill_session` 只在 budget.exceeded 或显式 stop 触发 | 守 |
| **不解析 tmux 终端输出** | — | 守 — 注册 regex_id,daemon emit typed event,业务代码不 grep | 守 | 守 |
| **fix-loop 撞 3 次必 escalate** | 守 | 守 — fix_counts 仍在 progress.jsonl,与 mux 解耦 | 守 + AgentPath depth limit | 守 |
| **ccteam-core 零 team 字面量** | 守 | 守 | 守 | 守 |
| **跨项目记忆走官方接口** | 守 | 守 | 守 — `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`(不被 mux 影响)| 守 — `~/.codex/AGENTS.md` |
| **新建项目走 `<projects_root>/<team>-<slug>/`** | — | 守 — session 名 `ccteam-bg-<job_id>` 是 mux 内 namespace,与项目目录正交 | 守 — `ccteam-chat-<slug>-<role>` 沿用 | 守 |
| **root README.md MUST be English** | 守 | 守 | 守 | 守 |
| **HITL approval state SoT(mode 4 V0.6.1 F124)** | — | 守 — progress.jsonl::plan_decision 不变 | 守 | 守 |

**新加红线建议**(V0.8 doc-first 评审时确认):

- **RM1. `MuxBackend` 是子进程供养唯一接口** — ccteam 业务代码不允许 `std::process::Command::spawn` 长跑子进程(短跑工具 OK,如 git/cargo)。新红线 grep gate:`grep -rn "Command::new.*spawn" crates/ccteam-{core,cli}/src/` 排除 allow-listed 短跑工具后必须 0 命中(类似 V0.6.6 F171 STUB_TOOLS pattern)
- **RM2. mux pattern registry 集中化** — daemon-side 正则 pattern 集中在 `crates/ccteam-core/src/mux/patterns.rs` 静态常量,vendor adapter 不允许内联字符串。新 grep gate

---

## 八、风险登记 + 缓解

| # | 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|---|
| R1 | **rmux 0.3.1 fresh public preview(2026-05-25 published)** | 中 | 上游 bug 拖 ccteam | V0.8 W0 S1-S4 spike + 不 flip default 到 V0.8.1;V0.8 整周期 dogfood,bug 提上游 PR 不自己 fork |
| R2 | **rmux PTY-only,无 headless process-mode** | 高 | mode 2 bg 多套 PTY 包裹 | 已确认对 mode 2 不构成 blocker(jobs.jsonl 是主 I/O,PTY 是 supervision 通道);可选给 rmux 上游提 headless feature request |
| R3 | **macOS 在 rmux 上游 feature-inventory 全 `skipped`** | 中 | ccteam 在 macOS 不稳 | W0 S3 spike;失败则给上游 patch + 自己 macOS CI 跑 baseline,在上游补齐前不 flip default |
| R4 | **Codex `app-server` 在 PTY 行为** | 低 | mode 3b 可能要走 Option B(独立 supervisor)| W0 S4 spike;失败时 mode 3b 走 Option B,unification 红利打 80 折但仍 OK |
| R5 | **workspace 膨胀(rmux 11 crate)** | 中 | clippy / test 时间膨胀;binary 体积涨 | W0 S1 测增量;> 20% 时考虑只 dep 必要 crate(rmux-sdk + rmux-client),server 走 bundle 独立 rmux 二进制 fallback |
| R6 | **Anthropic 上游可能选 Zellij**([issue #31901](https://github.com/anthropics/claude-code/issues/31901))| 中 | 选错赛道生态分裂 | `MuxBackend` trait 设计就是为可换 backend;若上游选 Zellij,加 `ZellijBackend` impl 即可,业务面零感知。**这是 trait 设计的核心价值** |
| R7 | **多 client attach 仲裁不像 tmux 自然** | 低 | dev workflow 两窗口看同一 session 体验差 | rmux 已支持 `--mode=observer`/`controller`(原 feasibility §10.10);ccteam-attach 默认 controller,doc 文案明确 |
| R8 | **用户 `.tmux.conf` 习惯丢失** | 低 | 老用户 ccteam attach 后键绑奇怪 | V0.8 release notes 明确告知;rmux 已支持 `tmux.conf` migration fallback(`references/rmux/README.md` §Configuration 段)读取 `~/.tmux.conf` 部分键绑 |
| R9 | **V0.7 主线 + V0.8 Epic 并行排期** | 中 | 时间冲突 | 不冲突 — V0.7 ship 后(预计 6-8w)再起 V0.8;V0.8 doc-first 可与 V0.7 收尾 1-2w 重叠(只是 PRD 阶段) |
| R10 | **CLAUDE.md "Windows 走 WSL2" 红线松绑后,Windows 用户群增加 → bug 面增加** | 中 | 维护负担 | W6 GitHub Actions Windows runner 全开;Windows-specific bug 单独 issue label;前期保守对外口径"Windows native 是 V0.8 preview,production V0.9" |

---

## 九、与前篇 feasibility 调研的关系 + 决策矩阵更新

### 9.1 前篇覆盖了什么

[`docs/research/rust-tmux-rewrite-feasibility.md`](rust-tmux-rewrite-feasibility.md)(remote branch `claude/rust-tmux-rewrite-feasibility-pD3EB`,基线 V0.6.7):
- 评估了 4 个选项(A 自研 / B vendor 二进制 / C install.sh 兜底 / D 渐进 hybrid)
- 推荐 V0.7-A1:依赖 Helvesec/rmux + MuxBackend trait + adapter
- §11 完整竞品全景(rmux / cmux / wmux / amux / tmux-ide / isomux / shpool / Zellij)
- §12.2 amux 的 pane scraping 启发 → ccteam 改"daemon-side typed event"

### 9.2 本篇推进了什么

| 维度 | 前篇 | 本篇 |
|---|---|---|
| 目标 | **替换 tmux** 作 mode 3 backend | **统一 mode 2 / 3a / 3b**,在 ccteam 子进程供养层做架构收口 |
| trait 范围 | `MuxBackend` 替 tmux 11 命令 | `MuxBackend` 是子进程供养唯一接口(覆盖 mode 1/2/3) |
| bundling | bundle rmux 二进制 OR Cargo dep | **Cargo dep + `ccteam mux daemon` re-exec**(单 binary) |
| 时间窗 | V0.7-A1 path,2-3w | V0.8 主线 Epic,6-8w(整 unification) |
| 默认 backend flip 时机 | "V0.7 flip" | **V0.8.1 flip,V0.9 删 tmux** |
| Codex 3b 怎么办 | 前篇未细化 | 本篇明确 Option A(进 mux)推荐 |
| mode 1 怎么办 | 前篇未提 | 本篇 `InProcBackend` 保留 in-proc zero-cost |
| 资源 ledger | 前篇 §10.3 提到 cgroup | 本篇明确接入 ccteam-cost(rmux daemon 是 OS-level 隔离点) |
| 设计哲学 | 前篇 §10 列了 12 agent-first 支柱(平铺)| 本篇 §十一 从"用户假设根本翻转(ops vs agent)"切入,组织为 Tier S/A/B + rmux 现状覆盖度 + 上游协作策略,可直接驱动 V0.8 W 排期 |

### 9.3 修订后的"应该走哪条路"决策矩阵(对照前篇 §12.3)

| 选项 | 路径 | 工作量 | 收益 | 推荐度 |
|---|---|---|---|---|
| **V0.7-D**(短期止血) | install.sh 自动装 tmux | 0.5d | 减少 ccteam onboarding 报"tmux missing" | ✅ V0.6.8 / V0.7.0 早期落地,**与 V0.8 Epic 不冲突** |
| **V0.8-A**(本篇主推) | 嵌入 rmux Cargo dep + MuxBackend 统一 mode 2/3a/3b | 6-8w | 7 大红利全收(§2.2)+ Windows 原生 + 单 binary | **★★★★★ 强推 V0.8 主线 Epic** |
| V0.8-A1-narrow | 同 V0.8-A 但只统一 mode 3(不动 mode 2 bg)| 3-4w | 退化版,只收 Windows + 安装零摩擦 | ★★★ 若 W3 mode 2 改造 spike 不顺利,fallback |
| V0.8-B | 学 amux 做 pane scraping 不换 mux | <1w | 拿 rate-limit 自愈 | ❌ 拒(违反红线,前篇已驳)|
| V0.8-C | 转 Zellij | 6-8w | 跟 Anthropic 上游 | ⚠️ 等 [issue #31901](https://github.com/anthropics/claude-code/issues/31901) 表态;`MuxBackend` trait 留好 ZellijBackend 接入位 |

---

## 十、附录

### 10.1 单 binary 体积估算

V0.6.7 当前 ccteam release musl-static:
- linux x64:~1.8 MB
- linux arm64:~1.7 MB
- macOS arm64:~2.1 MB

V0.8 加 rmux Cargo deps(rmux-sdk + rmux-client + rmux-server + 间接依赖):
- 估增量 ~1.5-2.0 MB(rmux upstream 单 binary 是 ~3 MB,与 ccteam 共用 tokio / serde 等大部头后增量缩水)
- 预估 V0.8 ccteam release musl-static ~3.5-4.0 MB

仍属"单 binary curl 装一行" friendly 区间(对比 docker image 100+ MB)。

### 10.2 Anthropic 上游动向监测

需在 V0.8 W0 doc-first 阶段 + 整 Epic 周期持续监测:
- [Claude Code issue #31901](https://github.com/anthropics/claude-code/issues/31901) — Zellij as Agent Teams backend 评估
- Anthropic 是否官方推荐某个 mux —— 影响 V0.8.x flip default 时机

**应急响应**:若 Anthropic 在 V0.8 收尾前官宣 Zellij,新增 `ZellijBackend` impl(估 2-3w),`MuxBackend` trait 不动。

### 10.3 reference 目录约束

`references/rmux/` 已就位(本调研写作期间用户检出 2026-05-26)。按 CLAUDE.md "vendor 红线":
- `references/rmux/` 走 `.gitignore`,**不入库**
- 实际依赖在 `Cargo.toml` 走 crates.io 拉(等 v0.3.1 publish 完成确认;若未 publish,临时 `git = "https://github.com/helvesec/rmux"` rev-pin)
- `CCTEAM_RMUX_BIN` env override **不需要** — daemon 走 `ccteam mux daemon` 自 re-exec,不依赖独立 rmux 二进制

### 10.4 未决问题(V0.8 doc-first 评审要回答)

1. **W3 mode 2 PTY 包裹是否影响 child 行为?** Claude `--bg` 现有 jobs.jsonl 写入路径在 PTY 下行为是否变化(尤其 stderr ANSI 着色被 PTY merge)?spike S2 顺带验
2. **`ccteam mux daemon` 与 systemd-user-unit 集成?** Linux 用户可能想跑 daemon 在 boot 启动,而非 ccteam 第一次需要时;V0.8.x patch 加 `ccteam mux install-systemd-unit` helper?
3. **多 ccteam project 共享单一 daemon?** 推荐共享(daemon 自带 session namespace `ccteam-bg-*` / `ccteam-chat-<slug>-*`);但 cost ledger 要不要 per-project 隔离需评估
4. **`turns.jsonl` SoT 是否仍要 ccteam owned?** mode 3a 今天 ccteam 自己写 turns.jsonl(F172 V2 不依赖 Anthropic 内部 `~/.claude/projects/`);统一后 rmux pane snapshot 也可作 turns.jsonl 来源 — 二者是否合并?**倾向保留 ccteam owned**(语义边界清晰),mux event 仅作 supplementary
5. **mode 4 human-approval typed event 接入是否值得?** §3.5 列了可选优化;评估 ROI 后决定 V0.8 内做还是 defer

---

## 十一、Agent 时代为何要重新设计 mux:tmux 包袱 vs agent 真需求

> 本节回应根本问题:**tmux 1992 年从 GNU screen 分叉、2007 年正式发布,设计假设的 user 是运维 / 人类开发者。agent 时代,user 已经不是人类了 — 是 orchestrator(ccteam / Claude Code / Codex 自身)。哪些 tmux 包袱可以丢?为 agent 重新设计应有哪些特性?**
>
> 前篇 feasibility §九 列了 tmux 14 个历史包袱,§十 列了 rmux 12 个 agent-first 支柱。本节从**用户假设根本翻转**的角度重新展开,给出 ccteam V0.8 落地的优先级清单。

### 11.1 用户假设的根本翻转

| 维度 | tmux/screen ops user(1992-2010s) | ccteam agent orchestrator(2024+) |
|---|---|---|
| **cardinality** | 1 user, 5-10 sessions | 1 user, **50-100 sessions**(squad × bots × workflows) |
| **interaction modality** | 视觉读 pane / 手敲 keystroke | 结构化 snapshot(typed cells)+ 结构化 input(argv / RPC bytes) |
| **detach 语义** | "我去做别的,任务接着跑" | "我不在,触发 typed event 通知;我重连,获得 deterministic snapshot" |
| **session 形态认知** | 个人长期所有的工作区 | 短-中-长 **3 种 lifecycle 混杂**,每类需不同 supervision policy |
| **layouts** | 分屏 / 多 window 是核心 UX | 每 session 1 pane,**layouts 是死代码** |
| **配置来源** | `.tmux.conf` 个人化 | workflow.yaml 团队模板,**零个人化空间** |
| **stdout/stderr** | 终端 ANSI 视觉读 | 主 I/O 是结构化文件(jobs.jsonl / progress.jsonl),stdout 是 supervision 通道,**非业务通道** |
| **history 查询** | 视觉 scroll back | 按时间 / regex / event 类型**typed 查询** |
| **failure mode** | "我没看到 / 我没存" | "agent 死循环 / 超 budget / rate-limit / context overflow" — 全部需 **typed escalation** |
| **multi-tenant** | "1 host = 1 user 自己" | 1 host 可能 5 squad 并行,需 namespace + 资源公平 |
| **identity** | 名字 = 身份,collision 是 user 自己问题 | UUID = identity,name 是 label;collision **必须**daemon 防御 |
| **观测面** | 视觉读 pane 是真相 | typed event 是真相,**pane bytes 是噪声** |

**根因诊断**:tmux 把**长跑进程供养**和**人类终端 UX**两件事耦合在同一 binary 内。**agent 时代,这两件事应该解耦** — supervision 是 daemon 的事,UX(attach / 分屏 / 个人键绑)是 client-side 可选层。rmux 在协议层迈出第一步(typed SDK + headless RPC),但 UX 包袱仍在(tmux-compatible 90 命令、status bar、copy mode、format-string DSL)。

### 11.2 为 agent 应重新设计的 12 个特性

按"对 ccteam 价值降序" + rmux 现状覆盖度排:

#### Tier S — 架构核心(V0.8 W0-W7 必有)

**S1. supervision 与 UX 解耦**
- daemon 暴露 **headless RPC** 是一等公民,attach client 是可选附加层
- 删除 tmux UX(prefix key / copy mode / mouse / status bar / choose-tree / `.tmux.conf` 求值器 / format-string DSL)→ binary 体积 -60%、攻击面 -60%、vt100 escape 兼容矩阵 -60%
- **rmux 现状**:60% — daemon-first 已具备,但保留 tmux-compatible 90 命令兼容,UX 路径仍在 binary 内
- **ccteam 行动**:`CCTEAM_MUX_BACKEND=rmux` 启用时通过 feature flag 关 UX;`ccteam attach` 自写薄客户端(基于 `rmux-client::attach`),detach 用 `Ctrl-]`(telnet 风,不与 shell 应用冲突)而非 tmux `prefix + d`

**S2. typed event 取代字节流轮询**
- daemon 暴露:`Started{pid}` / `OutputChunk{bytes}`(底层,业务面不消费)/ `ProcessExited{code}` / `OutputIdle{duration}` / **`PatternMatched{regex_id, captured}`**
- **PatternMatched 是关键创新** — 客户端**预注册** regex(rate-limit / context-overflow / "waiting for input:" / Codex `task_complete` 等),daemon server-side 匹配,**业务代码零 grep**
- 严守"不解析 pane output"红线(CLAUDE.md §三)的同时拿到 amux 同等的 rate-limit 自愈能力(前篇 §12.2)
- **rmux 现状**:70% — `PaneOutputStream` 流式 + `Locator::wait` 一次性 wait-for-text 有,**持久化 regex 注册 + 按 ID 广播 event 的 API 缺位**
- **ccteam 行动**:给 rmux 上游提 `RegisterPattern` RPC PR;ccteam-side 集中 `crates/ccteam-core/src/mux/patterns.rs` 静态字典(参考 V0.6.6 F171 STUB_TOOLS 模式)

**S3. session metadata 是 typed key-value**
- session 名仅作 display label,**真实 identity = SessionId (UUID)**
- 附带 typed metadata:`{ role: "critic", workflow: "review-flow", parent_session: <uuid>, spawned_by_msg_id: "...", squad: "...", cost_so_far_usd: 0.42 }`
- RPC:`session.set_metadata(k,v)` / `daemon.find_sessions_by_metadata(filter)` **server-side typed 查询**
- **rmux 现状**:40% — `SessionName` + `SessionId` 双轨已有;`creation_tags` 是 client-side 一次性,**无 server-side typed query**
- **ccteam 行动**:V0.8 W4 把 ccteam 当前用 session 名字符串编码的 `<slug>` / `<role>` / `<bot>` 全部迁到 metadata;F172 mode-3 命名 `ccteam-chat-<slug>-<role>` 简化为随机 UUID + metadata role/slug

**S4. 资源 budget 作 first-class**
- 每 session 可声明:`max_wall_clock` / `max_cpu_seconds` / `max_rss_bytes` / `max_output_bytes`
- 临近阈值 daemon emit `BudgetWarning(threshold, used, limit)` 软 event
- 超阈值 daemon **freeze**(SIGSTOP/Suspend)而非 kill,orchestrator 决定 resume / kill / migrate;**与 tmux/rmux 都不同** — 它们假设 user 自己看着控制
- 内部走 cgroup v2(Linux)/ Job Object(Windows)/ rlimit + posix_spawn(macOS)
- **rmux 现状**:**0%** — session 跑到死或显式 kill
- **ccteam 行动**:V0.8 W4-W5 加 `MuxSessionSpec.budget`;先接 Linux cgroup v2,macOS/Windows 留 V0.9 followup

#### Tier A — 高价值(V0.8 W3-W7 落地)

**A1. spawn 因果链作 first-class**
- 每 session 携带 `parent_session_id: Option<SessionId>`,daemon 维护全 spawn 树
- RPC:`daemon.spawn_tree(root)` → 全子孙;agent A 触发 escalation,daemon 自动按树 typed-pause 后代
- **rmux 现状**:0%
- **ccteam 行动**:V0.8 W4 把 ccteam 现有 `AgentPath`(fix-loop depth limit)下推到 mux metadata,daemon 持有树,orchestrator 不再自维

**A2. multi-tenant 隔离**
- 一台 host 跑多个 ccteam squad,每 squad 独立 UDS / namespace
- daemon 启动 `--tenant <id>` 声明,session 命名空间隔离;`list-sessions` 默认只看本 tenant
- **rmux 现状**:10% — UDS 用户级隔离(单 user 单 daemon),**单 tenant 内无 squad 隔离**
- **ccteam 行动**:V0.8 W7 留 `--tenant` flag 位,实际接入 V0.9

**A3. 结构化数据 extraction**
- daemon 看到 fenced code block / JSON / Markdown table 自动 emit `StructuredOutput{kind, content}` event
- ccteam 不需要 grep pane bytes 拼装 agent 产物
- **rmux 现状**:50% — `crates/rmux-sdk/src/extract.rs` 有 `PaneTextMatch` 基础,部分覆盖
- **ccteam 行动**:V0.8 W4 复用 + 补 fenced/JSON 上游 PR

**A4. session lifecycle 显式分类**
- `MuxSessionKind::{Ephemeral, LongLived, Daemon}` 决定 supervision 策略:
  - Ephemeral:exit code 是终止信号,exit 后立即 cleanup
  - LongLived:exit 是异常,自动 `claude --resume <name>` respawn(F172 路径)
  - Daemon:用户 dev-server 类,需 explicit kill
- **rmux 现状**:0%(单一 session abstraction)
- **ccteam 行动**:V0.8 W1 在 `MuxBackend` trait 内置(本篇 §四已含 `MuxSessionKind`)

#### Tier B — 锦上添花(V0.9+ followup)

**B1. deterministic snapshot for crash recovery**
- daemon 周期性 / on-demand 落 `snapshot.bin`:vt100 screen + cursor + N 条 scrollback + metadata + budget counters
- daemon / orchestrator 重启 → 读 snapshot → 恢复 byte-for-byte 一致状态
- 关键差别:今天 ccteam 重启后只能看 transcript jsonl 推断 chat 历史;snapshot 后能恢复 pane screen state
- **远期 V0.9+,W4 metadata 落盘是其前置步骤**

**B2. record-replay**
- daemon 录全 session input/output,新 child 可 replay → agent A/B 测试 / regression 黄金路径
- **V1.0+ 才有 ROI**

**B3. server-side WASM hook(禁 shell)**
- tmux hooks 是 shell 命令,**不安全**;rmux 是 typed event 给 client
- 中间方案:server-side WASM hook(Zellij 方向)— 不让 shell,但 sandboxed 自定义逻辑
- **V1.0+ 远期**;V0.8 Epic 不引入

**B4. cost meter daemon-internal**
- 每 session 计 `output_bytes_total` / `wall_clock_secs` / `cpu_seconds`,daemon 维护
- ccteam-cost ledger 直接订阅,删 ledger 内的"vendor token 软计费"代码
- **依赖 S4 budget 接入 cgroup,V0.9 同步推**

### 11.3 设计支柱在 ccteam V0.8 Epic 中的优先级总表

| 支柱 | rmux 现状 | V0.8 Wave | 推动方式 |
|---|---|---|---|
| S1 解耦 supervision/UX | 60% | W2 启用 feature flag | rmux 上游 PR + ccteam attach 自写 |
| S2 typed event 基础 | 70% | W4 | **rmux 上游 PR**(RegisterPattern API) |
| S3 typed metadata | 40% | W4 metadata RPC + 命名简化 | rmux 上游 PR |
| S4 资源 budget(Linux cgroup) | 0% | W4-W5 | **ccteam 包装层先做,验证后回灌上游** |
| A1 spawn 树 | 0% | W4 | ccteam metadata 关联;上游单独 PR |
| A2 multi-tenant | 10% | W7 留接口位 | 实际接入 V0.9 |
| A3 结构化 extraction | 50% | W4 复用 + 补丁 | rmux 上游 PR |
| A4 lifecycle 分类 | 0% | W1 trait 内置 | ccteam 主导,设计稳定后回灌 |
| B1-B4 | 0-30% | V0.9+ | — |

**Epic 战略价值修订**:V0.8 unification 不只是"3 套 supervision 合一"的工程整理,更是**给 rmux 上游引入 agent-first 核心抽象**(typed pattern registry / typed budget / typed metadata query / spawn 树 / 结构化 extraction)的契机。这些抽象一旦在上游成立,**rmux 从"tmux 的 Rust 改写"升级为"为 agent 设计的 mux"**,ccteam 是这条路上首批 design partner。

### 11.4 上游协作策略

| 项目 | 主导 | 备注 |
|---|---|---|
| S1 UX feature flag | rmux 上游(我们提需求 + PR)| OSS 友好向 |
| S2 持久化 pattern registry | **rmux 上游 PR**(RFC 形式)| 新 API,需上游评审 |
| S3 metadata server-side query | rmux 上游 PR | 扩 `creation_tags` 到双向 query |
| S4 budget(cgroup hooks)| **ccteam 先做包装层验证,稳定后回灌上游** | OS-specific,先实战再上游 |
| A1 spawn 树 | ccteam 用 metadata 关联,上游单独 PR | 可拆 |
| A3 extraction 扩展 | rmux 上游 PR | `extract.rs` 已开门 |

**为何不 fork rmux**:上游主理人(Helvesec)X 社区表态明确 agent-friendly 方向(README 自述 "agentic era");协作比 fork 可持续。fork 是 emergency fallback(若 rmux 走商业化关闭 OSS 或路线分歧 — 概率低)。

### 11.5 推论:ccteam 在 mux 上的战略位置

```
                    通用终端 mux                     agent-specific mux
                    (人类 user 设计)                  (orchestrator user 设计)
                          │                                  │
   单一产品 ──────────────┼──────────────────────────────────┼───
                  tmux ●                                    ?
                                                            │
                  Zellij ● (WASM 插件)                       │
                                                            │
   开源生态 ──────────────┼──────────────────────────────────┼───
                  rmux ● ────── V0.8 Epic ──────►   rmux (升级) ●
                  (tmux 兼容)                       (S1-S4 + A1-A4)
                                                          ▲
                                                          │
                                                          │
                              ccteam(V0.8 Epic design partner)
                              通过 trait 设计 + dogfooding + 上游 PR
                              把 rmux 推到右下角象限
```

ccteam 占"role-level orchestration"(workflow.yaml + 27 MCP 工具)的领跑位;rmux 占"session-level supervision"位置。**二者绑定推进**:ccteam 给 rmux 提供 agent-era 需求清单 + 真实 dogfooding,rmux 给 ccteam 提供干净的 mux 抽象。**这是 V0.8 Epic 超越"换掉 tmux"工程账面收益的更深远价值**。

---

## 十二、统一 mux 对 IM 入站/出站 + 所有 CLI agent 抽象的启示

> 本节回应:**新的统一 mux 引入对 ccteam IM 消息入站/出站设计是否有帮助?能否进一步统一 Codex / Claude Code / 未来其他 CLI 形态 agent + IM 的架构抽象?**
>
> 结论先行:**是 — 但分两步走**。V0.8 Epic 先把 mux 在"agent 子进程供养"维度跑通(本篇 §三-§六)。**V0.9 把同一 daemon event bus 扩到 IM channel,把所有"长跑双向 I/O 端点"统一到 `ChannelBackend` trait** — 届时 CLI agent / IM 平台 / 文件 watcher / MCP server uplink 等都是 daemon 名下的 channel,ccteam orchestrator 只跟一个 typed event bus 对话。

### 12.1 ccteam IM 现状(为何相关)

`crates/ccteam-imd/`(V0.6.1 F130 折入 ccteam 主进程,V0.6.8 F193 bot-to-bot mpsc)是 14+ IM 平台 supervisor,核心模块:

| 模块 | 职责 | 当前实现方式 |
|---|---|---|
| `inbound.rs` | 解析 IM 平台来的 user/group message → 路由到 ccteam 业务面 | 14 平台各自 HTTP poll / webhook / WS,统一翻译成内部 ChannelMessage |
| `outbound.rs` | 把 `chat_*` progress 事件 + agent 主动消息 → 推送到 IM | 读 `turns.jsonl` + progress.jsonl → 转 IM 平台 SDK send-message API |
| `bot_mpsc.rs`(F193)| bot-to-bot @mention via daemon-internal mpsc | 主进程内 tokio mpsc channel,**ccteam 重启即丢** |
| `router.rs` | 多 bot 路由 | 主进程内 HashMap |
| `supervisor.rs` | IM 客户端连接 supervise | 主进程内 tokio task,**ccteam 重启即断** |
| `rate_limit.rs` / `sanitize.rs` / `acl.rs` / `nl_admin.rs` / `three_layer_sec.rs` | 安全 / 限流 / 5-keyword 管理 | 业务面逻辑 |

**关键事实**:`supervisor.rs` 是 ccteam 主进程内 tokio task,与 mode 3a chat session **同生命周期**。**ccteam orchestrator 重启 → IM 连接断 → 重连窗口(秒-分钟级)内 user 消息可能丢**(具体平台行为,Telegram long-poll 可补 / Slack RTM 重连可补 / WebHook 必丢)。

### 12.2 IM channel 在 mux 视角下是什么

把 IM 平台抽象到 mux 视角:

| mux session(agent 视角)| IM channel(平台视角)|
|---|---|
| 长跑 child process(`claude` TUI / `codex app-server`)| 长跑连接客户端(Telegram poller / Slack RTM ws / WeChat webhook listener)|
| stdin = orchestrator 写命令 | inbound = IM 平台收 user 消息 |
| stdout = child 输出 | outbound = IM 平台收 bot 消息要发 |
| typed event `ProcessExited` | typed event `ChannelDisconnected{reason}` |
| typed event `PatternMatched` | typed event `InboundMessage{from, text, attachments}` |
| typed event `OutputIdle` | typed event `ChannelIdle{duration}` |
| metadata `{ role, workflow, parent_session }` | metadata `{ platform: "telegram", chat_id, bot_handle }` |
| Lifecycle:LongLived | Lifecycle:Daemon(永不自然退出,只显式 kill)|
| 资源 budget:CPU/RSS/wall-clock | 资源 budget:msg/sec 限流 + API quota |
| crash-resilience:daemon owns child | crash-resilience:daemon owns connection |

**这 11 条对应关系几乎是 isomorphic 的**。差别只在 backend 实现:agent 是 PTY + child process,IM 是 HTTP/WS + connection state。但**从 orchestrator 角度看,两者无差别** — 都是"开口拿 typed event 流,反向写 typed command"。

### 12.3 所有 CLI 形态 agent 的共通抽象

CCteam 今天支持 Claude / Codex,未来必将面对 opencode / aider / gemini-cli / 国内厂商各家 CLI。每接一个新 vendor 都得写一份 adapter:

| vendor | mode 2 adapter | mode 3 adapter |
|---|---|---|
| Claude | `ClaudeBgAdapter`(jobs.jsonl + claude --bg)| `ClaudeTuiAdapter`(tmux + claude TUI)|
| Codex | `CodexExecAdapter`(codex exec --json)| `CodexAppServerAdapter`(codex app-server UDS)|
| opencode | 待写 | 待写 |
| aider | 待写 | 待写 |
| ...每 vendor 都得自己卷一遍 |

**抽象出 CLI agent 的共通形态**:
- argv-based 启动(`claude --bg ...` / `codex exec ...` / `opencode chat ...`)
- 子进程 + 文件 SoT(jobs.jsonl / progress.jsonl / transcript jsonl)
- typed lifecycle(short bg / long chat)
- typed event(rate-limit / context-overflow / waiting-for-input / process-exited)

加上 IM 平台,**所有"orchestrator 不在场仍要持续工作的双向 I/O 端点"共享一个抽象**:

```rust
// 终极统一(V0.9+),不是 V0.8 Epic 范围
pub trait ChannelBackend {
    async fn open(spec: ChannelSpec) -> Result<ChannelHandle>;
    async fn send(h: &Handle, msg: TypedCommand) -> Result<()>;
    async fn subscribe(h: &Handle) -> ChannelEventStream;
    async fn close(h: &Handle) -> Result<()>;
    async fn metadata_set/get/query(...) -> ...;
}

pub enum ChannelSpec {
    Pty { argv, working_dir, env, size },          // mode 2/3 agent
    HttpPoll { endpoint, headers, poll_interval }, // Telegram bot poller
    WebSocket { url, auth, reconnect_policy },     // Slack RTM
    Webhook { listen_addr, secret },               // Discord events / Slack events
    Mpsc { name },                                  // bot-to-bot in-daemon(F193 升级版)
    // future: FileWatcher, McpUplink, K8sLogStream, ...
}

pub enum ChannelEvent {
    // 通用
    Started, Idle{duration}, Disconnected{reason}, BudgetWarning{...},
    PatternMatched{regex_id, captured},
    // PTY 特有
    ProcessExited{code}, OutputChunk{bytes},
    // IM 特有
    InboundMessage{from, text, attachments},
    Typing{from},  // user "正在输入" 状态
    Reaction{from, msg_id, emoji},
}
```

### 12.4 IM 入站/出站在统一 mux 下的具体收益

#### 入站(user → agent)

**今天**:Telegram bot 接收 → ccteam-imd `inbound.rs` 解析 → progress.jsonl 写 `chat_input` 事件 → mode 3 agent 通过 Claude Code hook 触发 / mode 2 agent 通过 jobs.jsonl 看到

**统一后**:Telegram channel session 在 daemon 内 → daemon emit `ChannelEvent::InboundMessage{from, text}` → ccteam 业务面 subscribe → 按 metadata 路由(`chat_id` → `bot_handle` → workflow → agent session)→ daemon 同 event bus 转给目标 agent session

**红利**:
1. **IM 重连 / agent 处理是同一架构** — agent crash 和 IM 平台断连用同一 typed event(`Disconnected{reason}`)、同一恢复路径(daemon respawn or retry connect)
2. **bot-to-bot @mention(F193)不再依赖 ccteam 内 mpsc** — daemon 内 mpsc channel 是一种 `ChannelSpec::Mpsc` backend,bot1 send → mpsc channel → bot2 subscribe;**ccteam 重启不丢消息**(daemon 是 sibling 进程)
3. **多 ccteam project 共享 daemon 时,IM channel 自然 multi-tenant**(支柱 A2)— Telegram bot A 只发给 squad A,Bot B 只发给 squad B,daemon 路由
4. **IM rate-limit 是 daemon 内 budget 触顶**(支柱 S4)— Telegram 30 msg/sec,daemon backpressure 让 agent 输出节流,而非应用层硬拼

#### 出站(agent → user)

**今天**:agent 写 `chat_*` progress 事件 → ccteam orchestrator 读 turns.jsonl + outbound cursor(`OutboundCursor` V0.6.4 race fix)→ 转 IM 平台 send-message API → 平台 deliver

**统一后**:agent 直接 `mux.send_to_channel(channel_id, content)` typed RPC → daemon 内 channel backend 调 IM 平台 SDK → 发出

**红利**:
1. **`OutboundCursor` race 类 bug 在架构上消除** — agent 不再异步通过 turns.jsonl + cursor 拼装"哪些已发哪些没发";daemon 持有 ChannelHandle,send 即写 ledger
2. **agent 发消息和 agent 收 stdout 是同一 RPC** — `mux.send(handle, TypedCommand::Bytes(...))` 对 PTY 是 stdin write,对 IM 是 send-message,**agent 不区分**
3. **F38 screenshot + IM mirror 同源** — `ccteam peek --channel telegram-bot-foo` 等价于看最近 N 条 IM 消息 mini-snapshot
4. **跨 IM 平台 send-attachment 统一接口** — daemon 暴露 `TypedCommand::Attachment{kind, bytes}`,Telegram/Slack/WeChat 各自 backend 翻译,agent 不知平台差异

### 12.5 推荐路径:V0.8 不动 IM,V0.9 generalize

V0.8 Epic 已 6-8 周满载(本篇 §六)。**强烈不建议**把 IM unification 塞进 V0.8 — 会同时改太多 surface,test surface 爆炸。

**但 V0.8 trait 设计可为 V0.9 IM unification 留接口位**:

| V0.8 当下设计选择 | V0.9 generalize 影响 |
|---|---|
| trait 名 `MuxBackend` | V0.9 重命名 `ChannelBackend`(rename refactor,机械) |
| `MuxSessionSpec.argv` 直接字段 | V0.9 改 `ChannelSpec` enum,`Pty { argv, ... }` 是 variant 之一 |
| `MuxSessionKind::{Ephemeral, LongLived, Daemon}` | V0.9 加 `ImChannel` / `WebHook` 等 variant(扩 enum,不破坏老 variant) |
| `MuxEvent::PatternMatched` 已 backend-agnostic | V0.9 直接复用,新增 `InboundMessage` variant |
| daemon UDS 协议 typed | V0.9 协议向后兼容扩 channel variant |
| `ccteam mux daemon` 子命令 | V0.9 改 `ccteam channel daemon`(alias 保留)|

**V0.8 trait 设计纪律**(回头看本篇 §四):
- ✅ `MuxSessionSpec` 用结构体 + 字段,不依赖 PTY 概念名词 — 已避免 `pane_id` / `tty_path` 这类
- ✅ `MuxEvent::PatternMatched{regex_id}` 是 backend-agnostic 的 — bytes regex 对 IM 文本同样适用
- ✅ `MuxSessionKind::Daemon` 已存在 — IM channel 的 "永不自然退出" lifecycle 已有位
- ⚠️ trait 名建议 V0.8 直接叫 `ChannelBackend`(避免 V0.9 大 rename refactor)或留 `MuxBackend` 作 sealed-trait alias
- ⚠️ `MuxSessionSpec.argv: Vec<String>` 建议 V0.8 改 `backend: SessionBackend::Pty { argv, ... }` 形式,V0.9 扩 `Http` / `WebSocket` / `Mpsc` 等 variant 是 enum extension(non-breaking)

### 12.6 与 ccteam-imd 现有代码的关系

V0.9 IM unification 不是删 `ccteam-imd`,是**重新分层**:

| 当前 `ccteam-imd` 模块 | V0.9 归属 |
|---|---|
| `supervisor.rs`(连接 supervise)| **下沉到 mux daemon**(channel backend impl)|
| `inbound.rs`(IM 协议解析)| 部分下沉(协议解析 → daemon backend),部分保留(NL 语义路由)|
| `outbound.rs`(协议封装 send)| **下沉到 mux daemon** |
| `bot_mpsc.rs` F193 | **下沉到 mux daemon**(`ChannelSpec::Mpsc` variant)|
| `router.rs` | **保留 ccteam-imd**(高层 bot 路由是业务面)|
| `rate_limit.rs` / `acl.rs` / `sanitize.rs` / `three_layer_sec.rs` | **保留 ccteam-imd**(策略 / 安全是业务面)|
| `nl_admin.rs`(@ccteam 5-keyword)| **保留 ccteam-imd**(NL 解析是业务面)|
| `onboarding.rs` / `credentials.rs` | **保留 ccteam-imd**(token / oauth 流是业务面)|
| `transport/` 14 平台 SDK 适配 | **部分下沉**(连接 + 收发 → daemon),业务策略仍 ccteam-imd |

**净效果**:`ccteam-imd` 从"自己开 14 个 supervisor task" 退化为"业务面 router + 安全策略 + 高层路由",代码量估算砍 40-50%;**连接 supervision 和消息持久化全到 daemon**,crash-resilience / multi-tenant / budget 全部从 mux 层自然继承。

### 12.7 战略 takeaway

| 维度 | V0.6.7 现状 | V0.8 Epic 后 | V0.9 IM unification 后 |
|---|---|---|---|
| agent 子进程供养 | 3 套(mode 2 spawn / mode 3a tmux / mode 3b 独立 UDS) | **1 套**(MuxBackend) | 同 V0.8 |
| IM 连接供养 | ccteam 主进程 tokio task | 同 V0.6.7(不变)| **同 agent,1 套**(ChannelBackend) |
| crash-resilience | 部分(mode 3 tmux 有,其余无) | 全 agent 有 | **全 endpoint 有**(agent + IM)|
| bot-to-bot 消息 | 内存 mpsc,ccteam 重启丢 | 同 V0.6.7 | **daemon 持久 mpsc,survive ccteam 重启** |
| 跨 vendor agent 接入 | 每 vendor × 每 mode 一份 adapter | mode 1 adapter / vendor | **同 V0.8**(IM 不在此路径)|
| 多 ccteam project 共享 host | 每 project 独立 ccteam-imd + tmux | 共享 mux daemon(支柱 A2)| **共享所有 channel daemon** |
| typed event 统一总线 | 无 | mode 2/3 有 | **agent + IM + future channel 全在一条总线** |

V0.8 是 unification 的**第一跳**(agent supervision 统一);V0.9 是 unification 的**第二跳**(把 IM 视为 channel 的特例,继承 V0.8 全部架构红利)。**两跳合起来,ccteam 的"长跑端点供养"维度从 3 套异构机制收敛到 1 套同构机制 — 这是 V1.0 GA 之前必经的架构清扫**。

V0.8 trait 设计的微调(§12.5)是低成本前瞻动作,**强烈推荐 V0.8 W1 起手就按 `ChannelBackend` + `ChannelSpec` enum 形态设计 trait**,不要等 V0.9 大改。

---

## 十三、Claude Code 与 Codex 消息入站/出站的 rmux 统一架构详解

> 本节是 §二.2 第 1 红利"统一 vendor adapter 形态"的**深度展开** —— 用户提问"引入 rmux 重点就是解决 Claude Code 和 Codex 入站/出站统一,现在入站靠 tmux send-keys、出站靠 hook,引入 rmux 之后有更好的方法吗"的正面回答。
>
> 一句话:**rmux daemon 是 typed event bus 与 typed command bus 的双向中枢;Claude / Codex 在 orchestrator 视角下变成同一个 RPC**。

### 13.1 现状(V0.6.8)— Claude 和 Codex 的 4 条不对称数据通道

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                ccteam orchestrator (V0.6.8 现状)                              │
│                                                                                │
│   ┌──────────────────────────────────────────────────────────────────────┐   │
│   │ progress.jsonl  ◄── 业务事件 SoT(7 类 + chat_session_reset / turn_done)│
│   │ turns.jsonl     ◄── 对话原文(mode 3 ccteam-owned)                       │
│   └──────────────────────────────────────────────────────────────────────┘   │
│       ▲ inbound    ▲ outbound      ▲ inbound       ▲ outbound                │
│       │            │ (state)        │               │                         │
└───────┼────────────┼────────────────┼───────────────┼─────────────────────────┘
        │ (1)        │ (2)            │ (3)           │ (4)
        │            │                │               │
        │ tmux       │ Claude Code    │ direct UDS    │ codex UDS
        │ send-keys  │ hook subprocess│ JSON-RPC      │ JSON-RPC event stream
        │ -l 字符串   │ fork-per-event │ orchestrator  │ + F122 bridge writes
        │ (escape    │ writes         │ → codex UDS   │ progress.jsonl 直接
        │  risk)     │ progress.jsonl │               │
        │            │ 直接           │               │
        ▼            ▲                ▼               ▲
   ┌──────────────────────────────┐  ┌──────────────────────────────┐
   │ tmux session(ccteam-chat-...) │  │ codex app-server 独立进程    │
   │ ├── claude TUI(PTY child)    │  │ ├── 自己 bind 自己的 UDS      │
   │ │   ├── stdin from tmux       │  │ │   socket                   │
   │ │   └── hook 钩子 → fork       │  │ ├── JSON-RPC 协议            │
   │ │       `ccteam internal      │  │ └── ccteam-side 独立          │
   │ │       hook progress-append`│  │     supervisor 守(F112)      │
   │ └── transcript jsonl(Anthropic │  └──────────────────────────────┘
   │     内部 ~/.claude/projects/  │
   │     ,ccteam tail 镜像 →        │
   │     turns.jsonl)               │
   └──────────────────────────────┘
```

**4 条通道、4 种协议**:

| # | 方向 | vendor | 通道 | 协议形态 | 痛点 |
|---|---|---|---|---|---|
| (1) | 入站 | Claude | `tmux send-keys -l --` | shell 命令行字符串 | escape risk;每次新进程;无 typed schema |
| (2) | 出站 | Claude | Claude Code hook(`pre_tool_use` 等)| 子进程 + 写文件 | fork-per-event;只能写到文件 SoT,无 push 给 orchestrator;失败静默 |
| (3) | 入站 | Codex | orchestrator → codex UDS JSON-RPC | typed JSON-RPC | orchestrator 直连,**与 Claude 入站协议完全不同**;每加新 vendor 又一套 |
| (4) | 出站 | Codex | codex UDS event stream + F122 progress bridge | typed JSON-RPC + 文件桥 | bridge 是 ccteam-core 内自定义代码;**与 Claude 出站机制完全不同** |

**根因诊断**:Claude Code 是**人机交互 TUI**(stdin/stdout + 外挂 hook),Codex app-server 是**机机 RPC server**(UDS + JSON-RPC)。两种 vendor 哲学迥异,ccteam V0.6.8 各写一套适配 — adapter 总 LOC ~2200,**两 vendor 之间几乎零代码复用**。

### 13.2 rmux 引入后:daemon 是双向 typed bus 中枢

```
┌────────────────────────────────────────────────────────────────────────────────┐
│              ccteam orchestrator (V0.8+ with rmux)                              │
│                                                                                  │
│   progress.jsonl, turns.jsonl 仍是文件 SoT(下游 写者 = orchestrator,单写者)  │
│                                                                                  │
│   ┌───────────────────────────────────────────────────────────────────────┐    │
│   │ 单一 API:                                                              │    │
│   │   mux.send_to_session(sid, TypedCommand)  ────────── 入站(任何 vendor)│
│   │   mux.subscribe(sid) → Stream<TypedEvent> ────────── 出站(任何 vendor)│
│   └───────────────────────────────────────────────────────────────────────┘    │
│       │ typed RPC over UDS                              ▲ typed event stream    │
└───────┼─────────────────────────────────────────────────┼───────────────────────┘
        │                                                 │
        ▼                                                 │
┌────────────────────────────────────────────────────────────────────────────────┐
│               rmux daemon(sibling process via `ccteam mux daemon`)              │
│                                                                                  │
│   ┌──────────────────────────────────────────────────────────────────────┐     │
│   │  Unified event bus + command router                                   │     │
│   │  • InboundCommand → per-session backend translator                    │     │
│   │  • OutboundEvent ← per-session backend collector                      │     │
│   │  • All sessions, all vendors → one stream, one schema                 │     │
│   └────────────┬──────────────────────────────┬──────────────────────────┘     │
│                │                              │                                 │
│   ┌────────────▼────────────────┐  ┌──────────▼─────────────────────────┐     │
│   │ Session A:Claude            │  │ Session B:Codex                    │     │
│   │ Backend = ClaudeTuiBackend  │  │ Backend = CodexAppServerBackend     │     │
│   │ ┌─────────────────────────┐ │  │ ┌─────────────────────────────────┐ │     │
│   │ │ PTY master              │ │  │ │ PTY master                      │ │     │
│   │ │ (stdin/stdout bytes)    │ │  │ │ (process supervision only)      │ │     │
│   │ └─────────────────────────┘ │  │ └─────────────────────────────────┘ │     │
│   │ ┌─────────────────────────┐ │  │ ┌─────────────────────────────────┐ │     │
│   │ │ HookSidecar(UDS)        │ │  │ │ CodexUdsBridge(daemon-owned UDS │ │     │
│   │ │ 接 Claude hook 子进程    │ │  │ │ + JSON-RPC client to codex)     │ │     │
│   │ │ 的 typed event 投递      │ │  │ │                                  │ │     │
│   │ └─────────────────────────┘ │  │ └─────────────────────────────────┘ │     │
│   │ TypedCommand::KeyboardInput │  │ TypedCommand::JsonRpcCall          │     │
│   │   → PTY stdin               │  │   → forwards to codex UDS          │     │
│   │ TypedEvent ←                │  │ TypedEvent ←                       │     │
│   │   ← HookSidecar 收 hook     │  │   ← CodexUdsBridge 收 JSON-RPC     │     │
│   │   ← PatternMatched(regex)   │  │   ← PatternMatched(regex)          │     │
│   │   ← ProcessExited           │  │   ← ProcessExited                  │     │
│   └─────┬─────────────▲─────────┘  └─────┬──────────────▲────────────────┘     │
│         │             │                  │              │                       │
└─────────┼─────────────┼──────────────────┼──────────────┼───────────────────────┘
          │             │                  │              │
          ▼             │                  ▼              │
   ┌──────────────────────────┐    ┌─────────────────────────────────────┐
   │ claude TUI in PTY child   │    │ codex app-server child process      │
   │                           │    │  binds own UDS for JSON-RPC         │
   │ hooks 钩子 → 子进程       │    │  daemon's CodexUdsBridge connects   │
   │ `ccteam mux hook-emit \   │    │  as JSON-RPC client                 │
   │   --session <sid> \       │    │                                     │
   │   --kind tool_use \       │    │                                     │
   │   --json '{...}'`         │    │                                     │
   │   ↓ short subprocess      │    │                                     │
   │   ↓ UDS to daemon         │    │                                     │
   │   HookSidecar 收 typed    │    │                                     │
   └───────────────────────────┘    └─────────────────────────────────────┘
```

**关键架构转变**:vendor-specific 协议(Claude hook fork-per-event / Codex JSON-RPC over UDS)被**收敛到 daemon 的 backend 适配层**。orchestrator 永远只对接一个 typed API。

### 13.3 入站详细路径对比

**Claude 入站**:

| 阶段 | V0.6.8 现状 | V0.8 with rmux |
|---|---|---|
| orchestrator API | `TmuxSession::send_keys_literal(text)` | `mux.send(sid, TypedCommand::KeyboardInput(text))` |
| 协议形态 | shell argv `tmux send-keys -t <name> -l -- "<text>"` | typed Rust struct over UDS |
| escape 处理 | `send-keys -l` literal flag(仍是 shell 边界) | daemon 直写 PTY master,**零 escape** |
| 失败响应 | shell exit code,解析 stderr 字符串 | typed `Result<(), MuxError>`,enum 枚举 |
| 测试 mock | spawn 假 tmux binary(`CCTEAM_TMUX_BIN`)| `MockBackend` 注入 trait,无进程 |

**Codex 入站**:

| 阶段 | V0.6.8 现状 | V0.8 with rmux |
|---|---|---|
| orchestrator API | `CodexJsonRpcClient::call(method, params)` 直连 codex UDS | `mux.send(sid, TypedCommand::JsonRpcCall { method, params })` |
| 协议形态 | orchestrator 自己 bind Codex UDS,自己处理 JSON-RPC | daemon 持 UDS handle,orchestrator 不直接接触 codex 协议 |
| 多 codex session 并发 | 每个 codex session 独立 UDS,orchestrator 持多 socket | daemon 持所有 socket,orchestrator 走 SessionId 路由 |
| 失败响应 | UDS 断 / JSON-RPC error 各自类型 | 统一 typed `MuxError`,与 Claude 同 enum |

**统一收益**:orchestrator 业务代码不再写"if vendor == Claude then send-keys else if vendor == Codex then JSON-RPC" 这类分支;**`mux.send(sid, cmd)` 一行打通,daemon 内 backend 自动翻译协议**。

### 13.4 出站详细路径对比(关键设计点)

```
V0.6.8 现状(Claude hook 出站)              V0.8 with rmux(Claude hook 出站)
───────────────────────────                ───────────────────────────────────
Claude TUI 触发 pre_tool_use hook           Claude TUI 触发 pre_tool_use hook
  ↓                                          ↓
hook command:                              hook command:
  `ccteam internal hook                      `ccteam mux hook-emit \
   progress-append`                            --session <sid> \
  ↓                                            --kind tool_use \
fork subprocess                                --json '{...}'`
  ↓                                          ↓
reads STDIN(Claude payload)                fork subprocess
  ↓                                          ↓
opens progress.jsonl                       opens UDS to daemon
  ↓                                          ↓
appends event(无 schema check)             daemon HookSidecar 收
  ↓                                          ↓
exit                                       daemon validate + publish to bus
                                             ↓
                                           orchestrator subscribe → writes
                                             progress.jsonl(单写者)
                                             ↓
                                           web UI / 其他 subscriber 同步收到
                                             event(无需 tail 文件)
```

**为什么 V0.8 路径"加一跳"反而更好**:

| 维度 | V0.6.8 hook → 直写 progress.jsonl | V0.8 hook → daemon → orchestrator → progress.jsonl |
|---|---|---|
| schema 验证 | 无 — bad event 静默写入,后续 reader 才崩 | daemon publish 前 typed validate,bad event 立即返错 |
| 实时性 | orchestrator tail progress.jsonl,**轮询延迟** ~50ms | daemon push,**< 1ms** |
| 多 subscriber | 只有 orchestrator 一家;web UI 也得 tail 文件 | event bus 多 subscriber(orchestrator + web + cli peek + future)|
| 跨 vendor 一致 | Claude 走 hook,Codex 走 F122 bridge,**两套** | **同一 typed event 经同一 bus**,vendor 信息在 event metadata |
| 写者收敛 | hook subprocess 是直接 writer,orchestrator 也写 | **orchestrator 唯一 writer**,事件源单一 |
| crash-resilience | orchestrator 死 → 新 hook event 仍写文件,重启后 reread | orchestrator 死 → event 暂存 daemon bus(可选 ring buffer)→ 重连重放 |
| Codex 同构 | Codex 走完全不同路径 | **Codex backend 同样 publish 到 bus,机制一样** |

**Codex 出站(V0.8)**:

```
codex app-server emit JSON-RPC event over its UDS
  ↓
daemon's CodexUdsBridge 收 JSON-RPC frame
  ↓
翻译为 TypedEvent(与 Claude hook event 同 enum)
  ↓
daemon publish to event bus(同一条 bus,与 Claude 共用)
  ↓
orchestrator subscribe → writes progress.jsonl
```

**Claude 和 Codex 在 orchestrator 视角下出站完全无差别** — 因为 orchestrator 只看 typed event,event 的 vendor 是 metadata 字段,不是 protocol 差异。

### 13.5 红线对齐:不解析 pane 输出 仍守住

CLAUDE.md §三"不解析 tmux 终端输出"红线在 V0.8 仍**完全成立** — 而且更稳:

- Claude 出站 typed event 通过 **Claude Code hook subprocess** 投递(Anthropic 官方 typed 通道),**不**通过 PTY 字节流
- daemon 的 vt100 屏幕状态**只用于** screenshot / peek / wait_for_text(同前)
- `PatternMatched{regex_id}` 用预注册 regex 作 typed event(本篇 §11.2 S2),业务代码零 grep — **形式上没有"业务层解析 pane",事实上拿到 amux 类自愈能力**

**Codex 出站没有 pane 解析问题** — 它本来就是 JSON-RPC,daemon 只是协议中转。

### 13.6 LOC + 工程账面收益

V0.6.8 现状 vendor adapter 估算(`crates/ccteam-core/src/execution/`):

| 文件 | 现 LOC | 用途 | V0.8 改造后估 |
|---|---|---|---|
| `claude_tui.rs` | ~450 | mode 3a Claude TUI + tmux send-keys + transcript tail + hook 投递路径 | ~200(send/subscribe 走 trait)|
| `claude_bg.rs` | ~350 | mode 2 Claude bg + jobs.jsonl tail | ~180 |
| `codex_exec.rs` | ~280 | mode 2 Codex exec + send-keys fallback | ~140 |
| `codex_app_server.rs` | ~420 | mode 3b Codex UDS supervisor + JSON-RPC client + F122 bridge | ~150(supervisor + bridge 下沉 daemon)|
| `codex_jsonrpc.rs` | ~200 | Codex JSON-RPC client(orchestrator 直接持有)| ~80(typed schema 定义,具体调用走 daemon)|
| `transcript_tail.rs` | ~180 | tail Anthropic 内部 jsonl 镜像 turns.jsonl | 不变 |
| `turns_mirror.rs` | ~120 | 同上 sibling | 不变 |
| `session_recovery.rs` | ~200 | crash-restart 拼接 jobs.jsonl 反推状态 | ~60(走 mux.exists / mux.subscribe)|

**LOC 总减幅 ~40%**(从 ~2200 砍到 ~1300);更重要的是**两 vendor 之间共享 backend 抽象**,加第三 vendor(opencode / aider / future)只需新写一个 backend translator(~300 LOC),不再每个 mode × vendor 各写一次。

### 13.7 与本篇前文的对应位置

| 议题 | 本篇位置 |
|---|---|
| 整体 vision + 7 红利 | §二.2(第 1 条"统一 vendor adapter"即本节深度展开)|
| `MuxBackend` trait 设计 | §四(本节用的 `mux.send_to_session` / `mux.subscribe` 即 trait 方法)|
| mode 3a Claude 落地 | §三.3 |
| mode 3b Codex 落地 + Option A 推荐(进 mux PTY)| §三.4 |
| typed event 设计支柱 | §11.2 S2(PatternMatched + 预注册 regex 红线方案)|
| Wave 排期 | §六 W2-W3(W2 RmuxBackend 起 Claude mode 3a,W3 mode 2 + Codex)|
| IM channel 扩展(V0.9)| §十二 — daemon event bus 同时承载 agent 出站 + IM 入站,**架构形态完全相同** |

### 13.8 落地次序建议(W2-W4 三 wave)

1. **W2 Claude TUI in rmux(mode 3a only)** — `ClaudeTuiBackend` 落地:`TypedCommand::KeyboardInput` → PTY;Claude hook subprocess 改投递到 daemon HookSidecar UDS;orchestrator subscribe 替代 tail progress.jsonl(progress.jsonl 仍写,但是 orchestrator 写)。Codex 仍走 V0.6.8 路径(unaffected)
2. **W3 mode 2 bg(Claude + Codex)** — `ClaudeBgAdapter` + `CodexExecAdapter` 改 spawn via `MuxBackend::spawn`;jobs.jsonl tail 改 mux subscribe
3. **W4 Codex app-server in rmux(mode 3b)** — `CodexAppServerBackend` 落地:codex 进 mux PTY supervision,daemon's `CodexUdsBridge` 接 codex 自有 UDS,做 JSON-RPC → TypedEvent 翻译;F122 bridge 代码删除(职责下沉 daemon)

**关键里程碑**:W4 结束时,**`crates/ccteam-core/src/execution/` 里 vendor 名字字面量出现频次 → 0**(只在 daemon backend impl 文件出现);ccteam 业务代码彻底 vendor-agnostic。**这是 §十一.5 战略象限中 ccteam 升级 rmux 上游"agent-era abstraction" 的起点**。

---

## 十四、入站 vs 出站的能力边界:为何 outbound 不可能 100% 走 mux

> 本节回应用户尖锐追问:**"主要看出站入站机制,能不能全部用 tmux 或 rmux 的能力 — 之前入站 send-keys 是 tmux 能力,出站 hook 用的是 Claude Code 的能力"**。
>
> **结论先行**:**inbound 100% 可以走 mux 能力(tmux 或 rmux 都行);outbound 在 Claude 上不可能 100% 走 mux 能力 — 这是 bytes 流向决定的根本不对称,不是实现差距**。rmux 在 outbound 上的角色是**统一 BUS**(把所有 source 路由到同一 typed event 流),不是**统一 SOURCE**。Codex 看起来"出站统一"是因为它本身就把出站事件用 typed JSON-RPC 表达,与 mux 协议层重合。

### 14.1 入站机制:本来就是 mux 能力域,没有别的选择

向一个 PTY 内的进程"送入消息"只有一条物理路径:**写 PTY master fd**。
- `tmux send-keys -l -- "<text>"` = 把字节写进 tmux server 持有的 PTY master fd
- `rmux-sdk::pane.send_text("text")` = 把字节写进 rmux daemon 持有的 PTY master fd
- 二者底层是**同一系统调用** `write(pty_master_fd, bytes, len)`,只是上层包装不同

**Claude TUI 和 Codex app-server 在入站上唯一区别**:
- Claude TUI:消费 PTY stdin 字节(键盘事件)— 与 tmux/rmux 自然对接
- Codex app-server:消费自己 bind 的 UDS socket 上的 JSON-RPC frame — 与 PTY stdin **正交**;但仍可让 daemon 持 UDS handle,作为 backend 内的一个 channel

无论哪种 vendor,**入站协议都收敛到 daemon 的一个 typed API**(`mux.send(sid, TypedCommand)`),daemon 的 backend 自动翻译到具体 wire 协议(PTY write / UDS write)。**inbound 完全是 mux 能力域**(§13.3 已展开)。

### 14.2 出站的根本不对称:byte 流方向是子进程主导,不是 mux 主导

入站时**ccteam 是 producer,Claude/Codex 是 consumer** — producer 决定协议,所以入站统一在 mux 一侧成立。

出站时**Claude/Codex 是 producer,ccteam 是 consumer** — **producer 决定有哪些 channel 可用**。mux daemon 在出站方向是**被动 consumer**(只能看到 producer 暴露的字节流):
- PTY stdout/stderr 字节流(被动)
- 子进程退出码 + 信号(被动)
- 子进程 fork 出来的辅助进程(Claude hook subprocess)— **被动**;mux daemon 看到 fork() syscall 但看不到 hook 进程的 stdin payload

**关键**:mux daemon **不持有** Claude 内部状态(当前 tool 调用、persona、plan 决策树)— 这些信息**只有 Claude 进程自己知道**,Claude **主动选择**通过 hook subprocess 把它们暴露出来。这是 Anthropic 的架构决策,mux 无法绕过。

### 14.3 出站事件的 4 个层次 — 各层"谁能观测到"

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Layer 4 — Semantic Event(tool 名字 + 参数 + 结果 / persona / plan 决策树)    │
│ ────────────────────────────────────────────────────────────────────────────│
│  Claude:  ✓ ONLY via Claude Code hooks(typed JSON payload,Anthropic 官方)   │
│  Codex:   ✓ via JSON-RPC events over UDS(typed,Codex 官方)                  │
│  rmux:    ✗ INVISIBLE — TUI bytes 是 lossy 投影,无法可靠还原 tool params     │
│           (业务面读 pane bytes = 违反 CLAUDE.md §三红线)                      │
├──────────────────────────────────────────────────────────────────────────────┤
│ Layer 3 — Conversation Content(assistant message 文本)                       │
│ ────────────────────────────────────────────────────────────────────────────│
│  Claude:  ✓ via Anthropic 内部 transcript jsonl tail(~/.claude/projects/)   │
│           ✗ rmux PTY capture 渲染后是 ANSI + status bar + 高亮混合,不可还原  │
│  Codex:   ✓ via JSON-RPC events                                              │
│  rmux:    ✗ INVISIBLE(同 Layer 4 理由)                                       │
├──────────────────────────────────────────────────────────────────────────────┤
│ Layer 2 — Pattern Event(rate-limit / context-overflow / "waiting for...")    │
│ ────────────────────────────────────────────────────────────────────────────│
│  Claude:  部分 via hook + 部分 via 输出文本(rate-limit error message)        │
│  Codex:   部分 via JSON-RPC + 部分 via 输出文本(冗余覆盖)                    │
│  rmux:    ✓ via PatternMatched(预注册 regex_id,daemon server-side 匹配)     │
│           红线安全:业务面零 grep(§11.2 S2)                                    │
├──────────────────────────────────────────────────────────────────────────────┤
│ Layer 1 — Process Event(started / exited / SIGCHLD / SIGWINCH / idle 30s)    │
│ ────────────────────────────────────────────────────────────────────────────│
│  Claude:  ✗ Claude 不暴露 process 级事件                                      │
│  Codex:   ✗ 同上                                                              │
│  rmux:    ✓ 全覆盖(daemon owns child handle,wait/waitpid 是 race-free)       │
└──────────────────────────────────────────────────────────────────────────────┘
```

**rmux 真正独占的 outbound source 只在 Layer 1 + Layer 2**(process 级事件 + pattern 事件)。**Layer 3-4 是 vendor 自己的事**,mux daemon 只能做 BUS,不能做 SOURCE。

### 14.4 Claude 出站的 4 个 source 详解

ccteam 当前从 Claude 出站事件的 4 个来源:

| Source | 提供的信息 | 谁的能力 | rmux 能否替代? |
|---|---|---|---|
| **(a) Claude Code hooks**(`pre_tool_use` 等)| 工具名 + 参数 + 结果 / persona 切换 / plan_pending | Claude Code 官方 | ✗ **不能** — TUI 渲染丢失了结构化参数;rmux 只能看 ANSI 字节 |
| **(b) Anthropic 内部 transcript jsonl** | assistant message 全文 + tool block | Claude Code 官方(F172 V2 仍依赖)| ✗ **不能** — 同 Layer 3 理由;Anthropic 可能改格式,但比 (a) 替代 easier(只是文本)|
| **(c) rmux PTY 字节流 + 预注册 pattern**(V0.8 新增)| rate-limit / idle / context-overflow 等 surface 级事件 | rmux 能力 | ✓ 部分代替 hook 中的 surface signal |
| **(d) rmux process handle**(V0.8 新增)| exit code / SIGCHLD / OOM | rmux 能力 | ✓ 完全独占(Claude 不暴露)|

**Claude 出站不可能"全 mux 能力"**:**只有 (c) 和 (d) 在 rmux 能力域**;(a) 必须依赖 Claude Code hooks,(b) 必须依赖 Anthropic 内部文件。V0.8 后 ccteam 的 Claude 出站架构是:

```
┌────────────────────────────────────────────────────────────────────────┐
│ rmux daemon event bus(orchestrator 只对接这一个 stream)              │
└──┬─────────────────┬───────────────────────┬───────────────┬──────────┘
   │ (a) hook event  │ (b) transcript event  │ (c) pattern   │ (d) process
   │                 │                       │               │
┌──┴───────┐     ┌───┴──────────┐      ┌─────┴────┐     ┌────┴──────┐
│Claude    │     │transcript    │      │rmux      │     │rmux       │
│Code hook │     │jsonl tail    │      │PatternM  │     │ProcExited │
│subprocess│     │(ccteam-side  │      │atched    │     │/ Idle     │
│→ daemon  │     │ task)→ daemon│      │(daemon-  │     │(daemon-   │
│HookSidecar│    │HookSidecar   │      │ side)    │     │ side)     │
└──────────┘     └──────────────┘      └──────────┘     └───────────┘
  vendor-          vendor-              mux 能力          mux 能力
  specific         specific             (vendor-          (vendor-
  source           source               agnostic)         agnostic)
```

**4 个 source、1 条 bus**。orchestrator 看到的是统一 typed event stream,**但 source 仍然是 vendor-specific**。

### 14.5 Codex 出站为什么"看起来"统一

Codex 的 JSON-RPC over UDS 是**结构化典型** — 它把 Layer 4 (semantic event) 直接用 JSON-RPC 表达,不依赖 PTY 字节流也不依赖 hook subprocess。这让 Codex 出站**天然只有 1 个 source**(UDS):

```
┌────────────────────────────────────────────────────────────────────────┐
│ rmux daemon event bus                                                  │
└──┬───────────────────────────────┬─────────────┬─────────────────────┘
   │ Codex UDS event(承载 Layer 4) │ pattern(c) │ process(d)
   │                               │             │
┌──┴─────────────────────┐    ┌────┴──────┐  ┌───┴──────┐
│daemon's CodexUdsBridge │    │rmux       │  │rmux      │
│接 codex 自有 UDS,      │    │PatternM   │  │ProcExited│
│翻译 JSON-RPC → typed   │    │atched     │  │          │
│event                   │    │           │  │          │
└────────────────────────┘    └───────────┘  └──────────┘
```

Codex 也是 **3 个 source**(UDS + pattern + process),只是 UDS 是 typed-vendor-source(可视为"vendor 选择了 typed channel",**功能上等价于 Anthropic 用 hooks**)。

**Codex 不"更统一",只是 vendor 选了 in-band typed**;Claude 选了 out-of-band typed(hook subprocess)。两种都是 typed,只是路径不同。

### 14.6 创意替代:如果**真的**全 mux 出站(放弃 hook),会损失什么

假设 V0.8 激进设计:**完全不用 Claude Code hook,纯靠 rmux PatternMatched + transcript jsonl tail + process handle**,损失清单:

| 当前 ccteam 用 Claude hook 拿到的信息 | 用 rmux 替代是否可行 | 损失 |
|---|---|---|
| `pre_tool_use` 工具名 + 参数 | 部分可 — TUI 渲染时工具名通常显示,但参数被截断 | **工具参数丢失** — F128 tool_added / advise_vote 等用例失效 |
| `post_tool_use` 工具结果 | 不可 — 结果可能很长被 TUI 滚动出屏 | **工具结果丢失** |
| `chat_session_reset` 时机 | 可 — `/new` / `/compact` / `/clear` 命令注入后能 pattern 检测 | 时机有 ~100ms 延迟 |
| `persona_changed` | 不可 — TUI 不显式渲染 persona 切换 | **persona 切换不可见** |
| `plan_pending` HITL state(V0.6.1 F124)| 不可 — 需 plan 详情 + agent path | **HITL 功能整个失效** |
| `turn_done` 准确时机 | 部分可 — idle pattern 替代,但精度低 | turn 边界模糊 |

**结论**:激进替代会**让 ccteam 退化到 V0.5 之前的能力水平**(无结构化 tool 观测 / 无 HITL / 无 persona);**不可接受**。

### 14.7 创意替代:把 Claude hook 投递的数据"路由到 mux"算不算"全 mux 能力"

这是用户问题的另一可能理解:**hook 仍是 Claude 的能力,但 hook 的 PAYLOAD 改路由到 mux daemon,而非直接写文件 — 这样从架构形态上 outbound 是"mux daemon 内部消化",算不算统一?**

这正是 §13 已经在做的设计 — **算"统一",但要诚实地说**:
- **统一的是路由**(daemon 是唯一 outbound sink)
- **没统一的是 source**(Claude 的能力 vs Codex 的能力 vs rmux 的能力 各自贡献)

用一句话表达:**mux daemon 是 outbound 的 *unified bus*,不是 outbound 的 *unified producer***。

类比:邮局是邮件的统一中枢,不代表所有邮件都是邮局**写的**;邮件是写信人写的,邮局只是收集和分发。outbound 信息源是 vendor(claude/codex),mux daemon 是中央邮局。

### 14.8 asymmetry 的根因 — byte 流方向决定可观测性主权

| 方向 | producer | consumer | "能力归属" |
|---|---|---|---|
| **入站** | ccteam | claude / codex | **producer 主导协议** → ccteam 可全部走 mux 写 PTY 实现统一 |
| **出站** | claude / codex | ccteam | **producer 主导协议** → vendor 各自决定 channel 形态(hook / JSON-RPC / 文件)— ccteam 只能按 vendor 提供的 channel 收 |

**这个不对称是物理的,不是工程上的"缺哪一脚就能补"**。
- 入站统一 = ccteam 决定怎么发 → 完全可控,走 mux 100%
- 出站统一 = 取决于 vendor 选择 → 部分可控,**mux 是 bus**,**source 是 vendor 的事**

如果 Anthropic 未来给 Claude Code 加一个 official `--event-output-fd N`(把 typed event 流到给定 fd),那 rmux 可以"接管"那个 fd → 出站接近 100% 走 mux。**但今天这个 API 不存在**;ccteam 只能用既有的 hook 机制。

### 14.9 与本篇前文的对应位置

| 议题 | 本篇位置 |
|---|---|
| §13.2 daemon 是双向 typed bus 的图 | 本节 14.3 layer 矩阵 + 14.4 4-source 图是其**诚实展开** |
| §13.5 红线对齐 | 本节 14.3 Layer 4 invisible 部分的根据(为何 hook 不可省)|
| 落地次序 §13.8 | 本节 W2 设计要点:**Claude hook 仍要,只是 hook 命令改成 `ccteam mux hook-emit`(写 daemon UDS),不再直写 progress.jsonl** |
| 未来 Anthropic 改 API | 本节 14.8 末尾;若上游加 typed event fd,ccteam 可演进 |

### 14.10 一图收尾:rmux 在 outbound 上的实际角色

```
   ┌─────────────────────────────────────────────────────────────┐
   │   ccteam orchestrator                                        │
   │   ▲ 订阅 1 条 typed event stream                              │
   └───┼─────────────────────────────────────────────────────────┘
       │
   ┌───┴────────────────────────────────────────────────────────┐
   │   rmux daemon = OUTBOUND BUS(统一 1 条 stream 出)           │
   │                                                              │
   │   收集 4 类 source,翻译为同一个 TypedEvent enum:            │
   │   ┌──────────┬─────────────┬───────────┬──────────┐         │
   │   │(a)Claude │(b)Anthropic │(c)rmux    │(d)rmux   │         │
   │   │Code hook │transcript   │Pattern    │Process   │         │
   │   │subprocess│jsonl tail   │Matched    │Exited /  │         │
   │   │→ daemon  │→ daemon     │(daemon-   │Idle      │         │
   │   │UDS       │tail(or kept │ side)     │(daemon-  │         │
   │   │          │ in ccteam)  │           │ side)    │         │
   │   └──────────┴─────────────┴───────────┴──────────┘         │
   │      ▲             ▲           ▲             ▲              │
   └──────┼─────────────┼───────────┼─────────────┼──────────────┘
          │             │           │             │
        Claude       Anthropic    rmux 能力      rmux 能力
        能力          能力                                       
        (vendor)     (vendor)    (mux)         (mux)             
        ───── source 仍 vendor-specific ───── │  ─── mux 独占 ───
```

**rmux 的真实角色**:
- ✓ outbound BUS — 4 类 source 收敛到 1 条 stream
- ✓ outbound SOURCE for Layer 1 (process) + Layer 2 (pattern)
- ✗ outbound SOURCE for Layer 3-4 (conversation / semantic) — **物理上看不到**

**Claude Code hooks 仍要保留**,只是 hook command 从 `ccteam internal hook progress-append`(直写文件)改为 `ccteam mux hook-emit`(投递到 daemon)。**形态上"hook 不消失,但它 feed 到 mux bus,而非平行通道" — 这是用户能问到的统一极限**。

---

## 十五、Vendor TUI 字节流的 mux 捕获能力 — §14 过早结论的修正

> 用户对 §14 的反驳:**"vendor 都是 TUI 形式,暴露的是终端显示,这个 mux 应该可以捕获的"**。
>
> **这个反驳是对的**。§14 把"Layer 3-4 对 mux INVISIBLE"说得过于绝对了。事实上 vt100 capture **看得见**,只是看到的是**有损投影**(lossy projection)而非**无损源**(lossless source)。本节做诚实修正,并据此重新评估 outbound 架构选项。

### 15.1 §14 错在哪里 — "INVISIBLE" 应是 "LOSSY"

§14.3 表格说 Layer 3 (conversation content) / Layer 4 (semantic) **对 rmux INVISIBLE**。准确表述应是:

| 层 | 对 mux 的实际可见性 | §14 表述 | 修正后表述 |
|---|---|---|---|
| Layer 1 process | 全可见(handle owned)| ✓ | ✓ |
| Layer 2 pattern | 全可见(预注册 regex)| ✓ | ✓ |
| Layer 3 conversation | **可读但有损**(TUI 渲染了内容,但混着 ANSI / 高亮 / 状态栏 / 滚动)| ✗ INVISIBLE | **◐ LOSSY**(60-80% 准确,关键场景可用)|
| Layer 4 semantic | **部分可读**(tool 名 / permission prompt 渲染有 distinctive 结构,但参数 / 结果 / plan 树常被截断/换行)| ✗ INVISIBLE | **◐ LOSSY**(tool 名高准确,params/results 低准确)|

**关键洞察**:vt100 capture + grid 解析 + 预注册结构化 pattern 能拿到的信息远超 §14 暗示的"只有 surface event"。

### 15.2 vt100 capture 实际能可靠提取什么(按可靠性分级)

以 Claude TUI 为例(用 vt100 grid 解析,非简单字节 grep):

#### ✓ 高可靠性(daemon-side 直接落 typed event 完全可行)

| 信息 | 提取方法 | 可靠性 |
|---|---|---|
| **tool 名** | 行首 `●\s+(\w+)\(` pattern + 颜色属性(Claude 工具行通常上色)| ~95%(rendering 变化敏感)|
| **tool 是否完成** | 后续行 `⎿` continuation char 出现 | ~90% |
| **permission prompt** | 多行块 `Do you want to allow...` + `[y]es [n]o` 选项 | ~98%(Claude 的 prompt 格式稳定)|
| **rate-limit 提示** | 错误带 `rate limit` / `Anthropic API` 关键字 | ~99% |
| **context overflow 警告** | 红色 + 关键字 | ~99% |
| **token usage / cost 状态栏** | 状态栏数字 + 单位(`127k tokens` `$0.42`)| ~95% |
| **thinking 状态** | spinner 帧 + `Thinking...` 字串 | ~99% |
| **user prompt 提交** | `>` 提示符 + 用户输入回显 | ~95% |
| **session reset** | `/new` / `/compact` / `/clear` 命令注入后清屏 + welcome banner | ~95% |
| **`turn_done` 边界** | output 停滞 + 提示符回归 + spinner 消失 | ~90% |

**这些 10 个 typed event 足够支撑 ccteam 当前 progress.jsonl 7 类业务事件的 80%**。

#### ◐ 中可靠性(daemon 可尝试,业务面接受降级)

| 信息 | 限制 |
|---|---|
| **tool 参数(短)** | 单行参数可读;> 80 char 被 TUI 截断为 `...` |
| **tool 结果(摘要)** | Claude 通常只渲染 `⎿ Read 47 lines` 摘要;具体内容滚动出屏 |
| **assistant message 文本** | 可读,但 markdown 渲染 / 代码块高亮 / 链接颜色化让 plaintext 还原有损 |
| **plan 列表(markdown)** | 渲染为编号列表,**树结构 / 依赖关系 / item 状态码丢失** |

#### ✗ 低可靠性(daemon 拿不到,必须 vendor channel)

| 信息 | 为何 mux 拿不到 |
|---|---|
| **tool 参数全文** | 大参数 Claude 不渲染 full text 到 TUI(用户也不需要看)|
| **tool 结果全文** | 同上;Claude 写 result 到 Anthropic 内部 transcript jsonl,但 TUI 只渲染摘要 |
| **persona JSON 切换** | 通常 TUI 不显示;`/persona` 命令后只有 banner 变化 |
| **HITL plan_pending 的结构化 plan 树** | F124 需要的 plan tree dependencies / status enum,TUI 不渲染这层结构 |
| **MCP tool registration runtime metadata** | F128 tool_added,Claude 内部状态,**不渲染到 TUI** |

**修正后的判断**:Layer 4 大部分**部分可见**,小部分**真不可见**。

### 15.3 红线"不解析 pane 输出"— 字面 vs 精神

CLAUDE.md §三:**"永不主动 kill 长 session" + "不解析 tmux 终端输出"**。后者是关键约束。

**字面解释**:"业务面 / orchestrator 进程的 Rust 代码不允许 `grep pane bytes`"。
- §11.2 S2 已正式提出:daemon-side 在预注册 regex 之上做匹配 → 业务面零 grep,**字面合规**

**精神解释**:"TUI 是 vendor UX 表面,vendor 改 UX 不应该让 ccteam 崩。状态机 SoT 应走 vendor 的官方 typed 通道(hook / JSON-RPC)而非 UX-derived 通道"。
- 重 daemon-side TUI extraction(本节 15.2 的 10 个 typed event)= **精神上轻微违反** — 不是严格"业务面 grep",但 daemon 解析 = **ccteam 整体仍然依赖 Anthropic TUI 渲染**;Anthropic 改 TUI(如改 `●` 为别的、改 `⎿` continuation、改颜色)需 ccteam 同步发新版 daemon

**两个事实削弱"精神违反"的严重性**:
1. **ccteam 早已依赖 Anthropic 内部文件格式**(transcript_tail.rs 读 `~/.claude/projects/.../*.jsonl`)— Anthropic 改 jsonl 格式 ccteam 同样要 patch。即"对 Anthropic UX/internal 依赖" not new
2. **Anthropic 的 TUI 比内部 jsonl 更稳定** — TUI 是 product surface,有向用户公开的稳定预期;内部 jsonl 是 implementation detail,理论上更易变。**Anthropic TUI 一年内变化几次** vs **内部 jsonl 几乎每月微调**

**结论**:重 daemon-side TUI extraction 在红线精神上**略增加 vendor surface 依赖,但量级与现状 transcript jsonl 依赖相当**。可接受,但需明确写入 release notes 治理(本节 15.7)。

### 15.4 出站架构三选项重审

| 选项 | 描述 | Claude 完整度 | Codex 完整度 | 新 vendor 友好度 |
|---|---|---|---|---|
| **A. 纯 mux capture** | 完全不用 hooks / JSON-RPC,daemon 解析 TUI 为唯一 source | 60-80%(失去 tool params 全文 / HITL plan / persona) | 60-80%(同) | **极高** — 任何 TUI agent 立即接入 |
| **B. mux capture 主 + hook/UDS 富化(hybrid)** | daemon 解析 TUI 为通用 base;Claude hook / Codex UDS 作为 enrichment(填补 lossy 部分) | 95%+(hook 补 tool params / plan / persona) | 95%+(UDS 补结构化) | **高** — 无 hook/UDS 的 vendor 走 80% 路径,有 hook/UDS 时升级到 95%+ |
| **C. hook/UDS 主 + mux process 兜底(§13/§14 立场)** | hook/UDS 是 Layer 3-4 唯一 source;mux 只补 Layer 1-2(process / pattern)| 100%(对 Claude/Codex 完全 lossless) | 100% | **低** — 新 vendor 必须实现 hook/UDS 才能接入 |

### 15.5 推荐选 B(hybrid)— 这是用户问题"全 mux 出站"的最佳近似

**Option B 的具体落地形态**:

```
                ┌─────────────────────────────────────┐
                │ ccteam orchestrator                  │
                │ 订阅 1 条 enriched typed event stream│
                └────────────┬────────────────────────┘
                             ▲
        ┌────────────────────┴────────────────────────────┐
        │ rmux daemon — event enrichment merger           │
        │                                                  │
        │   每个 session 持有 EnrichedEvent stream:       │
        │   base event(TUI 解析)+ 可选 enrichment payload│
        │                                                  │
        │   ┌────────────────┐         ┌──────────────┐   │
        │   │ Base source:   │         │ Enrichment:  │   │
        │   │ TUI vt100      │   ◀┐    │ vendor       │   │
        │   │ extraction     │    │    │ specific     │   │
        │   │ (per pattern   │    │    │ channel      │   │
        │   │  registry)     │    │    │              │   │
        │   └────────────────┘    │    │ Claude: hook │   │
        │           ↑             │    │ subprocess   │   │
        │           │             │    │              │   │
        │           │  Merger:    │    │ Codex: UDS   │   │
        │           │  by         │    │ JSON-RPC     │   │
        │           │  timestamp  │    │              │   │
        │           │  + sequence │    │ Future: ?    │   │
        │           │  ID         ├────┤              │   │
        │           └─────────────┘    └──────────────┘   │
        └──────────────────────────────────────────────────┘
                ▲                              ▲
                │                              │
        ┌───────┴────────┐         ┌───────────┴────────────┐
        │ claude TUI in  │         │ Claude Code hook       │
        │ rmux PTY pane  │         │ subprocess → daemon UDS│
        │ stdout/stderr  │         │ (optional enrichment)  │
        └────────────────┘         └────────────────────────┘
```

**关键设计**:
1. **base event 从 TUI 解析,vendor-agnostic** — daemon 持 pattern registry,扫 TUI 字节流出基础 typed event(tool 名 / permission prompt / rate-limit / idle / 等)
2. **enrichment 从 vendor channel 拿,vendor-specific** — Claude hook 携 tool 全参数 + result + plan 树等富信息;daemon 按 sequence ID 把 enrichment 合到对应 base event
3. **没 enrichment 也能跑** — orchestrator 收到的 EnrichedEvent 中 `.enrichment: Option<...>` 字段,业务代码视情况降级;新 vendor 没 hook → enrichment 永远 `None`,但 base event 有
4. **enrichment 失败也能跑** — hook subprocess fork 失败 / UDS 断 → daemon 继续 emit base event(degraded but functional)

**这是用户问题的最强答案**:**输出**架构上**"mux 是主路径"成立**(base event 全走 mux 解析);**hook 退化为 enrichment**(可选,丢失只是降级,不是 ccteam 死);**新 vendor 不需要 hook 就能接入**(只是富信息缺失)。

### 15.6 修正后的 V0.8 W2-W4 wave 计划

§六 / §13.8 原 wave 计划保留主要 milestone,但 W2-W4 内容调整为 Option B 落地:

| Wave | 原计划(§六 / §13.8)| 修正(Option B)|
|---|---|---|
| **W2** | Claude TUI in rmux + hook subprocess 改投递 daemon | + daemon-side TUI extraction registry(10 个 base pattern)+ EnrichedEvent merger 框架 |
| **W3** | mode 2 bg 进 mux | + bg agent TUI extraction(jobs.jsonl 仍是 fallback)|
| **W4** | typed event → progress.jsonl 桥 + Codex 进 mux | + Codex UDS 也走 enrichment 模式(base 从 TUI 解析,enrichment 从 JSON-RPC 富化)|
| **W5+** | attach / cross-platform | 同 |

**新增 wave 子目标**:
- W2 子目标:**注册 10 个 base pattern 到 daemon**(本节 15.2 "高可靠性" 那一列),整理为 `crates/ccteam-core/src/mux/patterns/claude.rs` 静态 const
- W3 子目标:**Codex 同样 10 个 base pattern**(`patterns/codex.rs`)
- W4 子目标:**EnrichedEvent merger 算法稳定**(by timestamp + sequence ID,容忍 enrichment 延迟到 base event ±2s 之内)

### 15.7 fragility 治理 — Anthropic 改 TUI 不让 ccteam 崩

| 风险 | 缓解 |
|---|---|
| Anthropic 改 `●` 为别的字符 / 改颜色 | daemon 内 pattern registry 单文件,patch ship 走 V0.8.x patch;CI 加 Claude Code 多版本 smoke test(`claude --version` 探测,差异版本走对应 pattern set)|
| TUI 渲染宽度敏感(`tput cols` < 80 截断不同)| daemon 在 spawn session 时强制 `pty size = (200, 50)`(与 ccteam-core/src/tmux.rs 现有 `-x 200 -y 50` 一致),pattern 按 200 col 假设 |
| 不同 Anthropic 区域版本(中国/美国)渲染微差 | pattern variant 走 feature flag,daemon 自动探测后 fallback |
| **ccteam release notes** | 每版本 release notes **明列** "Claude TUI X.Y 已验证 / X.Z 未验证";用户升级 Claude Code 前查表 |

**核心治理原则**:**daemon 内 pattern registry 是 ccteam owned 的"vendor TUI compatibility shim",同 transcript_tail.rs 一样需要 vendor-version 跟踪**。运维负担 +1 表面,但换来"vendor-agnostic 出站架构" + "新 vendor 接入只需 patterns 文件" 的长期价值。

### 15.8 与 §14 的关系

§14 立论:**outbound 100% 走 mux 不可能 — Layer 3-4 INVISIBLE,hook 必留**。

§15 修正:**outbound 主路径走 mux 是可行的 — Layer 3-4 是 LOSSY 不是 INVISIBLE;hook 降级为 enrichment**。

| 维度 | §14 立场 | §15 修正 |
|---|---|---|
| Layer 3-4 可见性 | INVISIBLE | LOSSY projection(60-80% 准确)|
| 出站架构 | hooks/UDS 是 SOURCE,mux 是 BUS | **mux 是 base SOURCE,hooks/UDS 是 enrichment SOURCE** |
| 红线立场 | 严守字面,daemon side regex 拉满 | **微调精神**:接受 daemon-side TUI extraction = vendor surface 依赖,与现状 transcript_tail.rs 同量级 |
| 新 vendor 接入 | 必须有 hook/UDS,否则不可用 | 只需写 TUI pattern 文件,base 功能即开 |
| 完整度 | 100%(对 Claude/Codex 完美)| 95%+(base + enrichment 合并;无 enrichment 80%)|
| 推荐 | Option C | **Option B (hybrid)** |

**用户的反驳促成了这个修正** — `§14` 的"INVISIBLE" 是把 capture 能力低估了一档;`§15` 的 "LOSSY projection" 才是诚实表述。**这也是用户问题"出站能不能全走 mux"的最强工程答案**:**主路径走 mux 可以,enrichment 退化为可选 — 这等同于 architectural 上的"mux 是出站第一性 source"**。

### 15.9 红线表的拟定修订(待 V0.8 doc-first 评审确认)

CLAUDE.md §三 "不解析 tmux 终端输出" 行的修订建议:

```
旧:
| 不解析 tmux 终端输出 | — | 守 | 守 — agent 行为住 .claude/agents/<role>.md ... |

新:
| 业务面零 grep pane bytes | — | 守 — daemon-side TUI extraction 允许 | 守 — daemon-side TUI extraction 允许,
                                              业务面只消费 daemon 翻译后的 typed event;
                                              vendor TUI 渲染格式变更视同 daemon backend 协议
                                              变更,需 ccteam release notes 同步(同 transcript
                                              jsonl 依赖治理) |
```

修订后:
- "业务面零 grep" 是字面红线(强制)
- "daemon-side TUI extraction" 是 architectural pattern,vendor surface 依赖明牌
- ccteam release notes 治理把"TUI 兼容"作为一类正式工程负担,与 "transcript jsonl 兼容" 并列

---

## Sources

(本篇)
- `docs/research/rust-tmux-rewrite-feasibility.md` — 前篇调研,本篇推进基础
- `references/rmux/`(本地检出 2026-05-26,v0.3.1)— 一手验证
- `crates/ccteam-core/src/tmux.rs`(472 LOC)— 现状抽象层
- `crates/ccteam-core/src/execution/{claude_bg,claude_tui,codex_exec,codex_app_server}.rs` — 现有 vendor adapter
- CLAUDE.md §三红线表 + §五 PR 纪律

(rmux 上游一手)
- `references/rmux/crates/rmux-sdk/src/lib.rs` — SDK 公共面
- `references/rmux/crates/rmux-sdk/src/bootstrap/startup_unix.rs` line 270-360 — `connect_or_start` 协议(launcher 闭包契约)
- `references/rmux/crates/rmux-client/src/attach.rs` + `src/attach/terminal.rs` — raw-mode + termios 真实 attach driver(publishable 单独 lib)
- `references/rmux/src/main.rs::run_hidden_daemon` — daemon re-exec 模式,ccteam 镜像
- `references/rmux/spec/feature-inventory-v1.yaml` — Windows pass / macOS skipped 平台覆盖
- `references/rmux/README.md` v0.3.1 published 2026-05-25 + "fresh public preview, bugs expected" warning

(上游动向)
- [Claude Code issue #31901](https://github.com/anthropics/claude-code/issues/31901) — Anthropic 评估 Zellij as Agent Teams backend
- [Helvesec/rmux GitHub](https://github.com/helvesec/rmux) — 上游主仓
- [rmux.io](https://rmux.io/) — 文档

(架构对比)
- `docs/tech-design.md` §2.1 / §6.1 — ccteam 5 块架构 + tmux 当前用法
- `docs/versions/v0-6-0/README.md` §五 — "模式 × vendor"双轴 scope 定义
- 原 feasibility §10 — 12 个 agent-first 设计支柱
- 原 feasibility §11.2 amux — pane scraping 启发(本篇 §2.3 红线注解承袭)
