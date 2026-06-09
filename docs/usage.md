# ccteam 使用指南

一份命令为主的端到端用户指南:install → init → config → start → 接入 IM → 日常用 → 运维。
直接照着代码块敲。所有命令都对照当前 CLI 校验过。

核心模型:**一个 chat 就是一台终端**。四个对象——

- **chat**:Telegram / 飞书(Lark)私聊/群聊或 web 控制台会话。每个 chat 有自己的当前 project、当前 session、session 列表,互相隔离。
- **project**:本地一个已 `ccteam init` 的目录,用 slug 标识。
- **session**:一个**独立**的 agent 会话(spawn-on-demand、按 id resume、空闲释放),像 Claude Code 原生 session 一样自带上下文,属于某个 chat + 某个 project。一个项目可同时开多个会话,**互不串台**——哪怕同一个 role 开两个,也各聊各的。每个会话有持久句柄 `s<N>`(单调递增、扛 daemon 重启、不复用)。
- **role**:session 启动时绑定的角色(`.claude/agents/<role>.md` 定义 persona + 工具)。session 以 `claude --agent <role>` 启动,**就 become 该 role**;role 可留空 = 裸 `claude`(不带 `--agent`,brain 走项目 `CLAUDE.md`)。默认 role 是 `cto`(chat-first 管家)。换 role = IM 里 `/role <role>`(底层 = 带新 `--agent` 原地重启该会话,**句柄 `s<N>` 不变**)。

两类命令别混:

- **shell 里的 `ccteam <子命令>`**(安装 + 运维):扁平 `init / start / stop / status / config / doctor` + 分组 `project <ls|show|new|stop|rm>` + 分组 `session <ls|attach|pause|resume|register|unregister|persona|add-tool|bots|role>`。
- **IM / web chat 里的网关命令**(日常对话):`/pair /new /use /role /cd /sessions /projects /newproject /help`、路由前缀 `@<handle>`、管理前缀 `@ccteam`。

> **命令菜单**:daemon 启动时会把**网关自有命令**注册进你的 IM 客户端(Telegram 走 `setMyCommands`)—— 在聊天框敲 `/` 就能看到候选(其余透传给 agent 的 slash 不进菜单,因为它跟当前 vendor 相关、列进去会误导)。随时发 `/help` 看网关命令清单 + 「其余 `/` 命令透传给当前 agent」的说明。

整体上手六步(终端一次性 + IM 日常):

```text
install → init → config → start → /pair(+approve) → /cd(自动 spawn cto)

curl install.sh | sh  →  ccteam init  →  ccteam config  →  ccteam start
                                                              ↓
IM: /pair <code>  →  /cd <项目>  →  直接发任务 / /role <role> / /new · /use · @handle
```

---

## 1. 安装

```bash
# 装 ccteam CLI(GH Releases 预编译;linux + macOS arm/x64,Windows 走 WSL2)
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
```

```bash
# 提示 ~/.local/bin 不在 PATH 时,加进 shell 配置后重开终端
export PATH="$HOME/.local/bin:$PATH"
```

```bash
# fallback:从源码装(二进制名仍是 ccteam)
cargo install --git https://github.com/firstintent/ccteam ccteam-cli
```

ccteam 不打包 Claude / Codex,调用你机器上的真实 CLI。本版主驱动是 **claude-code**(Codex best-effort)。确认装好且已登录:

```bash
claude --version          # 必需;需要时按提示登录
codex --version           # 可选,用 Codex 会话才需要
```

模型支持矩阵:

| 路径 | 支持级别 | 说明 |
|---|---|---|
| Claude harness + Claude 家族模型(`claude-*` / `sonnet` / `opus` / `haiku`) | first-class | 默认路径;角色 frontmatter 的 `model:` 用这些值最稳。 |
| Codex harness + Codex/OpenAI 模型 | best-effort | Codex adapter 可用时工作,真机长跑属于 best-effort。 |
| Claude harness + 非 Claude 模型(如 `deepseek-via-claude`) | 未验证 | ccteam 不阻断,但会在 session 启动时提示“模型未验证”;若空转,改回 `sonnet`/`opus`/`haiku` 后重新 `/new`。 |

agent 侧的 `mcp__ccteam__*` 工具由 `ccteam config`(见 §3)注册 —— 它**同时**给 Claude(`~/.claude.json`)和 Codex(`~/.codex/config.toml`)写入 ccteam MCP server,无需 `/plugin`、也不再用 `doctor --install-mcp`。ccteam 本身是纯 CLI、不是 vendor 插件。

> **从旧版本升级(须知)**:本版的独立 session 模型与旧的 per-role 会话状态**不兼容**。升级前请**清掉旧状态**再重新初始化 —— `ccteam stop` → 删 `~/.ccteam` 以及各项目里的 `.ccteam/` → 各项目重跑 `ccteam init` → `ccteam config` → `ccteam start`。旧的 per-role 历史会丢(pre-v1.0 阶段可接受,无兼容迁移)。**不碰你的业务代码 / `CLAUDE.md` / `.env` / `.git`**。
>
> 另外两处变化:① **role/skill/workflow 的内容现在住 ccteam 插件市场(ccteam-hub),不再随 ccteam 内置** —— 旧的内置 role catalog 已移除,`ccteam role search/add` 改从市场(经 HTTPS + `~/.ccteam/hub-cache/`)拉取(见 §7)。② **`ccteam init` 不再有 `--mode` 选项**(旧的 agent-team 模式已退役),始终就地初始化 artifact 型项目。

---

## 2. 初始化项目(`ccteam init`)

在每个你想交给 ccteam 管的目录跑一次 `init`。默认就地初始化当前目录(slug = 目录名):

```bash
cd ~/projects/demo-app
ccteam init
```

`ccteam init` 写入两处,**只碰 ccteam 自己的东西**:

- 项目 `.ccteam/` —— 仅 `state.json` + `workflow.yaml`(项目状态 + agent 拓扑声明,无 prompt)。
- 项目 `.claude/agents/cto.md` —— 默认 role(chat-first「CTO 管家」persona)。这是 ccteam 唯一托管的「指令面」。
- 项目 `.claude/settings.local.json` —— ccteam 的 hook + 基础设置写进**本地层**(gitignored、与你的 `.claude/settings.json` 合并),**绝不碰**你的 `.claude/settings.json`。

> **ccteam 不生成、不修改、不抑制项目知识文件**。项目自有的知识走 vendor 原生:Claude 自动读 `CLAUDE.md`,Codex 自动读 `AGENTS.md` —— 这俩文件都归项目自己(老项目用自己的,新项目有啥读啥),ccteam 不接管、不桥接。业务代码 / `.git/` / `.env` 永远保留;重跑 `init` 安全。

```bash
ccteam init --in /path/to/repo          # 在别处初始化(slug 默认取目录名)
ccteam init --slug demo-app             # 覆盖自动推断的 slug
ccteam init --force                     # 覆盖 ccteam 生成物(在源码目录自举时也用它)
ccteam project ls                       # 列已知项目
```

**slug 撞名 = 数字累加**:默认 slug 取目录名。两个不同路径同名(如 `/ws/demo` vs `/ws2/demo`)时,后建的自动累加成 `demo2` / `demo3` …(可读,非随机后缀)。同一路径重复 `init` = re-init 刷新,不算撞名。需要显式名用 `--slug`。

要在 `<projects_root>/<team>-<slug>/` 下**新建**一个带 team 前缀的项目目录,用 `ccteam project new <slug>`(见 §7),`init` 是就地初始化已有目录。

---

## 3. 一次性配置(`ccteam config`)

`ccteam config` 是 setup 总入口,**吸收**了原来散落的「装 MCP / 配 IM token / 改偏好」。

```bash
ccteam config                  # 交互式编号菜单(需要 TTY)
```

菜单三项:① 注册 / 刷新 ccteam MCP 服务(**给 Claude `~/.claude.json` + Codex `~/.codex/config.toml` 都写**,让 claude / codex 会话都能用 ccteam 工具;ccteam 是纯 CLI、不是 vendor 插件,这是唯一的 MCP 安装路径)② 设置 IM(Telegram)bot token ③ 查看偏好。

非交互形(headless / CI):

```bash
ccteam config mcp                          # 注册 / 刷新 ccteam MCP 服务(Claude + Codex 都写;等价菜单①)
ccteam config show                         # 打印当前偏好
ccteam config get fallback.on_claude_quota # 读一项偏好
ccteam config fallback.on_claude_quota codex   # 设一项偏好(off|codex)
```

偏好存 `~/.ccteam/preferences.toml`。当前支持的 key:`fallback.on_claude_quota`(`off` | `codex`,Claude 配额触顶时是否回退 Codex)。

IM token 也可手写凭证文件(见 §4);`ccteam config` 的菜单②会校验 token + 长轮询抓到你的 chat_id 后自动落盘到 `~/.ccteam/im/credentials.json`。

---

## 4. 接入 IM(Telegram / 飞书 Lark bot)

最省事走 `ccteam config` 菜单②(交互式)。要手写,在 Telegram 找 `@BotFather` → `/newbot` 拿 token,给 bot 发一条消息(群里则把 bot 拉进去发一条),再取 chat id:

```bash
BOT_TOKEN='123456:replace_me'
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"   # 在输出找 message.chat.id
```

手写本机凭证(私聊 chat id 通常正数,群聊可能负数):

```bash
mkdir -p ~/.ccteam/im
chmod 700 ~/.ccteam ~/.ccteam/im
$EDITOR ~/.ccteam/im/credentials.json
chmod 600 ~/.ccteam/im/credentials.json
```

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

`allowed_chat_ids` 是安全边界:只有列出的 chat 能触达 daemon,**生产不留空**。改完凭证必须重启 daemon 才生效。

### 飞书 / Lark bot(第二 IM 通道)

Telegram 之外可同时接入飞书/Lark(同一个 `credentials.json`,与 telegram 并存)。走**原生 WebSocket 长连接**,不需要公网域名 / 回调地址。

1. 开发者后台建应用 —— 飞书 `open.feishu.cn`(国内)或 Lark `open.larksuite.com`(国际)。
2. 开「机器人」能力;**事件订阅选「长连接(WebSocket)」模式**(不是 webhook),订阅 `im.message.receive_v1`。
3. 开通权限:`im:message`(读)+ `im:message:send_as_bot`(发)。
4. 拿到 `App ID`(`cli_...`)+ `App Secret`。

最省事同样走 `ccteam config` 菜单 —— 选「set Lark/Feishu app credentials」一项,按提示依次填 `App ID` / `App Secret` / region(F=飞书国内 / L=Lark 国际)/ allowed open_ids;它会先 live 校验 creds(取 tenant_access_token)再写盘,与 Telegram 项并存。要手写,在 `~/.ccteam/im/credentials.json` 加 `lark` 块(可与 telegram 同时存在):

```json
{
  "telegram": { "bot_token": "123456:replace_me", "allowed_chat_ids": ["123456789"] },
  "lark": {
    "app_id": "cli_replace_me",
    "app_secret": "replace_me",
    "allowed_user_ids": ["ou_replace_me"],
    "use_feishu": true
  }
}
```

- `use_feishu`:`true` = 飞书(国内 `open.feishu.cn`,默认);`false` = Lark 国际版(`open.larksuite.com`)。
- `allowed_user_ids` 是 **open_id**(`ou_...`)白名单,且**留空 = 拒绝所有人**(fail-closed,与 telegram「空=放开」相反,默认更安全);`["*"]` 放开所有人(不建议)。
- **怎么拿自己的 open_id**:先留个占位(或留空)启动 daemon,给 bot 发一条消息,在 daemon 日志里找这行 —— `Lark WS: ignoring ou_xxxx (not in allowed_users)`,`ou_xxxx` 就是你的 open_id;填回 `allowed_user_ids` 再重启。

改完凭证同样要 `ccteam stop && ccteam start` 才生效。飞书/Lark 支持文本 + 富文本(post)+ 图片/文件(image/file/audio/media)收发 —— 收到的图/文件自动落盘供 agent `Read`,`chat_send_file` 也能把图/文件发回(与 Telegram 对等)。

---

## 5. 启动 gateway daemon(`ccteam start`)

```bash
ccteam start > /tmp/ccteam.log 2>&1 &     # 一个进程:IM gateway + web chat + MCP socket + web UI
```

`ccteam start`(无参)启动常驻网关 —— **不 tick、无 orchestrator 循环**,纯路由 + 按需 spawn / resume session。同进程提供:IM gateway(Telegram 长轮询 + 出站、飞书/Lark WS)、web chat WS(`/ws/chat`)、标准资源 API(`/api/v1/*`)、MCP socket(`~/.ccteam/run/mcp.sock`)、Web UI(默认 `0.0.0.0:7331`)、hook sink。

```bash
ccteam start --web-bind 127.0.0.1:7331    # 只绑环回(此时 web 不需 token)
ccteam start --no-web                      # 只要网关,不起 web
ccteam start --no-imd                      # 只要 web,不起 IM 网关
```

```bash
# 重启(只停 daemon,不杀 tmux session;重启后按 session id 自动接回)
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

> `--web-bind` 是 `ccteam start` 的 web 地址参数;独立的内部 web 服务用 `ccteam internal web --bind`。两者别搞混。

---

## 6. 日常使用(IM 网关命令)

配对当前 chat(`<code>` 任意,如 `phone`;回 `paired <code>`):

```text
/pair phone
```

切项目、建 session:

```text
/cd demo-app                当前 chat 切到 demo-app
/cd demo-api               切另一个项目
/newproject demo /ws/demo  新建并 init 一个项目(team 前缀目录),再切过去
```

`/cd` 进一个项目后,该 chat 第一次发消息会**自动 spawn 一个 `cto` session**(`claude --agent cto`)—— 直接跟 cto 对话即可。cto 是 chat-first 管家:懂 ccteam、会**推荐**合适的 work-role、帮你串流程。本版 cto 只**推荐**,你自己用 `/role` 换角色。

**换角色干活**(把当前 session 切成另一个 role):

```text
/role reviewer    当前 session 切到 reviewer 角色(底层 = 带新 --agent 重启,保持同一 session id)
/role cto         切回管家
```

`/role` 原地换角色:同一个 session id(`/use <id>` 不失效),用新 `--agent <role>` 重启;同 role = no-op(不白扔上下文)。role 必须在项目 `.claude/agents/<role>.md` 有对应文件。

> **work-role 从哪来**:自己写 `.claude/agents/<role>.md`,或从 **ccteam 插件市场**(ccteam-hub,含 agency-agents 等开源 Claude 原生 .md 角色库,MIT)用 `ccteam role search/add` 一键装(见 §7「装 role」)或 web 控制台「插件市场」页装(见 §8);也可手动丢 .md。装完 `/role <role>` 即用。

**多 session + 切换 + 查看**:

```text
/new                        铸一个新会话(默认 vendor=claude / role=cto),回新句柄 s<N>
/new claude reviewer        建新会话(vendor = claude|codex;role 可选,默认 cto)
/new claude reviewer hitl   建一个开了「人工批准」的会话(尾 token hitl;默认是 skip)
/use s1                     切到会话 s1(同一项目里多会话间切换)
/role reviewer              把**当前**会话改成 reviewer 角色(原地重启、同句柄 s<N>)
/sessions                   列当前 chat 的会话(每行带 vendor + role + model + 上下文用量)
/projects                   列 daemon 已知项目(= 已 init 且 daemon 已加载)
```

`/new` 每次都**铸一个全新会话**(新 `s<N>`,绝不复用旧的),回一个句柄给你;`/use s<N>` 在已有会话间切。同一项目可并存任意多会话,各自独立——开两个 `reviewer` 也是两条互不串台的对话。**roleless**:IM 的 `/new` 至少给 cto;要起一个**裸 claude(无角色)**会话,用 web 控制台新建会话弹窗里的「(无角色 / 裸 claude)」选项,或资源 API `POST …/sessions` 传空 role(brain 走项目 `CLAUDE.md`)。

**人工批准模式(HITL,可选,默认关)**:`/new <vendor> <role> hitl` 建的 session 跑**非自动放行**的工具时,会先把「session sX(role) 要跑:`<tool> <摘要>`」+ `[✅ 同意] [⛔ 拒绝]` 两个按钮弹到你 chat,点同意才执行、拒绝则只挡这一次工具(不杀整个 turn)。allowlist 内 / 自动放行的工具永不弹。不带 `hitl`(默认 `skip`)的 session 维持 YOLO(直接执行,不弹)。`/role` 切换 + daemon 重启都保留 session 的批准模式。(Codex session 忽略此模式 —— 自带 sandbox。)

`/sessions` 每行形如 `s1:demo-app:Claude:reviewer — claude-opus-4-8[1m] · ctx 188k / 1M (19%)` —— 形如 `句柄:项目:vendor:role`,末尾是该 session 的 **model + 上下文用量**(绝对值 + 百分比;窗口 1M 来自 model id 的 `[1m]` 后缀,否则按 200k 基线)。用量是回合后值:空闲时准,turn 跑到一半偏旧。

**发消息 + slash 透传**(`@handle` 决定路由并设为当前 session;不带 `@` 时发给当前 session):

```text
@reviewer 看一下这个项目的 README,给我三条风险
@api /review       Codex 原生 RPC(review/start)
@api /compact      Codex 原生 RPC(thread/compact/start)
@reviewer /clear   Claude TUI slash 透传
@reviewer /model   弹出 model 选项按钮,点一下即应用(bare 弹窗型)
```

> gateway 先回 `submitted <session> turn <id>`,随后把 assistant / error 事件经同一条 outbound ledger 发回 IM。

**slash 在 IM 里按 vendor 的行为**(没有一条会静默变成发给模型的字面文本):

- **Claude session**:开放集(skill / 自定义命令 / `/compact` `/clear` `/usage` …)按字面 `send-keys` 透传给 TUI;弹窗选择型(`/model` 等)带参直接应用、不带参(bare)弹出 **inline 选项按钮**(web chat 里是 chips),点一下即应用;纯设置面板型(`/config` `/agents` 等无法用一句参数驱动的)会**显式拒绝并给提示**(不盲发、免得隐藏 TUI 卡进 modal 吞输入);万一卡住,发 `/esc` 等于按一下 Esc 把 TUI 拉回来。
- **Codex session**:每条 slash 映射 app-server **原生 RPC** 或即时查询(`/compact` `/review` `/interrupt` `/status` `/skills` …);弹窗型(`/model` `/review` `/permissions` …)bare 先弹**选项按钮**、选了再应用(两段式);`/new` `/clear` `/resume` 这类 Codex 无 in-thread 等价的,会**重定向**提示你用网关命令(如 `/new` 建新 session、`/use` 切 session);确实不支持 / TUI-only 的命令显式回执拒绝。

> 本版 role 绑定(`--agent`)是 **Claude 路径**的能力;Codex session 只保证读项目原生 `AGENTS.md`,role 绑定推后。

**turn 进行中的进度**:turn 跑的时候会有一条活的 status 消息逐步编辑,折叠显示步骤(形如 `⏳ working… · 📖 read ×5 · 🔧 bash ×3`),结束收尾成 `✅ done · n tools · m files`;最终答案单独成一条新消息(会 ping)。想关掉只发答案:daemon 起前设 `CCTEAM_IM_PROGRESS=off`(编辑节流阈值 `CCTEAM_IM_PROGRESS_THROTTLE_MS`,默认 1500ms)。

**长消息自动分片**:超过 Telegram 4096(UTF-16)上限的回答会被有序切成多条发出(代码块跨片自动闭合/重开),不再截断丢数据。

**发图 / 发文件给 bot**:在 TG 或 飞书/Lark 直接发图片或文件 + caption(如「这是报错」)→ agent 会自动 `Read` 落盘的文件(报错截图、日志都行)。TG >20MB / 飞书 >30MB 拒收。

**bot 发文件回来**:agent 调 MCP 工具 `chat_send_file(path, caption?, kind?)` 即可把文件/截图发回你绑定的 chat(零寻址参数,身份取 spawn 注入的 `CCTEAM_CHAT_{SLUG,ROLE}`;图 ≤10MB / 文件 ≤50MB,超限或不存在返回结构化 error)。配合 `screenshot`(返回 PNG 路径)即「发效果图」。

**agent 反问你**:当 agent 自己在 turn 中途要拿主意(发起 AskUserQuestion),它的问题会以**选项按钮**形式弹到你 chat 里 —— 点一下(或回数字 / 回一段自由文本)即把答案喂回 agent,它继续往下跑,不卡死。等太久没答会按兜底策略让它自决。

**给 bot 设定固定行为**走官方机制(**不靠注入**):role 的 persona 写在 `.claude/agents/<role>.md`(`/role` 换的就是它);项目知识走 vendor 原生 —— Claude 读项目 `CLAUDE.md`、Codex 读 `AGENTS.md`(都归项目自己,ccteam 不生成);全局指令放 `~/.claude/CLAUDE.md`。

```bash
$EDITOR ~/projects/demo-app/.claude/agents/reviewer.md   # 改 reviewer 角色;下次 /role reviewer(fresh start)生效
$EDITOR ~/projects/demo-app/CLAUDE.md                     # 项目知识(Claude 自动读,vendor 原生)
```

> 注:改 role.md 在 **fresh start / `/role` 切换**后生效;`/use` resume 一个已活 session 会沿用它历史里展现的 persona(in-context,不重读)。要立刻换 persona 就 `/role`。

**让 cto 调度 work-role 干活(cto dispatch)**:默认 `cto` 管家可以 spawn 一个 work-role 子 session、派任务、收结果 —— 不用你手动切来切去。这是 cto 自己在 turn 里调 MCP 工具完成的,你只要用自然语言让它做:

```text
@cto 起一个 backend-architect,让它评审 src/ 的接口设计,把结论汇总给我
```

cto 背后用的是 5 个 `mcp__ccteam__session_*` 工具:`session_spawn`(建子 session)· `session_dispatch`(派一个 turn)· `session_collect`(取子 session 的回复,polled)· `session_list` · `session_stop`。这组工具是 cto **专属**:daemon 校验每个 session 启动时注入的 secret(只有 `cto` session 持有有效 `(role, secret)`,且只能操作自己项目里的 session),work-role 调不到;只走 gateway session map。**注**:所有 agent 同 OS 用户运行,这道门是 best-effort(抬高门槛),**不是**硬隔离 —— 真正的进程隔离需要 per-agent OS 用户 / sandbox(未来版本)。

---

## 7. shell 侧项目 / session 管理(`project` / `session` / `role` 组)

日常驱动在 IM;shell 这组用于脚本 / 运维 / 无 IM 场景。

**`ccteam project`**(项目生命周期):

```bash
ccteam project ls                  # 列已知项目
ccteam project show demo-app       # 看一个项目的完整状态 + 近期事件
ccteam project new demo --team dev # 在 <projects_root>/dev-demo/ 下新建并 init
ccteam project stop demo-app       # 停该项目所有会话(走当前 mux backend 真停掉,可按 id resume;非删)
ccteam project rm demo-app         # 注销项目(只摘 config 注册 + 清 ~/.ccteam 内 per-slug 状态)
```

**删除(`project rm`)= init 的逆**:

```bash
ccteam project rm demo-app --dry-run        # 先列「会停 …」+「会删 …」,不动手
ccteam project rm demo-app --purge          # 注销 + 删 ccteam 在项目里的痕迹
ccteam project rm demo-app --purge --force  # 跳过「永不主动 kill 长 session」的确认门
```

- 不带 `--purge` = 仅注销(摘 `~/.ccteam/config.yaml` 注册项 + 清 `~/.ccteam/{progress,imd/registry}/<slug>`),项目目录里的文件原样不动。
- `--purge` = 额外删 **ccteam 建的**:项目 `.ccteam/`、种入的 `.claude/agents/cto.md`、`.claude/settings.local.json` 里 ccteam 的 hook 段。
- **永远保留不碰**:你自选的 work-role(`.claude/agents/` 里非 cto 的 .md)、项目 `CLAUDE.md` / `AGENTS.md`、`.env`、业务代码、你的 `.claude/settings.json`。
- `rm` 默认先停活动 session 再删;`--force` 跳过确认门。

> `project stop` = 停(显式用户命令,resumable,不违「永不主动 kill」红线);`project rm` = 删(先停再删)。

**`ccteam session`**(会话 + bot 配置):

```bash
ccteam session ls                          # 列活的网关 chat-mode session,并标出 orphan
ccteam session attach demo-app reviewer    # attach 到一个 chat session(role 可省,单个时自动选)
ccteam session pause demo-app              # 暂停某项目自动派工(永不 kill 长 session)
ccteam session resume demo-app
ccteam session persona demo-app reviewer - # 用 stdin 整文件替换 reviewer 的 .claude/agents/reviewer.md
ccteam session add-tool demo-app reviewer "Bash(git*)"   # 往 role 的 tools CSV 加一条
ccteam session role demo-app s1 reviewer   # 提示:换 session 的 role 走 IM /role(daemon 内存态)
```

- `session register / unregister / bots` 管 IM bot 注册(镜像 `mcp__ccteam__chat_*` / `admin_*` 工具),脚本 / 无-daemon 兜底用:

```bash
ccteam session register --slug demo-app --role reviewer --vendor claude \
  --platform telegram --chat-id 123456789 --chat-handle reviewer
ccteam session bots --slug demo-app        # 看注册表(role → @handle → platform/chat_id + running 状态)
ccteam session unregister --slug demo-app --role reviewer
```

> `session role` 是个**指针命令**:真正换 session 的 role 需要 daemon 的内存态,走 IM `/role <role>`;CLI 这条只打印指引。单 session 粒度删(`session rm`)本版未做。

**装 role(`ccteam role`)= 从 ccteam 插件市场(ccteam-hub)挑现成的 work-role**:

```bash
ccteam role search backend          # 搜插件市场 catalog(含 agency-agents 等开源 Claude 原生 role,MIT);带 --format json 可机读
ccteam role add backend-architect   # 拉取该 role 的 .md verbatim 写进当前项目 .claude/agents/
ccteam role add backend-architect --as be   # 用 --as 改落地文件名(消歧 / 起短名)
ccteam role add data-scientist --project demo-app   # 装到指定项目(默认当前目录)
ccteam role list                    # 列当前项目已装的 role(= /role 可切的)
```

- `role search` / `add` 读 **ccteam-hub 插件市场** 的 `index.json`(经 HTTPS 拉取 + 本地缓存 `~/.ccteam/hub-cache/`;首次访问联网,之后走缓存)。**官方 ccteam 插件(`source: ccteam`)在结果里置顶,其余来源依次排后。** `search` 无匹配会给提示、exit 0。
- `role add` 会取该 role 的 markdown 原文、**sha256 校验内容完整性**后写入 `.claude/agents/<role>.md`(零改写;agency 的 .md 本就 Claude 原生)。已存在同名 → 拒绝覆盖,加 `--force` 才覆盖。装完打印 `/role <role>` 提示,IM 里直接 `/role <role>` 切过去用。
- 插件内容住独立的 `firstintent/ccteam-hub`(curated marketplace),开源插件 verbatim vendor 进 hub(pinned sha)；ccteam repo 本身不带任何 role/skill 内容(唯一例外默认 `cto`)。web 控制台的「插件市场」页是同一来源的图形入口(见 §8)。

---

## 8. Web 控制台 + 标准资源 API

```bash
# token 落在文件里;ccteam start 已尝试自动复制到剪贴板(--no-clipboard 跳过)
cat ~/.ccteam/web-token
```

- 本机环回:`ccteam start --web-bind 127.0.0.1:7331` → 浏览器开 `http://127.0.0.1:7331`(无需 token)。
- LAN / 非环回 bind(默认 `0.0.0.0:7331`):自动开 token 鉴权,用上面 `~/.ccteam/web-token` 的值。

打开 `http://<host>:7331/app/chat` 进入 web chat 控制台。它和 IM 共用同一个 Gateway(同样 `/new` `/use` `/cd` `@handle` `/role`):

```text
/new claude reviewer
@reviewer 看一下当前项目
@api /review
```

Chat 面板走 `ccteam-chat.v1` WebSocket;Terminal 面板走既有 `ccteam-pty.v1`。`ccteam start --no-imd` 只启动 web server,此时 Chat 面板能打开但不会接入 Gateway;要用 web chat,保持 IM gateway task 启用(默认)。

**每个 session 独立页(per-session web)**:打开 `http://<host>:7331/app/chat/s/<sid>`(`<sid>` = `s1`/`s2`…,与 IM 的 `/use s1` 同命名空间)进入某个 session 的独立视图 —— 自己的历史(读该 session 的 `turns.jsonl`)、按 sid 过滤的实时事件流、干净不混流的切换。HITL 批准也会在这里渲染成「session sX 要跑…」+ 每个选项一个按钮(web 点击 resolve 是 best-effort,稳妥批准走 IM 按钮)。

**统一界面**:本版 web 是**一个 chat 风格外壳** —— 顶栏有面包屑 + 连接状态 + **cost pill**(今日成本 / 预算,实时);每个 session 有 **Chat | 终端** 两个 tab;底部全局导航三页 = **插件市场 / Status / Settings**(旧的多页 operator 仪表盘已收敛掉)。

**控制台页签**(浏览器里点点就能用,不必记命令):

- **新建项目**:新建会话弹窗里选「＋ 新建项目…」,填 slug(名)+ 路径即可在任意目录 scaffold 一个项目(走 `POST /api/v1/projects`),建好直接在里头起会话。
- **新建会话弹窗**:role 是从该项目 `.claude/agents/` 拉的**真实 role 下拉**(显示 role + 说明),外加一个「(无角色 / 裸 claude)」选项起 roleless 会话;不选则默认 cto。
- **插件市场页**:浏览 ccteam-hub 的 role/skill/workflow 插件(**官方 ccteam 插件置顶**,其余如 agency-agents 等开源依次),**点开看正文预览**(install 前 review),**一键装进当前项目**(sha256 校验,带「已装」状态标)。取代了旧的只读 Roles 页 —— 装完 IM 里 `/role <role>` 即用。
- **Status 页**:轻量状态总览 —— daemon 健康 + 会话 live/idle 数 + 今日成本/预算(同 `GET /api/v1/status`,也是 cost pill 的来源)。
- **Settings 页**:在浏览器里配 IM 凭证 —— Telegram(bot token + 异步抓 chat_id:存好 token 后给 bot 发条消息,页面轮询自动捕获)与 Lark/飞书(App ID / Secret / region / allowlist)。**秘密只显示掩码**(`…last4`),永不回显明文。**改完需重启 daemon 才生效**(凭证仅 daemon 启动时加载一次,无热重载)—— 页面会提示 `restart required`,照 §5 `ccteam stop && ccteam start`。
- **web 终端**(per-session):按会话解析到对应 pane,稳定连(不再秒断重连)。**本版默认 mux backend(rmux)即逐字节保真**(裸 ANSI / 光标 / 换行/对齐都对,连上回放当前屏幕),不再需要 `CCTEAM_MUX_BACKEND=tmux`。终端 UI 当前只对 claude 会话开放。

> **安全**:web 默认绑 `0.0.0.0:7331` 且**无 TLS**,token / IM 凭证走 LAN **明文**传输 —— 只在可信局域网用,**别暴露公网**;要更严就 `--web-bind 127.0.0.1:7331` 只绑环回(并用 SSH 隧道远程访问)。

**交互式 API 文档**:浏览器开 `http://<host>:7331/api/docs` 是 `/api/v1` 全量端点的 **Scalar 交互式文档**(可直接在页面里试调);机读 spec 在 `GET /api/v1/openapi.json`(OpenAPI 3.1)。两者与 `/api/v1` 同一 web-token 鉴权(非环回 bind 需带 token)。

### 标准资源 API(`/api/v1`,给集成方)

daemon 暴露一套 **web-token 鉴权**的标准资源 API(供 app / 独立前端集成;web UI 自身也基于它)。三资源 + 能力探测:

```text
GET    /api/v1/projects                      列项目
POST   /api/v1/projects                      注册项目
GET    /api/v1/projects/{slug}               单项目详情
DELETE /api/v1/projects/{slug}               注销 + 停 session(破坏性 file-purge 仍走 CLI project rm --purge)

GET    /api/v1/projects/{slug}/roles         列 role(读 .claude/agents)
GET    /api/v1/projects/{slug}/roles/{role}  读单个 role 定义
PUT    /api/v1/projects/{slug}/roles/{role}  写单个 role 定义

GET    /api/v1/projects/{slug}/sessions      列 session
POST   /api/v1/projects/{slug}/sessions      建 session(project × role × harness)
GET    /api/v1/sessions/{sid}                session 历史
POST   /api/v1/sessions/{sid}/turn           发一个 turn
GET    /api/v1/sessions/{sid}/events         按 sid 过滤的事件流(SSE)
POST   /api/v1/sessions/{sid}/stop           停 session

GET    /api/v1/marketplace                   插件市场 catalog(ccteam-hub)
GET    /api/v1/marketplace/{id}/body         插件正文预览(install 前 review)
GET    /api/v1/projects/{slug}/marketplace   catalog + 该项目「已装」状态
POST   /api/v1/projects/{slug}/marketplace/install   装一个插件进项目(sha256 校验)

GET    /api/v1/status                        daemon 健康 + 会话 live/idle + 今日 cost/budget
GET    /api/v1/capabilities                  当前可用 harness(× provider)动态列表(PATH 探测)

GET    /api/v1/openapi.json                   OpenAPI 3.1 spec(由同一套路由注册生成,防漂移)
GET    /api/docs                              Scalar 交互式 API 文档(浏览器里试调)
```

- **session-id** 命名空间 = gateway `s{n}`(与 IM 的 `/use s1` 一致)。
- **harness × provider**:harness = agentic CLI 驱动(本版 claude-code;codex best-effort;gemini-cli / grok-cli 等后续 adapter),provider = 子 facet(model)。都是 session 属性、非顶层资源,经 `GET /capabilities` 动态暴露。
- session 端点需要 live gateway(`ccteam start` 起的 daemon);独立 `internal web`(无 gateway)下 session 端点优雅返回 503。
- **DELETE `/projects/{slug}`** = 注销 + 停 session,**不**删文件树;要删 ccteam 痕迹用 CLI `project rm --purge`。

> per-session 独立 web 视图(`/app/chat/s/:sid`)已在本版前端落地(见上「每个 session 独立页」);整套 `/api/v1` + OpenAPI 文档同样 live。

---

## 9. 运维

```bash
ccteam status                  # daemon 心跳 + 每个项目(嵌套列其会话)+ 两行 web token/url
ccteam session ls              # 列网关会话(SLUG·SID·ROLE·VENDOR·STATUS),并标出 orphan
ccteam doctor                  # 安装 / 依赖体检
ccteam doctor --verify-mcp     # MCP 表面验收(active 15 / stubs 0,drift 退出码 1)
ccteam stop                    # 优雅停 daemon(保留 tmux session)
```

`ccteam status` 一眼看全:daemon 心跳 → **每个项目**(slug · age · last-event · OK/STUCK)下**嵌套列出它的会话**(role / vendor / status / sid / last-event;roleless 会话 role 显示 `-`)→ 末尾两行 web 访问信息:

```text
  web token: <hex>                                   # 裸 hex(给自己加前缀的工具)
  web url:   http://<局域网ip>:7331/?token=ccteam:<hex>   # 直接能开的完整 URL(带 ccteam: 前缀)
```

`ccteam session ls` 列网关会话,带 **VENDOR** 列;daemon 在线时 tracked 会话(含 **codex**)状态正确显示 `live`(此前 codex 会被误报 `registered, not running`);daemon 不在线降级标 `registered (daemon down)`;有 pane 活着但不在 tracked 的标 `orphan`。

成本(按 vendor 记账):

```bash
ccteam doctor --check-cost-orphan          # ledger 对账(shell 侧)
```

```text
@ccteam cost today             IM 侧 24h 成本汇总(也可 @ccteam cost <slug>)
@ccteam status                 daemon + bot 状态
@ccteam list                   列已注册 bot(@ccteam list bots / who 列本 chat 可达的)
@ccteam pause <slug>           暂停 / 恢复某项目自动派工(永不 kill 长 session;可 <slug>/<role>)
@ccteam resume <slug>
@ccteam stop <slug>            停某项目的 bot;@ccteam stop everything 停全部(危险,需回 CONFIRM 二次确认)
```

状态文件速查:

```bash
tail -120 /tmp/ccteam.log                  # daemon 日志
tail -80 ~/.ccteam/imd/outbound.jsonl      # outbound ledger(重启后重放 queued/failed)
cat ~/.ccteam/imd/gateway-state.json       # gateway session state
cat <project>/.ccteam/state.json           # 项目状态
cat <project>/.ccteam/progress.jsonl       # 业务事件(state SoT)
```

环境变量(隔离 / 覆盖路径):

```bash
CCTEAM_HOME=...                            # 隔离 daemon state / ledger / config(默认 ~/.ccteam)
CCTEAM_PROJECTS_ROOT=...                    # 默认项目根(默认 ~/projects)
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=...  # 覆盖 vendor CLI 路径
# Codex transport 单轴:默认走 `codex app-server --listen stdio://`(只需 PATH 上有 codex);
# 仅当你自管一个常驻 app-server daemon 时,设此 env 指向其 UDS 走 socket 覆盖:
CCTEAM_CODEX_APP_SERVER_SOCKET=/path/to/app-server-control.sock
```

> `~/.ccteam` 规范布局由 `ccteam-core::canonical_home_dirs()` 单一定义(hooks / progress / run / state);`ccteam doctor` 会报告 home-layout drift。

---

## 10. 卡住时(最常见 5 条)

```bash
# 先跑这三条,八成能定位
ccteam doctor
ccteam status
tail -120 /tmp/ccteam.log
```

1. **`ccteam: command not found`** — `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"`。
2. **Telegram 不回 / `drop msg from non-allowed chat`** — chat id 不在 allowlist,或改了凭证没重启:编辑 `~/.ccteam/im/credentials.json` 的 `allowed_chat_ids`(或重跑 `ccteam config`)→ `ccteam stop && ccteam start`。
3. **IM 收到 `发送失败: ... 下一步: ...`(pane 死 / socket 断)** — `ccteam stop && ccteam start`,再发同一 `@handle`;仍失败用 `/new claude reviewer2` 开新 session。
4. **收到 `会话暂时没有产出: ... 下一步: ...` 或超时提示** — 等一会重试;长上下文先 `@bot /compact`;反复超时就新建 session。
5. **`/cd` / `/new` 报 `项目不存在: ... 下一步: ...`** — 项目没 init 或 daemon 没加载:`cd <repo> && ccteam init` → `ccteam stop && ccteam start` → IM 里 `/projects` 确认 → `/cd <s>`。

> IM 路径里的 Claude session 默认 `skip`(`--dangerously-skip-permissions`,YOLO 模式、无批准门)——只把 bot 暴露给可信 chat,bot token 不进 git。要逐工具人工批准,用 `/new <vendor> <role> hitl` 起一个 HITL session(见 §6「人工批准模式」)。
