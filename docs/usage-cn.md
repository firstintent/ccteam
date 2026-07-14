# ccteam 使用手册

**ccteam —— 自托管、7×24 常驻的后台智能体团队:从网页端、Telegram、飞书远程驱动你机器上的 Claude Code / Codex / Grok Build / OpenCode。**

你装一次、起一个常驻进程,之后所有日常操作都在三个入口里完成,**推荐程度从高到低**:

| 入口 | 适合 | 章节 |
|---|---|---|
| 🖥️ **Web 控制台** | 创建项目、开会话、装插件、配 IM、看状态 —— 点点就能用,**首选** | [一、Web 控制台](#一web-控制台推荐) |
| 💬 **Telegram / 飞书** | 手机上随时收发、驱动会话、审批工具 | [二、Telegram / 飞书](#二telegram--飞书) |
| ⌨️ **命令行(CLI)** | 脚本、运维、无图形界面的高级场景 | [三、命令行](#三命令行高级) |

---

## 核心概念

- **chat** = 一个对话入口(一个网页控制台标签、一个 Telegram/飞书私聊或群)。每个 chat 有自己的当前项目、当前会话和会话列表,互相隔离。
- **project** = 一个本地代码目录,用 slug(短名)标识。
- **session** = 一个独立的 agent 会话(像 Claude Code 原生会话一样自带上下文),属于某个项目。一个项目可同时开多个会话、互不串台,每个有持久句柄 `s<N>`(扛重启、不复用)。
- **role** = 会话启动时**可选**绑定的角色(`.claude/agents/<role>.md` 里的 persona + 工具)。默认 **roleless**:裸 vendor 自读项目的 `CLAUDE.md`/`AGENTS.md`。persona 从插件市场装或自建,ccteam 不内置任何角色。

> **ccteam 只管自己的东西。** 它从不修改你的业务代码、`.git/`、`.env`,也不改写你的 `CLAUDE.md` / `AGENTS.md` —— 这些都归项目自己,Claude 和 Codex 原生读取。

---

## 开始之前:装好 + 起服务

这是唯一需要在终端里做的两步;做完之后,推荐全程用网页控制台。

### 1. 安装

ccteam 调用你机器上**已装好并登录的** Claude Code(必需)/ Codex(可选),自己不打包它们。

```bash
# 推荐:从源码构建并装成服务(需要 Rust 工具链 + Node.js 用于 web 控制台打包)
git clone https://github.com/firstintent/ccteam && cd ccteam
make install

# 备用:预编译二进制(无需工具链;linux + macOS arm/x64,Windows 走 WSL2;同样会问装 systemd)
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

ccteam --version
claude --version   # 必需,需要时按提示登录
codex --version    # 可选,用 Codex 会话才需要
grok --version     # 可选,用 Grok Build 会话才需要
opencode --version  # 可选,用 OpenCode 会话才需要
```

> 若提示 `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"` 后重开终端。

### 2. 服务

`make install` 已经把服务起好了:唯一的常驻进程(Web 控制台 + IM 网关 + 标准资源 API + MCP)在 Linux 由 systemd `--user` 托管、在 macOS 由 launchd agent 托管 —— 开机/登录自启、崩溃自动重启、退出登录也不死。两个平台都用 `make daemon-status` / `daemon-logs` / `daemon-restart` / `daemon-stop` 管理(macOS 日志在 `~/.ccteam/daemon.log`)。卸载:源码装用 `make uninstall`、预编译装用 `install.sh --uninstall`,都会停掉并删除服务和二进制,但保留 `~/.ccteam`。没有任何 supervisor 的环境用 `ccteam start` 前台跑。

`make install` 结束时(或随时 `ccteam status`)会打印 Web 控制台地址,形如:

```text
web url:   http://<你的局域网IP>:7331/?token=ccteam:<令牌>
```

**点这个链接就进控制台了** —— 下面所有操作都在里面。

---

## 一、Web 控制台(推荐)

打开 `ccteam start` 给出的链接即可。控制台是**无全宽顶栏**的聊天壳:**可折叠侧栏**(⌘K 搜索、新建会话、工作流、会话列表),成本和头像在侧栏底部。**工作流**含 Skills / Roles / MCP / 自进化(只读) / Compare。**设置**收编主机 / 插件市场 / Status / IM 凭据。主题**默认浅色**(可切深色)。

> **访问与安全**:默认绑 `0.0.0.0:7331`(局域网可访问)并用令牌鉴权,令牌存在 `~/.ccteam/secrets/web-token`。Web **无 TLS、明文传输**,请只在可信局域网用,**不要暴露公网**。要更严:`ccteam start --web-bind 127.0.0.1:7331` 只绑本机(此时免令牌),远程用 SSH 隧道。

### 注册 MCP(一次性,让 agent 能用 ccteam 的能力)

进 **主机** 页,点 **「注册 ccteam MCP」**。这一步把 ccteam 自己的工具(派活、发文件、截图等)写进 Claude / Codex 配置,会话才能调用它们。主机页还显示这台机器上 Claude / Codex 装没装、版本、是否就绪。

### 创建项目

在新建会话弹窗里选 **「＋ 新建项目…」**,填 slug(短名)和目录路径,即可把任意目录登记成项目并在其中开会话。同名不同路径会自动累加为 `demo2` / `demo3`。

### 开会话、切换、对话

- **新建会话**:选 vendor(Claude / Codex / Grok / OpenCode)与协议(stream-json / terminal 仅 Claude 管理员 / ACP=Grok·OpenCode)、可选力度、spawn 前 HITL 开关。**执行主机 = 项目绑定的主机**(会话跟项目走,不再按会话选);每行会话带厂商标记。角色列表来自项目 `.claude/agents/`(管理员可选);租户默认 roleless。建好回句柄 `s<N>`。
- **每个会话**有 **Chat | 终端** 两个标签页。Chat 里助手消息按 Markdown 渲染(标题/列表/表格/代码块,代码块一键复制);输入框 **Enter 发送、Shift+Enter 换行**,发送中可一键停止。
- **独立会话页**:`/app/chat/s/<sid>`(`<sid>` 与各入口的 `s1`/`s2` 同一命名空间)是某个会话的干净视图 —— 自己的历史、按会话过滤的实时事件,不与别的会话混流。
- **终端标签页**:逐字节保真地镜像会话屏幕(ANSI / 光标 / 对齐都对)。当前只对 Claude 会话开放。
- **历史会话与恢复**:会话列表下点「更多历史 (N) ▸」展开已**停止但未销毁**的会话(灰显)。点任意一个即从磁盘 `meta.json` **冷恢复**(cold-resume) —— 停止的会话、甚至 daemon 重启前的会话都不丢,随时可恢复(手机上 `/use <sid>` 同样能恢复)。「+ 导入历史会话」对话框还能发现你在 ccteam 之外用原生 `claude` 跑过的会话(按工作目录内容匹配),一键**收编**成普通 ccteam 会话,对话原文保留。

> 部分高级选项(terminal/rmux 协议、在 Web 里选角色、历史会话恢复与导入)目前仅对管理员开放,普通用户默认用标准 Claude / Codex / Grok 聊天会话;随功能稳定会逐步放开。

### 插件市场:装角色 / 技能 / 工作流

**插件市场** 页浏览 [ccteam-hub](https://github.com/firstintent/ccteam-hub) 的精选插件(官方插件置顶,其余如 [agency-agents](https://github.com/wshobson/agents)、[mattpocock/skills](https://github.com/mattpocock/skills) 等开源库依次)。点开看正文预览,**一键装进当前项目**(下载时校验 sha256,带「已装」标记)。装完在任意入口 `/role <角色>` 即可切换使用。

### 配置 Telegram / 飞书

进 **Settings** 页填 IM 凭证:

- **Telegram**:粘贴 bot token,保存后给 bot 发一条消息,页面会自动轮询抓到你的 chat_id。
- **飞书 / Lark**:填 App ID / App Secret / 区域(飞书国内 / Lark 国际)/ 允许的用户。

秘密只显示掩码(`…末四位`),永不回显明文。**改完需重启 daemon 才生效**(凭证仅在启动时加载),页面会提示 `restart required` —— 照 [运维](#运维) 重启即可。详细的 bot 创建步骤见 [二、Telegram / 飞书](#接入)。

### 多用户

一台机器、一个 daemon 给多人用(同一系统账号下是**软隔离**,是 UX 边界、非安全边界):

- 管理员在 **Settings → 用户管理** 建用户,得到一次性个人登录链接发给对方;对方打开即以自己身份登录,**只看到自己的项目和会话**。
- 每个用户在自己的 **Settings →「我的 IM bot」** 填**自己的** bot token,保存即校验、**即时生效不必重启**;这个 bot 只驱动该用户自己的会话。**每个 bot 的 token 必须各不相同**。

### Status / 成本

- **Status** 页:daemon 健康、会话 live/idle 数、每条会话的成本、今日成本/预算(也是顶栏成本药丸的来源)。
- 成本按 vendor 分别记账。Claude/Codex/Grok 有价表时用表计价;**OpenCode 只认自报 USD**(无上报或 0 显示「—」,绝不套用他家价表)。

### 标准资源 API(给集成方)

控制台本身就建立在一套 **令牌鉴权的 HTTP API** 之上,你也可以直接用它做集成:

- 交互式文档:浏览器开 `http://<host>:7331/api/docs`(Scalar,可直接试调);机读 spec 在 `/api/v1/openapi.json`。
- 资源:`/api/v1/projects`、`…/projects/{slug}/sessions`、`/sessions/{sid}/{turn,events,stop}`、`/marketplace`、`/status`、`/hosts`、`/capabilities`。
- 鉴权与 Web 同一令牌;会话类端点需要 daemon 在线。

---

## 二、Telegram / 飞书

把 ccteam 接到 IM 后,你就能在手机上随时驱动会话、收发文件、审批工具。最省事是在 [Web 控制台 Settings](#配置-telegram--飞书) 里配;也可以用 `ccteam config` 菜单,或手写凭证文件。

### 接入

**Telegram**:在 Telegram 找 `@BotFather` → `/newbot` 拿 token。配置三种方式任选其一:

1. **Web**(推荐):Settings 页填 token,自动抓 chat_id。
2. **CLI 交互**:`ccteam config` → 选「设置 IM bot token」,会校验 token 并自动抓 chat_id 落盘。
3. **手写凭证文件** `~/.ccteam/secrets/im-credentials.json`(目录 0700、文件 0600):

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

`allowed_chat_ids` 是安全边界:只有列出的 chat 能触达 daemon,**生产不要留空**。拿 chat id:给 bot 发条消息后 `curl -s "https://api.telegram.org/bot<token>/getUpdates"` 在输出里找 `message.chat.id`。

**飞书 / Lark**(可与 Telegram 并存,走原生 WebSocket 长连接,无需公网回调):在开发者后台(飞书 `open.feishu.cn` / Lark `open.larksuite.com`)建应用 → 开「机器人」+ **事件订阅选「长连接(WebSocket)」** 订阅 `im.message.receive_v1` → 开权限 `im:message` + `im:message:send_as_bot` → 拿 App ID(`cli_…`)+ App Secret。配置同样走 Web Settings / `ccteam config`,或在凭证文件加 `lark` 块:

```json
{
  "lark": {
    "app_id": "cli_replace_me",
    "app_secret": "replace_me",
    "allowed_user_ids": ["ou_replace_me"],
    "use_feishu": true
  }
}
```

- `use_feishu`:`true` = 飞书(国内),`false` = Lark(国际)。
- `allowed_user_ids` 是 open_id(`ou_…`)白名单,**留空 = 拒绝所有人**(fail-closed,比 Telegram 更安全)。拿自己的 open_id:先留空启动,给 bot 发条消息,在日志里找 `ignoring ou_xxxx (not in allowed_users)`,把 `ou_xxxx` 填回去。

> **手写凭证文件后必须重启 daemon 才生效**(Web Settings 配的同理)。飞书/Lark 与 Telegram 对等:文本、富文本、图片/文件收发都支持。

### 网关命令

聊天框里发这些命令,由网关直接处理。随时 `/help` 看清单(Telegram 里敲 `/` 也会弹候选)。

```text
# 项目
/cd <project>              切到某个项目(进项目后第一条消息自动起一个 roleless 会话)
/projects                  列出已知项目
/newproject <slug> <path>  新建并注册一个项目,再切过去

# 会话
/new [vendor] [role] [hitl]  新建会话 → 回一个句柄 s<N>
                             · vendor = claude(默认)| codex | grok | opencode
/compare <问题>              多 vendor 同题并行对比
                             · 省略 role = 裸 claude(自读项目 CLAUDE.md);写 role 则绑定该角色
                             · grok = 无角色 ACP 会话(忽略 role/hitl 参数)
                             · 尾加 hitl = 工具在 IM 里逐个批准(默认 skip = 直接跑)
/use <id>                  切到会话 s<N>(已停止的会话会自动从磁盘冷恢复)
/role <role>               把当前会话换成另一个角色(原地重启,句柄 s<N> 不变)
/interrupt [id]            打断正在跑的回合,保留会话(省略 id = 当前)
/stop <id>                 销毁一个会话
/screen [id]               截图一个会话的当前屏幕(省略 id = 当前)

# 查看 / 接入
/sessions [all]            列当前项目的会话(带 vendor · role · model · 上下文用量);`all` = 跨所有项目
/status                    全队健康:每个会话 idle / working / stuck + model · ctx
/help                      列出网关命令
```

### 寻址

```text
@<role>          切到该角色的会话并设为当前(单独 @role 只切换,不发消息)
@<role> <消息>    切到它并发一条
```

`@` 永远指向一个会话。确定性控制走上面的斜杠命令面(`/status` `/sessions` `/stop` …);自由形式的运维问题("今天哪个项目烧钱最多?")直接跟会话聊 —— 任何会话都能用 ccteam 的 MCP 工具回答。

### 直接对话 + 收发文件

- **不带前缀的消息** → 发给当前会话。
- **非网关的 `/命令`**(`/compact`、`/clear`、`/model` …)→ 透传给当前 agent;弹窗型(如 `/model`)会弹**选项按钮**,点一下即应用。
- **发图 / 发文件 + 一句说明** → agent 自动读取(报错截图、日志都行);agent 也能把文件 / 截图发回你的 chat。
- **回合进行中** → 一条活的进度消息(形如 `⏳ working… · 🔧 bash ×3`),最终答案单独成条(会提醒);超长回答自动分片;agent 中途要你拿主意时会弹**选项按钮**,点一下喂回答案、它继续往下跑。

### 人工批准(HITL)

默认会话是「直接执行」(`skip`)。用 `/new <vendor> <role> hitl` 起一个需审批的会话:它跑非自动放行的工具前,会把「要跑什么」+ `[✅ 同意] [⛔ 拒绝]` 发到你 chat,点同意才执行,拒绝只挡这一次(不杀整个回合)。Codex 会话自带 sandbox,忽略此模式。Grok 会话本版仅支持 `skip`(自动放行);IM 审批已规划但尚未接入。

### 让任何会话派活

每个会话都能通过 `mcp__ccteam__session_*` 工具雇同事、派任务、收结论 —— 不用你手动切来切去,也不需要特殊角色,直接用自然语言交代:

```text
起一个 codex 会话,按 docs/rfc-12.md 实现,测试全过后汇报给我
```

这套编排面的深度参考(逐工具、身份模型、多机语义)见 [orchestration-cn.md](orchestration-cn.md)。

---

## 三、命令行(高级)

日常用 Web / IM 即可。CLI 适合脚本、运维、无图形界面的场景。命令分两组:扁平的 `init / config / start / stop / status / doctor`,和分组的 `project / session / role`。

### 安装期 / 服务命令

```bash
ccteam init                    # 在当前目录就地初始化一个项目(slug = 目录名)
ccteam init --in /path/to/repo # 在别处初始化
ccteam init --slug demo        # 覆盖自动推断的 slug
ccteam init --owner user:u123  # 多用户:把项目归属给某租户
ccteam config                  # 一次性配置:① 注册 MCP ② 配 IM bot ③ 偏好(交互菜单)
ccteam config mcp              # 仅注册/刷新 ccteam MCP(给 Claude + Codex 都写;无 TTY 用这个)
ccteam start                   # 起常驻服务(见「开始之前」;加 & 后台跑)
ccteam start --web-bind 127.0.0.1:7331   # 只绑本机(免令牌)
ccteam start --no-web | --no-imd         # 只要网关 / 只要 web
ccteam stop                    # 优雅停 daemon
ccteam status                  # daemon 心跳 + 各项目及其会话 + web 链接
ccteam doctor                  # 安装 / 依赖体检(--verify-mcp 校验 MCP 表面)
```

`ccteam init` 只写 ccteam 自己的东西:项目 `.ccteam/`(状态)+ `.claude/settings.local.json`(ccteam 的 hook,写进本地层,**不碰**你的 `.claude/settings.json`)——**不种任何角色**,`.claude/agents/` 归你。重跑安全。偏好存 `~/.ccteam/preferences.toml`(目前一个键:`fallback.on_claude_quota` = `off`|`codex`,Claude 配额触顶是否回退 Codex)。

### `project`(项目生命周期)

```bash
ccteam project ls                  # 列已知项目
ccteam project show demo           # 项目完整状态 + 近期事件
ccteam project new demo --team dev # 在 <projects_root>/dev-demo/ 下新建并 init
ccteam project stop demo           # 停该项目所有会话(可按 id 恢复;非删除)
ccteam project rm demo             # 注销项目(仅摘登记 + 清 ccteam 状态)
ccteam project rm demo --dry-run   # 先预览会停什么、删什么
ccteam project rm demo --purge     # 注销 + 删 ccteam 在项目里建的痕迹
```

`rm --purge` 只删 ccteam 建的(项目 `.ccteam/` + settings.local.json 里 ccteam 的 hook 段);**永远保留**你的 work-role、`CLAUDE.md`/`AGENTS.md`、`.env`、业务代码、你的 `.claude/settings.json`。

### `session`(会话 + bot 注册)

```bash
ccteam session ls                          # 列网关会话(SLUG·SID·ROLE·VENDOR·STATUS),标出 orphan
ccteam session attach demo reviewer        # attach 到一个会话
ccteam session pause demo / resume demo    # 暂停 / 恢复某项目派工(永不 kill 长会话)
ccteam session persona demo reviewer -     # 用 stdin 整文件替换某角色的 .md
ccteam session add-tool demo reviewer "Bash(git*)"   # 给角色加一条工具
ccteam session register / bots / unregister …        # 脚本/无 daemon 时管 bot 注册
```

> 换某会话的角色走 IM `/role <role>`(需要 daemon 内存态);CLI 的 `session role` 只打印这条指引。

### `role`(从插件市场装角色)

```bash
ccteam role search backend         # 搜插件市场(官方插件置顶;--format json 可机读)
ccteam role add backend-architect  # 拉取该角色 .md(sha256 校验)写进当前项目 .claude/agents/
ccteam role add data-scientist --project demo   # 装到指定项目
ccteam role list                   # 列当前项目已装角色(= /role 可切的)
```

读 ccteam-hub 的目录(HTTPS + 本地缓存 `~/.ccteam/cache/hub/`),从上游仓库 @固定提交拉取、校验 sha256 后写入,已存在不覆盖(`--force` 才覆盖)。多文件 skill 整目录落 `.claude/skills/<id>/`。Web 控制台插件市场页是同一来源的图形入口。

### 运维

```bash
ccteam status                  # daemon + 项目/会话 + 末尾两行 web token/url
ccteam session ls              # 网关会话状态(daemon 离线降级标注)
ccteam doctor --verify-mcp     # MCP 表面验收(8 工具 / 0 stub,漂移退出码 1)
ccteam doctor --check-cost-orphan   # 成本 ledger 对账
```

重启(只停 daemon,重启后按会话 id 自动接回):

```bash
systemctl --user restart ccteam    # 或 make daemon-restart(先重编译再重启)
```

状态文件速查(`~/.ccteam` 按职责分组:`secrets/` 凭证、`state/` daemon 写的、`cache/` 可删、`run/` 套接字):

```bash
journalctl --user -u ccteam -n 120               # daemon 日志(systemd journal;或 make daemon-logs)
cat ~/.ccteam/config.yaml                        # 项目登记(slug → 路径)
cat ~/.ccteam/state/gateway/routing.json         # 网关路由(每个 chat 的当前项目/会话 + live 集)
cat ~/.ccteam/state/sessions/next-sid            # 单调 sid 计数(永不复用)
cat <project>/.ccteam/chat/<sid>/meta.json       # 会话描述(SoT:vendor/role/owner/uuid…)
tail ~/.ccteam/state/im/outbound.jsonl           # 出站 ledger(重启重放)
cat <project>/.ccteam/progress.jsonl             # 项目业务事件(状态权威)
```

环境变量:

```bash
CCTEAM_HOME=~/.ccteam2          # 隔离一整套状态/配置/会话(配合 ccteam --home 跑多实例)
CCTEAM_PROJECTS_ROOT=...        # 默认项目根(默认 ~/projects)
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=... CCTEAM_GROK_BIN=... CCTEAM_OPENCODE_BIN=...
# 覆盖 vendor CLI 路径
```

### 多机(卫星节点)

每台机器都跑同一个 `ccteam start`。join 过另一台 daemon 的节点就是它的**卫星**——之后由卫星**主动出站**连接 daemon(反向连接):只有 daemon 需要可达地址/端口(`:7331`),卫星零暴露、在 NAT/防火墙后也能用。给 daemon 前置 HTTPS 反代即可全链路 wss。

```bash
# daemon 侧(或 web 控制台 → 主机页):铸 join token
ccteam host mint-token --daemon http://daemon-host:7331 --web-token <admin-hex>

# 卫星侧(任何跑着 ccteam start 的机器):
ccteam host join --daemon http://daemon-host:7331 --token <join-token>
# 本机运行中的 ccteam start 30 秒内自动拨出上线。

ccteam host ls                     # 查看本机卫星凭据(如已 join)
```

卫星每 ~25s 经控制通道上报 agent 探测与已注册项目;主机页实时显示在线状态。**项目绑定主机** —— 要在卫星上跑会话,先让它拥有一个项目,然后往那个项目里 spawn(不再按会话选主机):

- **远程新建**:web 控制台 → 新建项目 → 主机选择器选那台卫星 → 填它机器上的绝对路径,daemon 请卫星就地 bootstrap 并注册。
- **接入既有 checkout**:在那台机器仓库里 `ccteam init`,然后主机页对上报的项目点「接入」。slug 撞名会得到独立 catalog slug(`demo` → `demo2`)——跨机 slug 相同不代表同一项目。

远程执行当前支持 Claude stream-json 会话;连接断了自动退避重连,exec 链路断开后下次 spawn 经 vendor `--resume` 续上下文。舰队容量:daemon 全局最多 50 个 live 会话(`sessions.max_live` 可配);超限时优雅挤停最久无活动的 idle 会话,被停会话可随时恢复。

---

## 排错(卡住时)

先跑这三条,八成能定位:

```bash
ccteam doctor
ccteam status
journalctl --user -u ccteam -n 120
```

1. **`ccteam: command not found`** — `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"`。
2. **Telegram 不回 / 日志 `drop msg from non-allowed chat`** — chat id 不在白名单,或改了凭证没重启:修 `~/.ccteam/secrets/im-credentials.json` 的 `allowed_chat_ids`(或重配 Web Settings)→ 重启 daemon。
3. **IM 报「发送失败 / 会话暂时没有产出」** — 重启 daemon 再发同一 `@handle`;长上下文先 `@bot /compact`;反复失败就 `/new` 开新会话。
4. **`/cd` / `/new` 报「项目不存在」** — 项目没初始化或 daemon 没加载:`cd <repo> && ccteam init` → 重启 daemon → `/projects` 确认 → `/cd <slug>`。
5. **Web 打不开 / 要令牌** — 用 `ccteam status` 末尾的完整 `web url`(带令牌);或 `--web-bind 127.0.0.1:7331` 只绑本机免令牌。

> IM 路径里的 Claude 会话默认 `skip`(直接执行、无批准门)—— 只把 bot 暴露给可信 chat,bot token 不要进 git。要逐工具审批,用 `/new <vendor> <role> hitl`。
