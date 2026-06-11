# v0.9.0 PRD(候选 v2,doc-first 待 user review)— 宿主轴:SessionHost 契约 + agent sandbox 集群编排(ccteam-as-control-plane)

> 状态:**候选 v2**(2026-06-10),doc-first;user review 冻结 scope 后才动代码。
> **本稿取代 2026-06-07 的 v1 草案**(「多机协同:Machine 一等资源 + Control/Worker 星型联邦 + 能力派活调度」)。v1 三件已物理归档至本目录 **`archive-v1/`**(prd-star-federation.md + architecture.html + prototype.html,原 git 47058ec)。v1 的 user 诉求表(D1-D7)与读码结论**继承**,联邦机制按 owner 裁定链**重排**——处置表见 §三;与 LAP 的对比分析见 §四。
> 来源:`docs/research/cloud-control-center-requirements.md` **§8 owner 裁定链**(TG 1106 单租户 sandbox 拓扑 → 1107 基底 kubernetes-sigs/agent-sandbox → 1111-1112 租户拓扑与 CLI 落位 → 1113-1114 单机/集群统一 → 1115「新架构写入下一个版本文档」)。
> 关系:**前置 = v0.8.11(协议轴:stream-json adapter)**;v0.8.12(track-upstream 市场)正交。
> 立场:**两次解耦的第二步** —— v0.8.11 把会话协议从 PTY 解耦,v0.9.0 把会话宿主从本机解耦。壳的五要素(IM 网关 / 持久 sid / 多前端 dispatch / vendor-native / 双 SoT 可恢复)对所有组合不变;**自托管单机仍是 zero-config 默认入门形态**,集群是同一引擎的 scale-out,不分叉产品。

---

## 〇、一句话

session = **协议 adapter × 宿主 backend** 两正交轴:协议轴(0.8.11 已立)= tmux-PTY | stream-json;宿主轴(本版新立)= **local | satellite | k8s-agent-sandbox**。统一缝 = 一个 **SessionHost 契约**,一套代码三实现(local = runner 退化形态、satellite = 裸机出站 WS、k8s = SandboxClaim 托管的同一 runner 二进制);采纳裁定②(**ccteam-as-control-plane**):网关居租户集群入口,会话 spawn 进单租户 agent sandbox。

## 一、user 诉求继承(v1 D1-D7 → 新模型重映射)

> v1 的诉求表仍是验收基准(对齐 `docs/requirements.md` 痛点映射纪律),落地机制更新:

| # | user 诉求 | v2 落地 |
|---|---|---|
| D1 | 「我手上 N 台机器,想都用上」 | **satellite 档**(裸机,出站 WS 编队,不强迫上 k8s)+ **k8s 档**(托管集群);hosts 注册表 |
| D2 | 「每台都登录了 claude+codex」 | capability 握手继承 v1 富化:**真实 claude 登录探针**(堵 logged-out 仍 `--version` exit-0 陷阱)+ codex login status + binary_version + models/context-window + budget headroom;TTL'd 心跳可刷新 |
| D3 | 「一个 IM/web 入口统一驱动」 | gw 单例持 Telegram long-poll(单消费者)+ web/REST;runner/satellite **绝不**起 IM/web listener |
| D4 | 「多台机器协同」 | gw 按 sid→host 路由 turn/事件/stop,经各 runner 的出站 WS 双向流 |
| D5/D6 | 「高配机跑部署压测 / 低配机跑讨论原型」 | **v0.9.0 显式选 host**(`/new --host` / project 默认 host / role 默认);tags/class 留作元数据与过滤展示;**auto-placement 调度降级 deferred**(§三) |
| D7 | 「并行度拉满」 | 并行天花板 = Σ per-host 容量;fleet cost-cap 聚合强制(继承 v1 §5.5)防静默 N× spend |

## 二、架构(新增层 SoT;review 冻结后并入 tech-design)

1. **两轴模型与支持矩阵**:`(protocol: tmux | stream-json) × (host: local | satellite | k8s-agent-sandbox)`。v0.9.0 目标:local×两协议(已有,收编)、satellite×stream-json(**P0**)、k8s×stream-json(**P0**)、satellite×tmux(终端镜像,P1)、k8s×tmux(P1,§五 Q3)。
2. **SessionHost 契约**(统一缝,唯一新 trait):`spawn(argv, env, cwd)` / stdin 写入 / 事件流(NDJSON)/ 终端字节流 / `pause` `resume` `kill` / 健康。**gw 消费端对 backend 无感知** —— 0.8.11 stream-json adapter 的消费逻辑一行不改,管道从本机 pipe 换成 WS。
3. **runner = 旧 Worker 的执行核续命**:v1 §3.2 读码结论(三条本机硬绑定:活 adapter 句柄不可序列化 / transcript inotify 本机 / AF_UNIX 不跨主机 ⇒ **执行钉 owning host**)在 v2 **原样成立且被更彻底地执行** —— 不跨机流式任何硬绑定,把**整个执行核**(spawn + adapter/event pump(仍是该 session 唯一 `turns.jsonl` writer)+ 本地双 SoT + socket 端点 + transcript tail + PTY/rmux 自托管)随 CLI 放进宿主;砍掉的是 Worker 的 daemon 面(无 IM、无 web、无 axum `/api/v1`、无入站口)。
4. **一个二进制三种角色**:`ccteam satellite` 子命令 —— local = in-process 退化(无 WS,单机与今天字节级一致,继承 v1 `--role both` 承诺);裸机 = systemd 常驻、**出站 WS** 连 gw(Cursor self-hosted worker 同款语法);k8s = pod 内同一进程,生命周期归 SandboxClaim。
5. **租户拓扑**:每租户 = 1 个 `ccteam-gw` sandbox(daemon;**不装 vendor CLI**)+ N 个会话 sandbox(镜像预装 claude/codex + runner + 项目持久卷 + 该客户凭证)。**多租户切在 k8s namespace 层,ccteam 永远单租户**;gw 自身也是 Sandbox CR。
6. **身份与寻址**:sid 全局唯一、单调、不复用、**不含 host 前缀**(owner 裁定:位置透明;v1 的 `n-<node>:s<N>` 取消);gw 持 sid→host 路由表(v1 Control 本就有);sid ↔ Sandbox CR 一一映射(`ccteam-s<N>`);host = session 第三 facet(vendor × provider × **host**)。IM/web 寻址面零变化。
7. **生命周期**:空闲两档 —— **pause**(整 pod 冻结,in-flight 原地活,恢复即续)/ **深度休眠**(pod 回收、持久卷留存,唤醒后 runner 重 exec `--resume`);WarmPool 压冷启动;**gw 重启/宕机 = 会话由宿主托管继续跑**(k8s 托 pod、satellite 托进程),gw 起来按 sid 重连 —— 继承 v1 §3.6 **degraded-not-dead** 语义:入口宕 ≠ 会话宕;诚实差异:v1 Worker 自带 standalone web 可应急驱动,v2 runner 无 web 面,gw 宕期间会话只跑不可驱(§五 Q6)。
8. **状态布局**:vendor transcript + `progress.jsonl`/`turns.jsonl` 双 SoT **落会话宿主**(本机磁盘 / sandbox 持久卷;SoT 跟 session 走,不集中);gw 只持账本、注册表、outbound ledger、sid→host 路由(可从宿主重建,继承 v1「Worker 是 SoT,Control 可重建」)。
9. **入站面收敛**:v1 = 每 Worker 开 mTLS :7332 入站;v2 = **satellite/runner 全员零入站**(仅出站 WS),唯一入站面 = gw 的一个认证 WS 端点(enroll token + per-host secret,默认 bind 集群网/tailnet,绝不公网;与备忘录 #1 bind 纪律同表)。
10. **红线兑现表**:No-prompt-injection(`--agent` 自读,sandbox 内同义)· 不解析终端输出(字节流仅镜像)· **永不主动 kill**(pause ≠ kill;深度休眠 ≡ 既有空闲释放;budget 例外不变)· `progress_bridge` schema 单一权威不变 · `ccteam-core` 零 team 字面 · 单机 zero-config 入门不变。

## 三、对 v1 草案(2026-06-07,47058ec)的处置表

| v1 设计 | v2 处置 | 理由 |
|---|---|---|
| Machine 一等资源(node id / tags / class / status / capability_snapshot,`machines.yaml`)| **保留,改名 host**;registry 在 gw;`ccteam host {add,ls,show,rm}` | 概念有效;命名对齐宿主轴 facet |
| 星型 Control/Worker | **保留星型;Worker 瘦身为 runner**(执行核保留,daemon 面剥离)| v1 §3.2 硬绑定结论支持「执行核整体随 CLI」;daemon 面是单机产物 |
| Worker 入站 mTLS :7332 + cluster CA(`cluster {init,invite,rotate}`)| **替换**:runner 出站 WS;入站面只剩 gw 一个认证端点 | 零入站红线;sandbox/NAT 现实;Cursor worker 先例;CA 运维面整段消失 |
| 节点 wire 复用 `/api/v1` REST/SSE(不造新 RPC)| **替换**:出站 WS 流(payload = CanonicalEvent/NDJSON(0.8.11 已立)+ spawn/stdin/pause/resume/kill/health 控制 verbs)| 方向反转(入站→出站)使 REST 复用不再成立;新 genus 很小,事件格式已存在 |
| 跨机 sid = `n-<node>:s<N>` 前缀 | **CUT**:sid 不含 host(owner 裁定);gw sid→host 路由表承担 lookup | 位置透明 > 热路径零 lookup(gw 内存 map,代价可忽略) |
| placement 调度(tag 过滤 + 真登录过滤 + headroom 加权 + per-tag FIFO 背压 + anti-overparallelize)| **降级 deferred**:v0.9.0 显式选 host;真登录过滤保留进 capability 握手;anti-overparallelize 并入 fleet cost-cap | 调度智能属被红队的 smart-logic 类;显式选择已覆盖 D5/D6;真实 thrash 信号出现再回归(§五 Q7) |
| `--role both` 单机退化字节级一致 | **保留语义**:LocalHost = runner in-process 退化 | zero-config 入门红线 |
| fleet cost-cap 聚合强制 | **保留**,并入 H5(host 维账本) | survey 警告有效 |
| capability_snapshot 富化(claude 真登录探针等)| **保留**,移入 satellite 注册/心跳握手 | D2 原样有效 |
| Control 单点诚实(degraded-not-dead)| **保留**,措辞按 v2 §二.7 更新 | 语义继承,机制变化 |
| `architecture.html` / `prototype.html` | **已归档 `archive-v1/`**,review 后按新架构重做或删 | v1 附件 |

## 四、对比分析:vs litellm-agent-platform(LAP)

> 依据 `docs/research/cloud-control-center-requirements.md` §2(LAP HEAD 05bc97c 全仓侦察)。LAP 与 ccteam 共享赛道名词(session / key / budget / HITL / 多 runtime),**执行域几乎不相交** —— 同名词逐维对照:

| 维度 | LAP | ccteam v0.9 |
|---|---|---|
| **结构位置** | vendor 托管 API **之上的 builder**(agent = DB 行) | 自有集群 sandbox **之上的控制面**(session = 活进程) |
| **agent 行为定义** | `compose_agent_system_prompt` 拼装注入(**prompt 注入路线**) | vendor 原生 `--agent` 文件自读(**零注入红线**) |
| **执行单元** | 一次性沙箱 run(无常驻) | **可恢复常驻 session**:持久 sid + pause/深度休眠 + `--resume` |
| **多 runtime 适配物种** | RuntimeAdapter(6 方法)适配**托管 agent API**(OpenCode/Hermes/Claude Managed/Cursor API/DeepAgents) | HarnessAdapter × SessionHost 适配**订阅 CLI 进程**(claude/codex 本机二进制)—— 不同物种,不正面互替 |
| **会话状态** | Postgres(SessionRow + runtime_events;runs 曾内存态) | 文件双 SoT 随宿主走(transcript + progress/turns 落 sandbox 卷),gw 可重建 |
| **key 托管** | 加密入 DB、「调用者永不见 provider key」(table-stakes,做得对) | per-session env 注入(H3 兑现备忘录 #3);**LAP 反面**:master key 缺省全放行、gateway key 内存态且与 master 等权 → ccteam 对应纪律 = #1 bind 默认收紧 + #19 范围化 token(P2) |
| **计费** | SpendLogs 只记账、**无强制** | 账本 + per-project 预算闸前置强制(#7)+ fleet cost-cap(H5) |
| **调度** | CRON routines 表 + trigger 端点(无 tick) | 「daemon 不 tick」红线:CRON 显式不做(系统 crontab + `POST …/turn` 等价剧本);auto-placement deferred |
| **入口形态** | 17 页 dashboard;IM 仅 per-agent Slack;Teams 页未配 | **IM 单入口**(TG/Lark 长轮询零入站)+ 统一 chat-shell web + 终端镜像 |
| **终端** | 全仓无 PTY/tmux | tmux/byte-faithful 终端 + 远端字节流回传(H4)—— LAP 结构性做不了订阅 CLI 形态 |
| **HITL** | flat inbox,任何 key 等权 | PermissionRequest→IM [同意][拒绝],per-sid;named gatekeeper 为 P2(#13) |
| **互补位** | LiteLLM proxy 可作 ccteam session 的 `base_url` 上游 | credential 普通字段即可(专属集成已 CUT,见备忘录 §5) |

**LAP 的 Claude Code / Codex 集成协议实锤**(2026-06-11 读码,owner 追问 TG 1117):
- **claude 侧**:LAP 的 harness 注册表(`src/agents/harnesses/`)**只有一个** `claude-code`,且**不驱动 claude CLI 二进制**,而是每次 run 现场 `npm install @anthropic-ai/claude-agent-sdk@latest`,跑一段 Node 脚本调 **Claude Agent SDK `query()`**:`permissionMode: bypassPermissions` + `systemPrompt: {type:"preset", preset:"claude_code", append}`(**注入路线**)+ `includePartialMessages`,把 SDK 消息转成 NDJSON frames(assistant/user/stream_event/result)回 Rust 解析;**一次性 run、无持久会话、无 resume**(`session_id` 缺省填 `"lite-harness"` 占位),且每 run 重装 SDK(冷启动 + 供应链面)。
- **底层同源**:Agent SDK 的 `query()` 内部就是 spawn claude CLI 的 stream-json 模式 —— LAP 与 ccteam v0.8.11 踩的是**同一条 vendor 官方协议**;差别全在形态:LAP = npm SDK 包装 + 一次性 + preset 注入;ccteam = 直驱用户已装 CLI + 持久 sid/resume + 零注入 `--agent`。
- **codex 侧**:**无 harness**。codex 在 LAP 仅以两种身份出现:① 模型 provider 注册名(`registry.register("codex", OPENAI_API_BASE, OpenAiResponsesTransformation)`,OpenAI Responses API 转换层);② `lite codex --reset` 凭证命令 —— 把 LiteLLM 网关 url+key 写给**本机** codex CLI 当模型上游(`base_url` 反向集成:LAP 不编排 codex,而是让 codex 的模型流量过 LiteLLM proxy)。`lite claude` 同理。

**一句话**:LAP 证明了赛道 table-stakes(统一 API / key 托管 / 持久 session 记录 / 记账 / HITL),也示范了赛道翻车点(默认全放行 / 等权 key / 注入路线 / 无常驻);其 Claude 集成停在「Agent SDK 一次性 run」形态,codex 干脆只是 provider —— **「持久订阅 CLI 会话」(Claude Code 本体 + codex CLI)整块无人占位,正是 ccteam 的位**。ccteam v0.9 不与它抢「托管 API builder」,两者还可经 `base_url` 互补共存(ccteam session 的模型流量可指 LiteLLM proxy,credential 普通字段)。

## 五、Open questions(review 时定)

1. **①/② 正式拍板**:全部澄清沿②(ccteam-as-control-plane)推进,需 owner 一句话钉死。
2. **宿主轴排期**:satellite 先行(H2 独立 ship、无 k8s 依赖、裸机即可 soak)vs 直上 k8s —— **建议 satellite 先行**(同一 runner 代码路径,k8s 只是托管化;排期题非架构题)。
3. **支持矩阵裁剪**:k8s×tmux(沙箱内终端镜像)首版要不要;倾向 P1 跟随 H4。
4. **镜像策略**:官方 SandboxTemplate 镜像谁 build / 谁签名 / 更新节奏(连备忘录 #1 签名供应链与 #15 自升级)。
5. **0.8.11.x 尾随原子归属**:#2 回合仲裁、#1 最小子集(bind 翻转 + doctor --security)先行独立 ship 还是并入本版;倾向先行(不依赖宿主轴)。
6. **gw 宕机时的应急驱动**:接受「会话只跑不可驱」(v2 现状)还是给 satellite 留一个可选 standalone 应急面(v1 遗产);倾向接受 + 文档诚实,HA 不做。
7. **auto-placement 回归条件**:出现真实多机 thrash/手选疲劳信号后,以独立小版本回提 v1 §四算法(资产不丢,文档在案)。

## 六、交付物 H1–H6(候选)

- **H1 · SessionHost 契约 + LocalHost 收编**(纯重构 wave):现有两条 spawn 路径(tmux pane / stream-json 子进程)收编进契约;全 baseline 零回归 = 唯一验收。
- **H2 · satellite backend**:`ccteam satellite` 子命令、出站 WS 协议(事件流 + 字节流 + 控制 + 心跳重连)、hosts 注册表与 CLI、capability 握手(D2 探针)、spawn 选 host、doctor 卫星健康;fake-satellite(本机 WS 回环)确定性 e2e + 真双机 smoke。**无 k8s 依赖,可独立 ship。**
- **H3 · k8s-agent-sandbox backend**:SandboxClaim 建删、SandboxTemplate 镜像规范(vendor CLI + runner 预装)、凭证注入(备忘录 #3 顺带兑现)、sid↔CR 映射、pause/深度休眠对接、WarmPool;kind/k3s CI e2e(claim→spawn→turn→pause→resume→delete)+ 真集群短 smoke。
- **H4 · 远端终端与 screenshot**:tmux backend 跑在 satellite/sandbox 内,逐字节流回传 web 终端(v0.8.9 byte-faithful 语义延续);screenshot 同管道。
- **H5 · 账本加 host 维 + fleet cost-cap**:成本/回放聚合按 `slug × sid × host × vendor`;fleet 级 cap 聚合强制(v1 §5.5);Status 列 host facet 与各宿主活动态。
- **H6 · 文档 + ship gate**:tech-design 架构节重写(两轴模型 + 协议→代码指针)、usage 卫星与集群剧本、CLAUDE.md §〇/§一、版本归档;commit 英语、文档中文。

**分期建议**:H1 → H2(satellite 先行)→ H3 → H4/H5 尾随 → H6。

## 七、显式不做(OUT)

| OUT 项 | 理由 |
|---|---|
| 跨租户 SaaS 控制面 / 租户管理 UI | 多租户切 namespace 层,ccteam 永远单租户 |
| auto-placement 调度(v1 §四算法) | deferred,§五 Q7 触发回归;v0.9.0 显式选 host |
| 备忘录 #4 软围栏、#5 uid 隔离 | §8 已 CUT:隔离 = sandbox 边界白送 |
| Control HA / gw 多活 | 单点诚实(degraded-not-dead)继承;HA 不做 |
| 编排智能(ccteam-flow)、自动扩缩容 | 编排层 deferred 不变;WarmPool 是 k8s 原语非自研 |
| 非 k8s 容器编排(nomad/swarm)、multi-cluster federation | 单基底先打穿;host 抽象已留位 |
| 镜像市场 / 第三方 SandboxTemplate 分发 | 连 #18 hub 签名后再议 |
| IM/web 寻址面新功能 | 宿主轴对用户面透明是本版纪律 |

## 八、参考

- `docs/research/cloud-control-center-requirements.md` —— 需求母体(§1-7 论证存档 + **§8 owner 裁定链 = 本 PRD 直接上游**)
- 本目录 `archive-v1/`(prd-star-federation.md + architecture.html + prototype.html,原 git 47058ec)—— D1-D7 诉求表、§3.2 硬绑定读码、capability 富化、fleet cost-cap、placement 算法(deferred 资产)
- `references/litellm-agent-platform`(本地,HEAD 05bc97c)—— §四对比分析对象;table-stakes 与反面教训双重参照
- `docs/versions/v0-8-11/prd.md` —— 协议轴前置(stream-json adapter,E1-E5)
- [kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox)(v0.4.6,SIG Apps)—— Sandbox / SandboxTemplate / SandboxClaim / SandboxWarmPool;pause/resume + 稳定身份 + 持久存储 ≅ resume-by-sid + 双 SoT 的集群原语
- `references/alleycat`(出站桥 / fake-vendor 测试范式)· Cursor self-hosted worker(出站 worker 语法先例)· `references/codex-desktop-app-analysis.md`

## 九、流程 & 验收(初稿,review 后细化)

- **doc-first**:本 PRD 候选 → user review → 对抗式 review 深化(沿 v0.8.10 范式)→ dev-plan 落本目录 → wave-per-phase + handoff(每 wave baseline ≥ 上 wave)。
- **验收骨架**:H1 全 baseline 零回归;H2 fake-satellite 确定性 e2e + 真双机 smoke(spawn→turn→断 WS→重连不丢不重→resume);H3 kind/k3s CI e2e 全生命周期 + 真集群短 smoke;H4 字节流终端一致性断言(键入回显 / resize / 重连屏幕一致);红线 guard:satellite/runner 零入站(无监听端口)、kill 语义(pause/休眠不计 kill)、`cargo test --workspace` ≥ 起跑实测 + clippy/fmt 0。

## 十、变更记录

- **2026-06-07 v1 初稿**(47058ec):多机协同 = Machine 一等资源 + Control/Worker 星型联邦(Worker= daemon−IM,入站 mTLS 7332,REST wire,节点前缀 sid)+ 能力派活调度。DRAFT,未 review 冻结。
- **2026-06-10 v2 重写**(本稿):按 owner 裁定链(单租户 sandbox 拓扑 / agent-sandbox 基底 / ccteam-as-control-plane / 单机集群统一)重排:两轴模型 + SessionHost 契约 + runner 三角色(= v1 Worker 执行核续命,daemon 面剥离)+ 零入站 wire 反转 + sid 去前缀 + placement 降级 deferred;v1 诉求表与读码结论继承,处置表 §三 逐项裁决。候选,待 review。
- **2026-06-10 +v1 归档 & LAP 对比(TG 1116)**:v1 三件物理归档 `archive-v1/`;新增 §四 对 `references/litellm-agent-platform` 的逐维对比分析(位置/注入路线/执行单元/状态/key/计费/调度/入口/终端/HITL/互补位),后续章节顺延重编号。
