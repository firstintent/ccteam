# v0.9.0 PRD — 多机协同:Machine 一等资源 + Control/Worker 星型联邦 + 能力派活调度

> **状态:DRAFT,doc-first 规划**。本文是**需求收集 + 架构设计**产物,**不是实现**;实现交另一个 dev session,user review 本 PRD(+ scope 冻结后的 dev-plan)后才动代码。本文作者**只收集需求 + 写文档,不开发**。
> **来源**:v0.8.8 ship(独立 session 模型)后,user 提出多机诉求(我手上 N 台机器,每台都登录了 claude+codex,想从一个 IM/web 入口统一驱动、把并行度拉满;高配机跑端到端部署 + 压测,低配机跑需求讨论 + 原型)。
> **代码基线**:dev = **v0.8.9 已 ship**(version 0.8.9:web UI 改造统一 chat shell + 插件市场 + byte-faithful 终端;v0.8.8 独立 session 模型在其下)。本版**架构上正交**(联邦在 gateway/REST 层加,UI 加一个 Machines tab 与 v0.8.9 的统一 chat shell 平级)。
> **架构 SoT**:落地后回填 `docs/tech-design.md`(gateway daemon → Control/Worker 角色拆分 + Machine 一等资源 + placement 调度)。

---

## 〇、一句话

v0.8.8 把 ccteam 做成「**单 daemon 多客户端**」(一台主机一个 daemon,IM + web + MCP 收口,按 sid spawn/resume 本机 session)。v0.9.0 把它扩成「**一个 Control 节点统一入口 + N 个 Worker 节点各自承载 pane**」的**星型联邦**:沿用 v0.8.8 铸 `sid` 的成功打法,把 **machine 铸成一个新的一等资源**(持久 node id + capability tags + spec class + status + endpoint),给 project / session 各加一个 `node` 属性;Control 持 IM/web 入站收口 + placement 调度 + cluster 注册表 + 全局只读聚合视图,**自己不 spawn 任何 pane**;Worker = 今天的 daemon 几乎原样(rmux/tmux PTY + adapter + event pump + 三把本机 AF_UNIX socket + transcript inotify tail),独占自己的 session,对 Control 暴露一个私有 mTLS 控制口(7332),Control 经各 Worker 既有的 `/api/v1` REST/SSE **路由 turn / 历史 / SSE / stop,不发明新 RPC**。能力派活:高配机打 `[deploy,e2e,stress,class=high]` 标签跑端到端部署 + 压测、低配机打 `[design,prototype,class=low]` 跑需求讨论 + 原型,placement 按 tag 过滤 + 真登录信号过滤 + headroom 加权负载均衡 + 每-tag FIFO 背压队列,把并行度拉满而不 thrash。

---

## 一、user 原始诉求 → 设计映射

> 本节是验收基准(对齐 `docs/requirements.md` 痛点映射纪律):每条 user 原话诉求映射到本 PRD 的落地章节。

| # | user 诉求(原话/语义) | 设计落地 | 章节 |
|---|---|---|---|
| **D1** | 「我手上有 N 台机器,想都用上」 | machine = 一等资源(持久 node id + 注册表),星型联邦 N 个 Worker | §二 核心模型 / §三 架构决策 |
| **D2** | 「每台机器都登录了 claude + codex」 | capability_snapshot 补**真实 claude login 探针**(堵 logged-out 仍 `--version` exit-0 陷阱)+ codex 既有 login status + binary_version + models/context-window | §二 / §五 placement(真登录过滤) |
| **D3** | 「从一个 IM / web 入口统一驱动」 | Control 单例持 Telegram long-poll(单消费者天然只能一个)+ web/SSE + inbound mpsc,Worker 绝不起 IM listener | §三 架构决策(Control 职责) |
| **D4** | 「多台机器协同」 | 跨机 turn / 历史 / SSE / stop 经 Control 代理到 owning Worker;cto fan-out 给每个 child 选不同机器 | §六 状态 SoT / §八 并行度 |
| **D5** | 「高配机做端到端部署 + 压测」 | 高配机 `--tags deploy,e2e,stress --class high`;require=[deploy,stress] 的 session 落高配机 | §五 placement(用户原例机械落地) |
| **D6** | 「低配机做需求讨论 + 原型」 | 低配机 `--tags design,prototype --class low`;无重 require / class=low 的 session 落低配机 | §五 placement |
| **D7** | 「把并行度拉满」 | 三轴最大化:真硬件并行(N 机 N pane)+ fan-out 调度(机群级 session_*)+ 热路径零序列化(节点前缀 sid 本机零 lookup)+ 背压防 thrash + fleet cost-cap 防静默 N× spend | §八 并行度故事 |

**痛点对照**(`docs/requirements.md`):D4/D7 服务痛点 11/12(大项目跨机并行加速)+ 痛点 15(单人放大成管上百 agent)——并行天花板从「一台机容量」升成 **Σ(per-node session 容量)**。

---

## 二、核心模型:Machine 一等资源(本版唯一新增核心概念)

> **纪律(`MEMORY.md` core-concepts = global nouns)**:核心概念 ≤ 3 个、必须是全局领域名词。v0.9.0 **只加一个**:`machine`(host 维度的领域名词,survey 反复指出今天「缺失的 host 维度」)。project / session **不新增概念**,各只多一个 `node` 属性(同 v0.8.8 给同组 struct 加 `permission_mode` / `secret`)。严守「≤3 核心概念 + feature=facet」。

### 2.1 持久身份(复刻 v0.8.8 铸 sid 的打法)

- **node id** = `n-<short-hostname>-<6hex>`,首次 `ccteam start` 铸一次,存新文件 **`~/.ccteam/node.json`**(per-host、扛重启、不复用、host-scoped)。
- Worker 内部 sid 仍裸 `s<N>`(自己的 `next_session` 单调计数器自铸,见 `gateway.rs` 自铸点,**不**改成由 Control 分配);跨机 wire 上 sid = **`n-<node>:s<N>`**(节点前缀)。ownership 编码进 id(`prefix == node_id` 即 owner)⇒ 本机 turn 字节级同今天**零 lookup**,跨机才解析前缀。

### 2.2 资源 schema(`MachineEntry`,纯数据 primitive,无 team 字面量)

```
MachineEntry {
  node_id,                 // n-<short-hostname>-<6hex>
  hostname,
  endpoint,                // https://<lan-ip>:7332
  spec_class,              // high | low
  tags: [String],          // deploy,e2e,stress / design,prototype …
  status,                  // live | draining | down(按 heartbeat-TTL)
  last_seen,
  capability_snapshot,     // 见 2.3
  cert_fingerprint,        // 见 §七 mTLS
}
```

Control 存 **`~/.ccteam/machines.yaml`**(`config.yaml` 的 sibling)。

### 2.3 capability_snapshot 富化(填今天的空 providers + 补缺失维度)

每 vendor:

```
{
  available,        // 已有的 --version exit-0
  login_ok,         // 【新增】真实 claude whoami/auth 探针
                    //   —— 今天只探 codex(codex --version && codex login status),
                    //      logged-out 的 claude 仍 --version exit-0,然后 spawn 失败
  binary_version,   // codex 有 ≥0.131 gate;claude 今天无,本版补
  models + context_windows,  // 填今天 reserved-empty 的 providers,把 [1m] 前置广告
                    //   而不是 session 跑起来才懒发现
  budget_headroom,  // 读本机 cost-budget.json 剩余/cap
}
```

node 级:`{ ncores, mem_gb, has_gpu(nvidia-smi best-effort), max_parallel(默认=ncores,可 override), live_session_count, load_avg }`。

> capability 从「**per-daemon-lifetime 冻结快照**」升成「**machine-keyed、TTL'd、心跳可刷新**」信号 —— late `claude login` **不需 Control 重启**即可传播。

### 2.4 一等资源 API + CLI

- **REST**:`GET/POST /api/v1/machines`、`GET /api/v1/machines/{node_id}`、`POST /api/v1/machines/{node_id}/heartbeat`、`DELETE /api/v1/machines/{node_id}`(= evict)。
- **CLI**:`ccteam node {join,ls,show,rm}`(对齐 `project` / `session` 子命令分组)+ `ccteam cluster {init,invite,ls,rm,rotate}`(operator 面:CA 自签初始化 / mint enroll token / 列 / 逐出 / 轮转机器凭证)。
- **属性增补**:`session` 与 `project` 各加一个 `node` 属性(见 §六 状态模型)。

---

## 三、架构决策:星型联邦,不是共享 daemon、不是 remote ProcessBackend

> 今天每台主机已各跑一个**单 daemon**(per-host pidfile 互斥,「Single per-host daemon」不变量)。v0.9.0 **保留这个 1 daemon/host 不变量**,只加一个角色位。

### 3.1 角色位

- `ccteam start --role control` —— 单例,用户指向的那台。
- `ccteam start --role worker --control https://<ctrl>:7332 --enroll <token>` —— 加入集群。
- `ccteam start --role both`(**默认**)—— 单机退化,折叠成一个进程、loopback、无 mTLS、省 node 前缀 —— **与今天 daemon 字节级一致,1–2 机用户零行为变化**。

### 3.2 为什么必须星型 + 执行钉本地(已读码核验三条不可迁移的硬绑定)

| # | 硬绑定 | 代码事实 |
|---|---|---|
| (1) | `GatewaySession { adapter: Arc<dyn HarnessAdapter>, thread: ThreadHandle }` 持指向**本机 pane** 的活句柄 | `SavedGatewaySession` 只持久化标量 + `thread.identity`;重启走 `resume_restored_sessions` → `resume_thread(&snapshot.thread.identity)` 重连**本机** pane(`gateway.rs`)。`Arc<dyn HarnessAdapter>` + `ThreadHandle` **不可序列化** |
| (2) | Anthropic transcript 在**本机** `~/.claude/projects/` | tail 靠**本机 inotify**(`claude_tui.rs`) |
| (3) | hook 回连 + in-pane MCP forwarder 走**本机** AF_UNIX `~/.ccteam/run/{mcp,hook}.sock` | UDS **不跨主机** |

⇒ **turn 必在 owning node 跑**,只 **DATA + CONTROL** 过 mTLS。

> **关于 rmux**:`RmuxEndpoint` 是 `#[non_exhaustive]`(见 `rmux_backend.rs`),doc 明写 TCP「anticipated… must be addable」—— 所以 rmux 本身**不是永久障碍**,但本版**不**选 remote ProcessBackend 路线(它要把 `BoxStream<ThreadEvent>` 流式跨机 + transcript inotify 跨机 + hook 跨机重 plumb,把最硬的 5 条单机绑定全塞进一个 trait)。**联邦在更上一层(gateway/REST)做,复用每台已跑通的本机执行栈不动。**

### 3.3 Control 职责

IM gateway(Telegram long-poll 单消费者**必须**在此,Worker 绝不起 IM listener)、web/axum + SSE on `0.0.0.0:7331`、inbound mpsc 消费、pending/HITL 注册表、**新增** Scheduler + ClusterRegistry。

- Control **不持任何 `Arc<dyn HarnessAdapter>`、不 spawn pane、ZERO pane 计算** —— 纯 I/O 路由 + 聚合读,所以**一台弱机也能领一队强机**。
- Control 的 gateway session-ops(submit/stop/resolve)在 `node != self` 时走新的 **`RemoteGatewayClient`**(reqwest + rustls client,打到 Worker 的 `/api/v1`,带 Worker 签发的 client cert),而非本地 adapter。

### 3.4 Worker 职责

= 今天 daemon **MINUS IM listener**,保留 gateway + 真 sessions map + event pump(仍是唯一 `turns.jsonl` writer)+ rmux 自托管 + 三把本机 socket + transcript tail + capabilities probe + cost ledger;对外只暴露一个**私有 mTLS 控制口 7332**(只 Control 调),复用既有 axum `/api/v1` router。Worker 内部对 sid 仍 **node-naive**(只认本机 `s<N>`),前缀是 Control/wire 边界的事。

### 3.5 节点间 wire(复用现成 REST,不造新 RPC genus)

- **各 Worker 已 ship 的 `/api/v1` REST/SSE**:`POST /projects/{slug}/sessions`、`POST /sessions/{sid}/turn`、`GET /sessions/{sid}/events`(SSE)、`POST /sessions/{sid}/stop`、`GET /sessions/{sid}`、`GET /capabilities`。
- **Control 新增**:`/api/v1/machines` + `/api/v1/cluster` facet(enroll / heartbeat / list)。

### 3.6 单点诚实

Control 是 IM/web 入口 + placement 的**单点(无 HA,本版 defer)**。Control 宕 = 入口宕,**但每个 Worker 的 live session 继续跑**(永不被 kill)且经自己 standalone `/api/v1` + web 仍可驱动;Control 重启从 `machines.yaml` + 下一轮 heartbeat 重建全局视图(**Worker 是 SoT,Control 可重建**)。文档写明 **degraded-not-dead**。

### 3.7 架构示意

```
                 ┌─────────────────────────────────────────────────────────┐
 用户(手机/浏览器)│                    CONTROL 节点(单例)                    │
   │    │        │  ccteam start --role control   (ZERO pane 计算)          │
   │    │        │                                                          │
   │    └─https─►│  web/axum + SSE (0.0.0.0:7331, web-token)                │
   │ Telegram    │  IM gateway + inbound mpsc                               │
   └──long-poll─►│  (getUpdates 单消费者,只在 Control)                     │
   (单消费者)    │  Scheduler (ccteam-sched: place_session + per-tag FIFO)  │
                 │  ClusterRegistry + sid→node 路由表 + fleet cost-cap      │
                 │  自签 CA (私钥 0600)                                     │
                 │  RemoteGatewayClient (reqwest+rustls, mTLS client cert)  │
                 └───────────┬──────────────────────────┬──────────────────┘
            mTLS 7332(双向证书)│                          │ mTLS 7332
            DATA+CONTROL 过线  ▼                          ▼
      ┌──────────────────────────────┐   ┌──────────────────────────────┐
      │ WORKER 高配机 n-gpu-box        │   │ WORKER 低配机 n-laptop         │
      │ = 今天 daemon MINUS IM listener│   │ tags=[design,prototype]/low   │
      │ tags=[deploy,e2e,stress]/high │   │                               │
      │  /api/v1 router(复用)         │   │  gateway+sessions+event pump  │
      │  gateway+sessions+event pump  │   │  rmux 自托管 + PTY pane        │
      │  (唯一 turns.jsonl writer)    │   │  本机 mcp/hook.sock(不跨机)   │
      │  rmux 自托管 + PTY pane        │   │  transcript inotify tail      │
      │  本机 mcp/hook.sock(不跨机)   │   └──────────────────────────────┘
      │  transcript inotify tail      │   每 Worker → Control:
      │  ~/.ccteam/node.json(持久 id)│     POST /machines/{id}/heartbeat
      │  ~/.claude + ~/.codex(本机   │     (capability+headroom+owned sids,
      │   登录,spend-capable)        │      TTL 刷新)
      └──────────────────────────────┘
   in-pane→daemon 回路(MCP forwarder / HITL hook / per-session secret)
   留**本机**,永不跨网;过网的只有 daemon↔daemon 控制面(mTLS)。
```

---

## 四、能力派活:placement policy(本版唯一真新算法)

Placement 是 Control 上对 capability 注册表的**纯函数** `place_session(req) -> Placement{Node | Queued{tag,pos} | Rejected{reason}}`(放在新 leaf crate **`ccteam-sched`**,无 I/O、无 tick、可单测)。其余全是 struct 字段增补 + 复用 `/api/v1` 当 wire。

### 4.1 三个入口同一函数

- (a) IM `/new <project> [@<role>] [--on <tag|node>]`
- (b) web `POST /sessions` 带可选 placement hint
- (c) cto 的 `session_spawn{placement?}`

—— 全部 funnel 进 `place_session`。

### 4.2 输入

1. **project↔node 主轴绑定**:`ProjectEntry` 加 `node: NodeId`(项目「住」在 init 它的那台 Worker;`config.yaml` 今天存绝对本机路径 + 无 host 字段是事实);默认 session **继承 project 的 node**。
2. **placement hint**:`{node}` 显式 pin / `{require:[tags]}` 声明式(如 `require=[deploy,stress,claude:1m]`)/ `{class: high|low}`。
3. **活态 capability + headroom**(心跳来的)。

### 4.3 匹配 + 负载均衡

filter 到满足 `{require tags}` ∩ `{请求 vendor available 且 login_ok 且 version-ok}` ∩ `{repo 在该 node}` ∩ `{未触 cost-cap、未满 max_parallel}` 的 node;幸存者里按

```
headroom_score = w1·(max_parallel − live)/max_parallel
               + w2·(1 − load_avg/ncores)
               + w3·budget_fraction
               − penalty(vendor 未登录)
```

取最大(贪心 least-loaded,折入算力 + 预算两个维度,避免往饱和/近 cap 的机堆)。**fan-out 时反复取当前最大 headroom ⇒ K 个独立 session 自然铺到 K 台机而非堆一台。**

### 4.4 用户原例机械落地

- 高配机 `ccteam node ... --tags deploy,e2e,stress --class high`,低配机 `--tags design,prototype --class low`。
- 「端到端部署 + 压测」session `require=[deploy,stress]` → 高配机;「需求讨论 + 原型」无重 require(或 `class=low`)→ 低配机。
- 两者**同时**跑、各扬所长,从**同一个 Telegram chat** 驱动。

### 4.5 背压 / 不 thrash

- 每-tag FIFO `pending_placements`(bounded cap=64,沿用 `INBOUND_BUF` 精神;high/low 各自一条队,**压测 backlog 绝不 head-of-line-block 交互讨论/原型 turn**)。
- 全部 tag-eligible node 满载 → **不** spawn-and-thrash(那会让每个 session 都变慢 = 拉满效率的反面),而是返回 **202 queued + 位次**;队空位由**心跳到达事件驱动** admit(**NOT tick**,守「daemon 不 tick」红线);队满 → **503 capacity exhausted** + 「加机器或抬 `--max-parallel`」诚实提示。
- cto role doc 加一句:把「高配机全忙,部署排队 #2」**回报用户**而非挂起。

### 4.6 anti-overparallelize 护栏

`place_session` **严格只决定 WHERE**;WHAT / HOW-MANY 归 cto/用户 —— 调度器**绝不**注入额外并行(小活强行 fan-out 的 token 浪费仍是 cto 判断)。明确**不**长 tick loop / DAG executor(否则撞 deferred `ccteam-flow`)。placement **sticky**:一个 sid 生命周期内**不迁移**(执行绑定不可变),同 project 的**新** session 可落别处。

---

## 五、状态 SoT 与对账(state 可中心化,execution 钉 owning node)

> 沿 survey 核验的**清晰分裂线**:state(文件/JSONL/标量)可中心化,execution(thread/adapter/pane/socket/env)钉产生它的 owning node。两层:per-session SoT 留在写它的 Worker,Control 只持**派生的、最终一致的 INDEX —— 永不当第二写者**。

### 5.1 sid 命名空间(修最尖锐的碰撞)

今天两台各自 `s1,s2,…` 独立递增(`gateway.rs` 自铸 + per-host `gateway-state.json` 持久化)→ 一旦交换 sid 必撞。修法(最小、survey 确认是局部改动):

- wire/display 边界给 sid 加节点前缀 **`n-<node>:s<N>`**,本地 `s<N>` 仍是 pane/turns/marker 键(`claude_tui.rs` pane 名、`turns_mirror.rs` 路径、`CCTEAM_CHAT_SID` **零改**)。
- ownership 编码进 id(`prefix == node_id` 即 owner)⇒ Control 的路由表是**便利/健康索引非权威**,本机 turn 免查表。
- **Worker 保留自己的 `next_session` 自铸**(**不**改成 Control 全局分配器)—— 避免 burst fan-out 时每次建会话都 round-trip Control mint id 这个序列化点(parallelism lens 的关键修正)。
- `/use`、`GET /sessions/{id}`、SSE filter 键、`session_dispatch` 全接受前缀形;裸 `s<N>` 在 Control 解析为 entry/self node(单机 back-compat)。

### 5.2 append-only JSONL 留 owning Worker

`progress/<slug>.jsonl`、`<project>/.ccteam/chat/<sid>/turns.jsonl`、Anthropic transcript —— 只由 owning Worker 的 event pump 写(`gateway.rs` 仍是唯一 live turns writer;`progress_bridge` 仍是 schema 唯一权威)。**Control 不镜像。**

- **全局「列所有机器所有 session」视图** = Control fan-out:并发 `GET` 各 Worker `/api/v1/sessions` 再 merge(观察 100 session 跨 10 机 = 10 个并发 cheap 调用,**非串行 walk**)。
- **单 session 历史** = Control **透明代理** `GET` owning-Worker `/sessions/{sid}`(Worker tail 自己本地 `turns.jsonl`,同今天)。
- **SSE** = Control 代理 owning-Worker `/sessions/{sid}/events` 流再 re-emit(`GatewayEvent.sid` filter 契约不变,只换成网络来源)。

### 5.3 路由表 + reply

Control 持 `sid→node` 权威映射(`MachineEntry` + session 记录的 `node` 字段);inbound IM `ChatKey→sid` 仍是 Control 上**纯内存函数查找**,解析出 sid 后按前缀 forward turn 到 owning Worker(`POST /sessions/{sid}/turn`)而非本地 adapter。`reply_to`(今天是 Worker `GatewaySession` 里的 `Arc<Mutex<ChatKey>>`,`gateway.rs`)随 inbound turn 带给 Worker;Worker event pump 产 `GatewayEvent` 经 per-session SSE 流回 Control;Control 的 outbound 消费者按 channel + chat_id 投 Telegram/web(同今天)。

> ⚠ **`RemoteGatewayClient` 必须忠实复现** `reply_to`-retarget + `after_turn_submitted` watchdog + sid-filtered SSE relay —— 否则**跨机 only** 的丢/重发答复、悬挂 600s HITL approval 会藏这里;用 fake Worker(`CCTEAM_*_BIN` 确定性)+ **2-process loopback mTLS smoke** 在 CI 守。

### 5.4 reconcile

- Worker 重启**独立 reattach 自己** 的 pane(`resume_restored_sessions` **100% 本地、零改** —— 这正是联邦胜 remote backend 之处),再于下一轮 heartbeat 向 Control 重报 session list;Control 按 `node_id` reconcile。
- Control 重启从 `machines.yaml` + 重新 fan-out 重建全局视图(**Worker 是 SoT**)。
- **无分布式事务、无共识**:sid single-writer-per-node by construction,**split-brain ownership 结构上不可能**(sid 由唯一一台 mint,last-writer = 真 owner)。drift 窗口 = 今天 `tracked_chat_sessions` 已接受的 sub-second glance-level drift,现按 node。

### 5.5 fleet cost-cap 聚合强制(补 survey 警告)

已核验 `cost-budget.json` per-host、`aggregated_cost_cap_24h` 只 sum 本机 claude+codex。fan-out N 机**静默 N× spend cap** = 严重 throughput 悬崖(全队同时触顶 auto-disable)。本版 Control **聚合各 Worker cost ledger 成 fleet budget 视图并强制一个 fleet cap**(不止 surface Σ-spend);`headroom_score` 的 `budget_fraction` 既做软避让、Control 做硬 fleet 上限。

---

## 六、并行度故事(产品主线)

并行度拉满是产品主线,从「单 daemon 多客户端」扩成「多 Worker 一视图」,沿现有强项**三轴最大化**:

1. **真硬件并行**:N 个 Worker 在 N 台独立机的 CPU/PTY 上**同时**跑 session;Control ZERO pane 计算(纯 I/O 路由)永不成 compute 瓶颈。「拉满效率」= 高配机狂跑端到端部署+压测,**同时**低配机跑需求讨论+原型,全在一个 IM/web pane 下。不同 Worker = 不同 event pump、不同 rmux server ⇒ 一个 90 分钟压测 run **永不 stall** 另一台的快速原型 turn(无 head-of-line blocking)。
2. **fan-out 调度**:cto-dispatch 的 `session_*` 家族(已是产品内并行原语)扩成**机群级** —— `session_spawn{placement}` 按 tag 给每个 child 选**不同**机器,「并行 build N 服务」把每个落到 least-loaded 的 capable node;`session_collect` 经 Control fan-out 归集。并行天花板从「一台机容量」升成 **Σ(per-node session 容量)**。
3. **热路径零序列化**:节点前缀 sid ⇒ 本机 turn 字节级同今天(零 overhead、零 lookup),跨机 turn 是一次 mTLS-authed 代理 POST,**无 lock/lease/共识 round-trip per turn** ⇒ 加机器近线性加吞吐,直到 Control 轻量 heartbeat/lookup 负载才显现。Worker 保留自铸 sid(不 round-trip Control mint)进一步保 burst fan-out 不被序列化。读侧聚合也并行(并发 fan-out `/sessions` 查询)。
4. **不退化成 thrash**:`headroom_score` 折入 live-session 数 + OS `load_avg` + 预算余量,饱和/近 cap 就移到下一台;全满则 per-tag FIFO **排队而非超订**;fleet cost-cap 聚合让 fan-out N 机**不**静默 N× spend 而无人察觉。anti-overparallelize:cto 决定 spawn 几个、调度器只决定派哪。

> **净**:效率 = (job→机器能力匹配的 placement)×(不被单机封顶的并发)×(每 turn 零协调 overhead)×(背压防 thrash)。UI 把跨机 fan-out 渲染成**可见的并发**(per-node session 列 + 实时进度),用户一眼看见 N 台机在并行干活。

---

## 七、跨机安全(诚实章节:承诺边界 + defer 项)

> 这是最难的部分,**逐条诚实声明承诺边界与 defer**,绝不把 single-OS-uid 信任模型「洗」过网络。代码自己已写明(已核验):`session_secret` + `verify_session_caller`(`main.rs`「only **RAISES THE BAR** …同 uid 可读 `/proc/environ` ⇒ 非硬边界」)—— 这套论证**本就只在单机成立,过网即死**(跨机「同 uid」语义消失,每台 Worker 是独立安全域、独立持 spend-capable 的 `~/.claude` + `~/.codex`、skip-mode session 一个 POST = 任意 shell)。

### 7.1 本版交付(buildable now,deps 基本在树)

**TIER 1 — 传输机密性(硬 blocker)**:今天唯一 TCP 面 web `0.0.0.0:7331` 是**明文 http**(scheme 硬编码 `"http"`,`main.rs`;`axum::serve` 裸 `TcpListener`、无 `bind_rustls`,已核验)。Control↔Worker **必须 TLS**。已核验 rustls **仅 client-side 在树**(reqwest rustls-tls + tungstenite webpki-roots),**server-side rustls + cert 签发/校验是真 net-new** —— 诚实记账:加 server `ServerConfig` + client-cert verifier + 一个 in-process CA 需要新 dep **`rcgen`**(签发库),**这是本版唯一真新依赖**,**不**轻描淡写成「纯 config」(subtle/getrandom/sha2 复用现成)。

**TIER 2 — 机器身份 = mutual TLS(不是裸 bearer)**:Control 首次 `--role control` **自签一个 CA**(私钥 `0600` 存 `~/.ccteam`);enroll 时给每个 Worker 签一张 client cert(`CN=node_id`),Control 自己持 CA-rooted server cert。Worker 控制口 7332 要求**双向 TLS**:Worker 出示 client cert、Control 出示 enroll 时按指纹 pin 的 server cert。未签名 peer 在 **TLS 层被拒、handler 之前就挡掉** —— 把 fail-open ACL 放大问题收在传输层。机器身份**密码学可验**,替死掉的「同 uid」假设。
> 选 mTLS 而非 per-node bearer:bearer 可被 exfil 后 replay;cert 是 proof-of-key-possession,且 **Worker 持 client cert ≠ CA,无法给 sibling 伪造 Control 信任的 server cert ⇒ Worker 间无法互相冒充 Control。**

**TIER 3 — in-pane→daemon 刻意留本地**:in-pane MCP forwarder、HITL PermissionRequest hook、interaction/ask 全连**本机** `mcp.sock`(路径自算同一 `$HOME`)+ 读**本机** pane env(`CCTEAM_CHAT_SECRET`/`SID`)。因 session 钉 owning node,这个回路**永不跨网** —— Worker 上的 agent 找 Worker 的 daemon,和单机完全一样。per-session secret 在 node 内仍是原样 best-effort(同今天,**不升不降**)。**过网的只有 daemon↔daemon 控制面(Tier 1+2)** ⇒ survey 的「mcp.sock 不能上网」是**被绕开而非违反**。

### 7.2 跨机特权边界(真边界)

Worker 授权一个 incoming 特权 op(`session_*`/turn/stop/HITL)的依据 = 「它来自 **Control 的 mTLS 链路**」+「**Worker 之间从不互相通信**」。这是 **REAL boundary**,层叠在不变且仍 best-effort 的 node 内 secret 之上。

> cto 跨机 dispatch(`session_spawn --on gpu-box`)= 一个 federation-authed REST 调到目标 Worker,子 session 的 per-session secret 在**目标 Worker 本地** mint 注入本地 pane env,**绝不过线** —— 保住(承认是 best-effort 的)gate 语义 per-node。

### 7.3 机器凭证 ≠ 人的 web-token

web-token 是单一共享明文 secret、被 `ccteam status` 明文打印 + 自动复制剪贴板(`token.rs` / `main.rs`)—— **绝不**复用它当节点间凭证(那是把单 secret 爆炸半径乘 N)。机器凭证(cert)是**独立类、永不打印、可 `ccteam cluster rotate <node>` 轮转**。

### 7.4 fail-closed 前提(硬 gate,不是 nice-to-have)

已核验的单机遗债在「一个 Control 当全队入口」下被**放大**,**必须先翻 fail-closed 才发联邦**:

- `acl.rs` 空 allowlist = open(已核验 `acl.rs:55`「Open: every sender allowed」;telegram/discord/web 全 fail-open)→ **翻 fail-closed**。
- Slack `verify_slack_signature_stub` 永远 false(`three_layer_sec.rs`,TODO V0.7)→ **补真 HMAC 或明确不支持 Slack inbound**。
- `~/.ccteam/run` 父目录无显式 `0700`(靠 umask)→ **收紧 `0700`**。

### 7.5 明确 DEFER(说清不假装)

1. **per-agent OS-user/sandbox 隔离**(单机本就 defer)。
2. **Control HA/failover**(单 Control = 单点,本版接受、文档写明 **degraded-not-dead**)。
3. **跨机 HITL 强化**:approve/deny 经 Control mTLS 代理到 owning node 的 pending 注册表,600s 阻塞语义保留但 round-trip 走 TLS —— 这是**最易碎的件**,若硬化排不进 scope 则 **DEFER 跨机 HITL**(hitl session 只从自己 node 的 UI 驱动),**宁可缺功能不发会挂 turn 10min/丢 approval 的脆代理**。
4. **自动跨机 git clone repo**(本版项目 pin 在 init 它的 node,缺 repo 的 placement **fail-fast 报错,不静默 cwd 失败**)。
5. **intra-cluster least-privilege / 多租户身份**(本版**单一信任域**,不解决)。

### 7.6 单一信任域声明(写进 PRD + usage.md,不埋)

enroll 一台 Worker = 「**我信任这台机如同我的 Control 机**」。整队属同一个人(产品即「我手上 N 台机器」)。本版**不**声称 per-agent / 多租户隔离,也**不**声称保护某 Worker 免于被一个已沦陷的 peer 攻击 —— 但**星型(Worker 间不通信)已把横向爆炸半径压到比 mesh 小**。

### 7.7 硬 ship gate(诚实退路)

> 若 rustls-server + cert-pinning 这版**确实**排不进 scope,正确动作是 **DEFER 跨机特权控制**,只发**单机 + 只读聚合视图**(Phase A,见 §九),并文档「联邦需你自备私网(wireguard/tailscale)」当 stopgap —— **绝不**在明文上发特权跨机控制面。

---

## 八、红线逐条对照(`CLAUDE.md` §三 = 唯一权威,本节逐条说明怎么守)

| 红线 | 怎么守 |
|---|---|
| **No prompt injection**(role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent` 自读,roleless 省 `--agent`;不向 pane/app-server 注入 system prompt)| **完全不动**:spawn 仍在 owning Worker 经 vendor 原生 `claude [--agent <role>]` / roleless 裸 claude;Control 只转发**已成形的** `TurnInput::UserText`(同 in-process 路径用的同一类型),mTLS 不碰 prompt;`node` 属性走 `SpawnCtx`/`ThreadHandle` 同款 out-of-band metadata 通道(类比 v0.8.8 已加的 `permission_mode`/`secret`),**纯标识,绝不是 system prompt**;`/compact /new /clear` 仍透传。|
| **session = 独立一等实体 + 持久 sid**(`s<N>` 单调、扛重启、不复用;同 role 多 session;pane/turns/marker 按 sid)| **强化**:sid 仍是身份,Worker 保留自己的 `next_session` 自铸(`gateway.rs` 不改),pane=`ccteam-chat-<slug>-<sid>`/turns/marker 按 worker-local sid 同今天;wire 上加节点前缀 `n-<node>:s<N>` 使全机群唯一(节点段 by construction 正交)。`node` 只是 session 多一个属性(同 v0.8.8 给同组 struct 加 `permission_mode`/`secret`),**不新增核心概念**。同 role 多 session、去 dedup 全保留。|
| **`progress.jsonl` = state SoT**(`harness/progress_bridge` 单一 schema 权威;`turns.jsonl` 按 sid;gateway `spawn_event_pump` 是 live daemon 唯一 turns writer)| **保留**:owning Worker 的 event pump 仍是唯一 `turns.jsonl` writer(`gateway.rs`),`progress/<slug>.jsonl` 仍 per-Worker,`progress_bridge` 仍 schema 唯一权威;Control 是**只读聚合器**,fan-out 查 + 代理历史/SSE,**永不**写 turns/progress ⇒ 无第二写者、无 SoT 分裂。|
| **永不主动 kill 长 session**(预算触顶 auto-disable / `project stop` / `rm --force` 是用户显式命令例外)| **保留 + 硬化**:Worker 因 missed heartbeats 变 unreachable/down **不**触发任何 kill(Control 与 Worker 都不),pane 继续活、愈合时 reconcile;只有显式 `project stop` / `session stop` / `cluster rm --force`(用户命令)拆除并路由到 owning Worker;**Control 无法 kill 远端 pane 除非转发用户显式 stop**。跨机 HITL deny 仍只挡该次工具、不 kill turn(若 defer 跨机 HITL,deny-by-default 也只挡单次)。|
| **会话 = resume-by-session-id**(spawn-on-demand + 按 sid resume + 空闲释放 + 扛重启,非常驻吊着)| **保留且是 linchpin**:resume 仍是 **Worker 本地操作**(`resume_restored_sessions` 按 deterministic pane 名 reattach **自己** 的 pane,从自己 `gateway-state.json`,`gateway.rs` 零改);**Control 永不 resume 远端 pane**(survey 证明远端会误判 `exists=false` 而双 spawn —— sticky ownership + resume-home-only 禁止之),只 re-query Worker 的 session list。跨机 `/use <id>` 解析 node 后透明代理。|
| **ccteam 不生成/桥接项目 `CLAUDE.md`/`AGENTS.md`**(项目知识层归 vendor + 项目自己)| **保留**:control/worker 拆分不碰项目知识层;`home_node` / `project.node` 字段在 ccteam **自己**的 `config.yaml`/`machines.yaml` 注册表,**不在项目 `CLAUDE.md`**;远端 node 上的项目知识层仍 vendor 原生 + repo 自带;跨机记忆(`~/.claude/CLAUDE.md`)仍 per-machine vendor-native、ccteam **只读**。|
| **`ccteam-core` 零 team 名字面量 + ccteam repo 零提示词类型插件**(唯一例外 `cto_role.md`)| **保留**:新类型(`NodeId`、`MachineEntry`、`ClusterRegistry`、`cluster.json`/`machines.yaml`/`node.json`)全是 **infra primitive**(host/path/cert/tag),无 team 名、无 prompt 内容;cto role doc 仅加一句「可派 spec-class + 回报排队状态」(引擎自带默认管家,算引擎配置非插件,无版本号 creep)。|
| **init 布局**(`.ccteam/` 只 state+workflow;role 库 `.claude/agents/`;ccteam hook 写 `settings.local.json`,绝不碰 `settings.json`)| **additive**:仅新增 Control 侧 `~/.ccteam/machines.yaml` + 每机 `~/.ccteam/node.json` + CA 私钥(`0600`);Worker 的 `~/.ccteam` 子目录不变;`settings.local.json` 不动;`~/.ccteam/run` 父目录收紧 `0700`(顺带修单机遗债)。|
| **HITL 批准边界 = `PermissionRequest` hook**(per-session,默认 skip;deny 只挡该次不 kill turn)| **node 内不变**(hook 连本机 `mcp.sock`、读本机 pane env);跨机经 Control mTLS 代理 approve/deny 到 owning node 的 pending 注册表,600s 阻塞语义保留 —— **但**这是最易碎件,**若硬化排不进 scope 则 DEFER 跨机 HITL**(hitl session 只从自己 node 的 UI 驱动),宁缺勿发脆代理。|
| **cto 调度门 = daemon 校验 per-session secret(best-effort 非硬边界)**| **node 内原样**(`verify_session_caller` 不变,secret 在目标 Worker 本地 mint 注入本地 pane env、**绝不过线**);**跨机特权边界另起一层真边界** = 「op 来自 Control mTLS 链路 + Worker 间不互通信」,层叠在不变的 node 内 secret 之上。诚实声明跨机仍是**单一信任域**、不解决多租户/per-agent 隔离。|
| **不解析终端输出**(读 transcript jsonl + 官方 hooks,不 scrape pane)| **保留**:transcript tail 仍在 owning Worker 本机 inotify;Control 只经 `/api/v1` 拿结构化 `SessionView`/历史/`GatewayEvent`,**不 scrape 任何 pane**;screenshot/pty 仍读本机 tmux(presentation 面,不进 SoT,隐含 web 请求落 owning node —— Control 透明代理)。|

---

## 九、分期实现

> 每 Phase 一个验收 gate(对齐 wave 范式);**红线:每 Phase baseline ≥ 上 Phase**(test pass count + clippy 0 warnings),否则不发 PR。**Phase B 含硬 ship gate**(见 §七.7)。

### Phase 0 — 角色折叠 + 单机字节级不变(地基,零行为变化)

- composition root(`main.rs`)拆 `run_control()` / `run_worker()`,加 `--role control|worker|both` 标志,`both` = 默认折叠单进程(loopback、无 mTLS、省 node 前缀)。
- `NodeId` newtype + `~/.ccteam/node.json` 首启铸持久 id(单机也铸,但 `both` 模式不显前缀)。
- **验收**:`--role both` 与今天 daemon **字节级一致**(全测套不退步:`cargo test --workspace` 维持 baseline,clippy 0 warning),证明角色拆分能编译且**单机零回归**。

### Phase A — capability 富化 + sid 节点限定 + 只读 fleet 视图(高价值低风险,零特权跨机控制)

- `capabilities.rs` 富化:per-node + 补真实 claude login 探针(堵 logged-out-but-available 陷阱)+ binary_version + 填 reserved-empty providers(models/context-windows 含 `[1m]`)+ budget_headroom;capability 改成 **machine-keyed TTL'd 心跳信号**(弃 daemon-lifetime 冻结快照)。
- `MachineEntry` 资源 + `machines.yaml` 注册表 + `GET/POST /api/v1/machines` + heartbeat 端点(明文 loopback 先打通,TLS 在 Phase B)。
- sid wire 限定 `n-<node>:s<N>`(Worker 本地 `s<N>` 与 pane/turns 零改);`SessionView`/`SessionResolve`/`SavedGatewaySession`/`TrackedSessionRow` + `ProjectEntry` 加 `node` 字段(serde `default=local`,旧 state 恢复为单机退化 fleet,**无迁移**)。
- 只读 fan-out:`ccteam status` fleet tree + web Machines tab + 全机群 session 聚合(并发 `GET` 各节点 `/sessions` merge);**无**任何特权跨机控制(turn/spawn/stop 仍本机)。
- **验收**:这一阶段单独就答「**从一个视图看我的机群**」。

### Phase B — mTLS 机器身份 + 远端 turn/历史/SSE 代理(开特权跨机控制,硬 ship gate)

- `ccteam-web` 加 server-side rustls + 双向 TLS 控制口 7332(client-cert required, CA-pinned);自签 CA(`rcgen`,**本版唯一真新 dep**)首启生成 `0600`;复用既有 axum `/api/v1` router 挂其后。
- **enrollment 握手**:`ccteam cluster invite`(一次性 + 短 TTL bootstrap token,getrandom+subtle 校验)→ worker `--enroll` 本地生成 keypair + CA 指纹 TOFU pin + CSR → Control 签 client cert(`CN=node_id`);机器凭证独立类、永不打印、可 `cluster rotate`;**绝不**复用 web-token。
- `RemoteGatewayClient`(reqwest+rustls):**忠实复现** in-process gateway 的 submit/stop/resolve/SSE-relay 语义(timeout/`reply_to`-retarget/`after_turn_submitted` watchdog/sid-filtered SSE);Control inbound 解析 `sid→node` 后 forward turn,代理历史 + SSE。
- **fail-closed 前提先行(硬 gate)**:`acl.rs` 空 allowlist 翻 fail-closed、Slack stub 补真 HMAC 或明确不支持 inbound、`~/.ccteam/run` `0700`。
- **2-process loopback mTLS smoke 进 CI**(`CCTEAM_*_BIN` fake 确定性)。
- **🔴 ship gate**:若 server TLS+pin 这版排不进 scope,**DEFER 本阶段,只发 Phase 0+A**(单机+只读聚合)+ 文档「联邦需自备私网」stopgap。

### Phase C — capability 派活调度 + 跨机 fan-out + fleet cost-cap

- `ccteam-sched` leaf crate:`place_session(req)->Node|Queued|Rejected`(**纯函数无 I/O 无 tick,单测**)+ `headroom_score`(live/load/budget 加权)+ 每-tag FIFO bounded 队列(202-queued/503-capacity,**心跳事件驱动 admit**)+ spec_class/tag 模型。
- 三入口 funnel:IM `/new --on`、web create-modal placement picker、cto `session_spawn{placement}`;spawn-time auth-failure 反馈把该 node 该 vendor 标 unavailable 到下次心跳。
- fleet cost-cap:Control 聚合各 Worker cost ledger 成 fleet budget 视图**并强制一个 cluster 上限**(不止 surface Σ-spend)。
- `ccteam node {join,ls,show,rm}` + `cluster {init,invite,ls,rm,rotate}` CLI;web `FanoutPanel`/`QueueStatusChip`/`PlacementPicker`;`cto_role.md` 加 spec-class 派活 + 排队回报一句。

### Phase D(候选/可推后)— 跨机 HITL 硬化 OR 明确 defer

- **若 Phase B 代理足够稳**:Control mTLS 代理 approve/deny 到 owning node pending 注册表,600s 阻塞跨 TLS,deny 只挡单次不 kill turn。
- **否则 DEFER**:hitl session 只从自己 node 的 UI 驱动,remote-owned hitl 在 IM/web 提示「请到该机本地批准」—— 文档写明,宁缺勿发会挂 turn/丢 approval 的脆代理。
- (均不在本版)自动跨机 git clone repo、Control HA/failover、per-agent OS-user 沙箱 = 显式 non-goal。

---

## 十、UI 规划(沿 v0.8.9 统一 chat shell,加 node 维度)

> 所有面都是 **promote 现有资源 API + 加 node 维度**,零新 LLM 面、不 parse 终端输出、不注入 prompt。导航:顶层 tab 增 **Machines**(与 chat / Roles / Settings 平级);session 维度全程可按 node 过滤/分组;create 流程加可选 placement,默认隐藏(auto)不打扰常见单机/自动派活路径。

**web**:

- **Machines/Fleet tab**(顶层新视图):每台机一张 node card —— hostname · node-id · status dot(live/draining/down,按 heartbeat-TTL green/amber/red)· spec_class · capability chips(claude✓/codex✓ + login_ok + `[1m]`/models + binary_version)· tags · headroom bar(live vs max_parallel)· load_avg · budget_headroom · last_seen 新鲜度。
- **统一 session list**(今天 per-daemon → 全机群):每行加 node badge(`@gpu-box`)+ 显示前缀 sid(`n-gpu-box:s7`)+ node filter;分组/过滤 by node 让跨机 fan-out 一眼可见;placement 显示在每个 session 上。
- **per-session chat/terminal/SSE**(对用户透明):Control 代理 owning-Worker 的历史+SSE+terminal,用户察觉不到 entry≠home,只多一个小 `on gpu-box` badge。
- **create-session modal**:加可选 node/tag/placement picker(默认 = scheduler 自动选;高级 = require-tags / class / 显式 pin node);auto 时建完显示**实际落点 node**。
- **cto fan-out affordance**:一个 cto session 的 children 展开,每个 child 标它落的 node,聊天里直观看见「3 台机在并行」。
- **队列/容量提示**:placement 被排队时 session 行显示 `queued #2 (high)`;全满 503 时 modal 给「加机器或抬 `--max-parallel`」诚实文案。

**IM**:

- `/nodes` —— 列机群:每行 node-id + status + tags + headroom 一行简报。
- `/new <project> [@<role>] [--on <tag|node>]` —— 不带 `--on` = 自动 placement(常见路径不变);带 = pin tag/node。回复点名落点(「已在 high 机 `n-gpu-box` 上拉起 s7 → `n-gpu-box:s7`」);排队时诚实回(「所有高配机忙,部署排队 #2,有空位即起」)。
- `/sessions` —— 每个 session 标注其 node。
- HITL `[同意][拒绝]` —— 无论哪台机 own,都经 Control 代理到 owning node 工作(若跨机 HITL 本版 defer,则该按钮对 remote-owned hitl session 提示「请到该机本地 UI 批准」)。

**CLI**:

- `ccteam status` 扩成 **fleet tree**:顶层 NODES(hostname · status · spec_class · tags · live-count · budget),再 per-node projects→sessions,再每 node 一块 web token/url + LAN-ip(每台有自己的 web-token + endpoint)。复用现成 `first_lan_ipv4`/`read_hostname`。
- `ccteam node {join,ls,show,rm}`(对齐 project/session 分组)+ `ccteam cluster {init,invite,ls,rm,rotate}`(operator 面)。

**关键组件**:`NodeCard` · `FleetView` · `NodeBadge` · `PlacementPicker` · `FanoutPanel` · `QueueStatusChip` · `CapabilityChip` · `StatusFleetTree`。

---

## 十一、非目标(显式不做)

1. **不做 remote ProcessBackend**(SSH / 把别机 pane 直接当本地驱动):那要把 `BoxStream<ThreadEvent>` 流式跨机 + transcript inotify 跨机 + hook 跨机重 plumb,把最硬的 5 条单机绑定全塞一个 trait;联邦在 gateway/REST 上层做,复用每台本机执行栈不动。`RmuxEndpoint` 虽 `#[non_exhaustive]` 留了 TCP,**本版也不走那条**。
2. **不做 daemon 进程迁移 / session 跨机迁移**:`GatewaySession` 持非序列化 `Arc<dyn HarnessAdapter>` + `ThreadHandle`,执行钉死 owning node;一个 sid 生命周期内**不迁移、不再平衡**(同 project 的新 session 可落别处)。
3. **不做 Control HA / failover**:单 Control = 单 IM/web 入口 + placement 单点(本版接受、文档写明 **degraded-not-dead**:Control 宕则 Worker live session 继续跑 + 可本机驱动,重启重建视图)。
4. **不做 per-agent / 多租户隔离**:整队是**单一信任域**(同一个人的机器);enroll 一台 Worker = 信任它如同 Control 机;不声称保护某 Worker 免于被已沦陷 peer 攻击(星型 Worker 间不通信已压低横向爆炸半径,但**非沙箱**)。per-agent OS-user/sandbox 仍 defer(单机本就 defer)。
5. **不做自动跨机 git clone repo**:项目 pin 在 init 它的那台 node(`config.yaml` 存本机绝对路径),缺 repo 的 placement **fail-fast 报清晰错,不静默 cwd 失败**;自动 clone 推后。
6. **不做编排器 / tick loop / DAG executor**:`ccteam-sched` 严格只决定 WHERE(placement),WHAT/HOW-MANY 归 cto/用户;**不注入额外并行**;`ccteam-flow` 仍 deferred、daemon 仍**不 tick**。
7. **不做历史迁移 / 兼容 shim**:pre-v1.0 红线,新旧 state 不兼容时清旧数据重 init;旧 node-less state 文件 serde-default 恢复为单机退化 fleet 是唯一「兼容」(本就是 standalone 退化形态)。
8. **不在明文上发特权跨机控制面**:若 server TLS + cert-pin 排不进 scope,**DEFER 跨机特权控制**,只发单机 + 只读聚合视图 + 「自备私网」stopgap。
9. **不做 mDNS / 广播自动发现**:LAN 上 web 已半开,自动发现是安全 footgun;只走 operator 显式 `cluster invite` + 一次性 token 入网,**可审计**。
10. **本版不引入 CRL/OCSP 证书吊销基础设施**:吊销 = `cluster rm` + cert 过期 + 轮转(`cluster rotate`)+ Control 重启;残余风险(已 mint 未到期 cert 的撤销窗口)**文档承认**。

---

## 十二、开放问题(待 user / 设计深化)

1. **fleet cost-cap 强制点**:聚合在 Control 后,一个跨 node 的 cluster 上限是**软 throttle**(placement 避让 + 提示)还是**硬阻断**(到顶后拒绝所有新 spawn)?硬阻断更安全但可能在用户没看 status 时**突然全队停摆** —— 需定语义 + 是否给 per-node 与 fleet **两级 cap**。
2. **spec_class 自动派生 vs 显式声明的边界**:`workload_class`(deploy/loadtest→high,discuss/prototype→low)默认怎么从 role 名/persona/vendor 推导?推导规则若太聪明会**滑向 `ccteam-flow` 编排**;是否只做**最保守推导**(roleless/讨论类→low,显式 IM/参数才 high)而把判断权留给 cto/用户?
3. **node 前缀 sid 的 back-compat 显示**:单机 `--role both` 用裸 `s<N>`、多机用 `n-<node>:s<N>`,但用户从单机升多机时已有的裸 sid 怎么办?pre-v1.0 直接清数据重 init,还是给一次性「bare sid 视为 self-node」的解析兜底(后者要确保 `/use`、SSE filter、`session_dispatch` 全路径一致)?
4. **跨机 HITL 是否进本版**:Phase B 的 `RemoteGatewayClient` + SSE relay 稳定度直接决定 Phase D 能否硬化 600s 阻塞代理;需要一个明确的 **go/no-go 信号**(loopback smoke 的 HITL round-trip 通过率 + 断连恢复)来决定发还是 defer。
5. **Worker endpoint 的 LAN 可达性假设**:`machines.yaml` 存 `https://<lan-ip>:7332`,但用户的机可能在不同子网/NAT 后(家里工作站 + 云 GPU 机 + 公司机)。本版假设同一可达私网(用户自备 wireguard/tailscale),还是要支持 **Control 出站 + Worker 反向心跳保持长连**(类似 IM 的 outbound 模型)以穿 NAT?后者改 wire 方向,影响 `RemoteGatewayClient` 设计。
6. **capability 心跳 TTL 与 spawn-failure 反馈环的具体参数**:TTL 太长会把活路由到刚登出/触顶的机,太短则 N 机 ×15s 心跳噪声;spawn-failure 如何**可靠归因到 login**(而非别的错)再跨机回传 Control 标 unavailable —— 这条反馈环实现非平凡,需定 retry/解除策略。
7. **Control 的 CA 私钥保护**:本版 `0600` 存 `~/.ccteam`,但 CA 私钥泄露 = **可伪造任意 node 身份(crown jewel)**。是否够?要不要 OS keyring / 单独加密?这是单点风险,需文档显式标注 + 评估是否本版收紧。

---

## 十三、流程 & 变更记录

- **流程**:doc-first → user review 本 PRD → scope 冻结 → dev-plan(Phase 0–D 拆 wave + subagent briefing 含验收 gate)→ dev session 实现(workflow,延续 v0.8.8 模式)→ `workspace.package.version` bump 0.8.x→0.9.0 → CLAUDE.md §一 baseline + `docs/tech-design.md` + `README.md`(英文,不含版本进展)+ `docs/usage.md`(含单一信任域声明)回填。
- **作者范围**:本文作者**只收集需求 + 写文档,不开发**;实现交另一个 dev session。
- **代码核验**:本 PRD 的代码事实(三条不可迁移硬绑定 / `next_session` 自铸 / `verify_session_caller` RAISES THE BAR / `acl.rs:55` 空 allowlist fail-open / Slack stub / `axum::serve` 裸 TcpListener 无 `bind_rustls` / codex-only login probe / `RmuxEndpoint` `#[non_exhaustive]`)均已 grep 核验过当前 dev 代码,line 号随代码漂移仅作指针。
- **2026-06-07 初版**:多机协同最终设计落 PRD —— Machine 一等资源(node.json + machines.yaml + capability 富化)+ Control/Worker 星型联邦(`--role control|worker|both`,执行钉 owning node、只 DATA+CONTROL 过 mTLS)+ placement policy(`ccteam-sched` 纯函数 + tag/login/headroom 过滤 + per-tag FIFO 背压)+ 跨机状态 SoT 与对账(sid 节点前缀、只读 fan-out 聚合、Worker 是 SoT)+ 诚实跨机安全章节(Tier1 TLS / Tier2 mTLS 机器身份 / Tier3 in-pane 留本地 + 5 项 defer + 单一信任域声明 + 硬 ship gate)+ 红线逐条对照 + Phase 0–D 分期 + 10 项非目标 + 7 个开放问题。
