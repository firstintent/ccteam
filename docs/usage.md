# ccteam 使用手册

**ccteam —— 自托管、7×24 常驻的后台智能体团队:从网页端、Telegram、飞书远程驱动你机器上的 Claude Code / Codex。**

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
- **role** = 会话启动时绑定的角色(`.claude/agents/<role>.md` 里的 persona + 工具)。默认角色是 `cto`(懂 ccteam 的管家);也可以无角色 = 裸 Claude(自读项目 `CLAUDE.md`)。

> **ccteam 只管自己的东西。** 它从不修改你的业务代码、`.git/`、`.env`,也不改写你的 `CLAUDE.md` / `AGENTS.md` —— 这些都归项目自己,Claude 和 Codex 原生读取。

---

## 开始之前:装好 + 起服务

这是唯一需要在终端里做的两步;做完之后,推荐全程用网页控制台。

### 1. 安装

ccteam 调用你机器上**已装好并登录的** Claude Code(必需)/ Codex(可选),自己不打包它们。

```bash
# 推荐:用 cargo 从源码装(需要 Rust 工具链 + Node.js 用于 web 控制台打包)
cargo install --git https://github.com/firstintent/ccteam ccteam-cli

# 备用:预编译二进制(无需工具链;linux + macOS arm/x64,Windows 走 WSL2)
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

ccteam --version
claude --version   # 必需,需要时按提示登录
codex --version    # 可选,用 Codex 会话才需要
```

> 若提示 `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"` 后重开终端。

### 2. 起服务

`ccteam start` 启动唯一的常驻进程 —— 同时提供 Web 控制台、IM 网关、标准资源 API 和 MCP。预编译安装脚本会问你要不要装成 systemd 服务或后台启动;手动起:

```bash
nohup ccteam start >~/ccteam.log 2>&1 &
```

启动后日志(或随时 `ccteam status`)会打印 Web 控制台地址,形如:

```text
web url:   http://<你的局域网IP>:7331/?token=ccteam:<令牌>
```

**点这个链接就进控制台了** —— 下面所有操作都在里面。

---

## 一、Web 控制台(推荐)

打开 `ccteam start` 给出的链接即可。控制台是一个聊天风格的界面:顶栏有当前位置、连接状态和**实时成本**(今日花费/预算);底部四个全局页 = **插件市场 / Status / 主机 / Settings**。左上头像里可切**中英文界面**、**明暗主题**和登出。

> **访问与安全**:默认绑 `0.0.0.0:7331`(局域网可访问)并用令牌鉴权,令牌存在 `~/.ccteam/secrets/web-token`。Web **无 TLS、明文传输**,请只在可信局域网用,**不要暴露公网**。要更严:`ccteam start --web-bind 127.0.0.1:7331` 只绑本机(此时免令牌),远程用 SSH 隧道。

### 注册 MCP(一次性,让 agent 能用 ccteam 的能力)

进 **主机** 页,点 **「注册 ccteam MCP」**。这一步把 ccteam 自己的工具(派活、发文件、截图等)写进 Claude / Codex 配置,会话才能调用它们。主机页还显示这台机器上 Claude / Codex 装没装、版本、是否就绪。

### 创建项目

在新建会话弹窗里选 **「＋ 新建项目…」**,填 slug(短名)和目录路径,即可把任意目录登记成项目并在其中开会话。同名不同路径会自动累加为 `demo2` / `demo3`。

### 开会话、切换、对话

- **新建会话**:选 vendor(Claude / Codex)和角色。角色是从项目 `.claude/agents/` 读出的真实下拉列表,另有「(无角色 / 裸 Claude)」选项;不选则默认 `cto`。建好回一个句柄 `s<N>`。
- **每个会话**有 **Chat | 终端** 两个标签页。Chat 里助手消息按 Markdown 渲染(标题/列表/表格/代码块,代码块一键复制);输入框 **Enter 发送、Shift+Enter 换行**,发送中可一键停止。
- **独立会话页**:`/app/chat/s/<sid>`(`<sid>` 与各入口的 `s1`/`s2` 同一命名空间)是某个会话的干净视图 —— 自己的历史、按会话过滤的实时事件,不与别的会话混流。
- **终端标签页**:逐字节保真地镜像会话屏幕(ANSI / 光标 / 对齐都对)。当前只对 Claude 会话开放。
- **历史会话与恢复**:会话列表下点「更多历史 (N) ▸」展开已**停止但未销毁**的会话(灰显)。点任意一个即从磁盘 `meta.json` **冷恢复**(cold-resume) —— 停止的会话、甚至 daemon 重启前的会话都不丢,随时可恢复(手机上 `/use <sid>` 同样能恢复)。「+ 导入历史会话」对话框还能发现你在 ccteam 之外用原生 `claude` 跑过的会话(按工作目录内容匹配),一键**收编**成普通 ccteam 会话,对话原文保留。

> 部分高级选项(会话协议、在 Web 里选角色、历史会话恢复与导入)目前仅对管理员开放,普通用户默认用标准 Claude / Codex 会话;随功能稳定会逐步放开。

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
- 成本按 vendor(Claude / Codex)分别记账。

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
/cd <project>              切到某个项目(进项目后第一条消息自动起一个 cto 管家)
/projects                  列出已知项目
/newproject <slug> <path>  新建并注册一个项目,再切过去

# 会话
/new [vendor] [role] [hitl]  新建会话 → 回一个句柄 s<N>
                             · vendor = claude(默认)| codex
                             · 省略 role = 裸 claude(自读项目 CLAUDE.md);写 role 则绑定该角色
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
@ccteam <verb>   管理:status · cost [today] · list · bots · pause / resume / stop <slug>[/role] · confirm
```

### 直接对话 + 收发文件

- **不带前缀的消息** → 发给当前会话。
- **非网关的 `/命令`**(`/compact`、`/clear`、`/model` …)→ 透传给当前 agent;弹窗型(如 `/model`)会弹**选项按钮**,点一下即应用。
- **发图 / 发文件 + 一句说明** → agent 自动读取(报错截图、日志都行);agent 也能把文件 / 截图发回你的 chat。
- **回合进行中** → 一条活的进度消息(形如 `⏳ working… · 🔧 bash ×3`),最终答案单独成条(会提醒);超长回答自动分片;agent 中途要你拿主意时会弹**选项按钮**,点一下喂回答案、它继续往下跑。

### 人工批准(HITL)

默认会话是「直接执行」(`skip`)。用 `/new <vendor> <role> hitl` 起一个需审批的会话:它跑非自动放行的工具前,会把「要跑什么」+ `[✅ 同意] [⛔ 拒绝]` 发到你 chat,点同意才执行,拒绝只挡这一次(不杀整个回合)。Codex 会话自带 sandbox,忽略此模式。

### 让 cto 派活

默认 `cto` 管家能自己起 work-role 子会话、派任务、收结论 —— 不用你手动切来切去,直接用自然语言交代:

```text
@cto 起一个 backend-architect,评审 src/ 的接口设计,把结论汇总给我
```

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

`ccteam init` 只写 ccteam 自己的东西:项目 `.ccteam/`(状态)、`.claude/agents/cto.md`(默认角色)、`.claude/settings.local.json`(ccteam 的 hook,写进本地层,**不碰**你的 `.claude/settings.json`)。重跑安全。偏好存 `~/.ccteam/preferences.toml`(目前一个键:`fallback.on_claude_quota` = `off`|`codex`,Claude 配额触顶是否回退 Codex)。

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

`rm --purge` 只删 ccteam 建的(项目 `.ccteam/`、种入的 `cto.md`、settings.local.json 里 ccteam 的 hook 段);**永远保留**你的 work-role、`CLAUDE.md`/`AGENTS.md`、`.env`、业务代码、你的 `.claude/settings.json`。

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
ccteam doctor --verify-mcp     # MCP 表面验收(active 15 / stub 0,漂移退出码 1)
ccteam doctor --check-cost-orphan   # 成本 ledger 对账
```

重启(只停 daemon,重启后按会话 id 自动接回):

```bash
ccteam stop && nohup ccteam start >~/ccteam.log 2>&1 &
```

状态文件速查(`~/.ccteam` 按职责分组:`secrets/` 凭证、`state/` daemon 写的、`cache/` 可删、`run/` 套接字):

```bash
tail -120 ~/ccteam.log                          # daemon 日志(看你重定向到哪)
cat ~/.ccteam/config.yaml                        # 项目登记(slug → 路径)
cat ~/.ccteam/state/im/gateway-state.json        # 网关会话状态
tail ~/.ccteam/state/im/outbound.jsonl           # 出站 ledger(重启重放)
cat <project>/.ccteam/progress.jsonl             # 项目业务事件(状态权威)
```

环境变量:

```bash
CCTEAM_HOME=~/.ccteam2          # 隔离一整套状态/配置/会话(配合 ccteam --home 跑多实例)
CCTEAM_PROJECTS_ROOT=...        # 默认项目根(默认 ~/projects)
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=...   # 覆盖 vendor CLI 路径
```

---

## 排错(卡住时)

先跑这三条,八成能定位:

```bash
ccteam doctor
ccteam status
tail -120 ~/ccteam.log
```

1. **`ccteam: command not found`** — `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"`。
2. **Telegram 不回 / 日志 `drop msg from non-allowed chat`** — chat id 不在白名单,或改了凭证没重启:修 `~/.ccteam/secrets/im-credentials.json` 的 `allowed_chat_ids`(或重配 Web Settings)→ 重启 daemon。
3. **IM 报「发送失败 / 会话暂时没有产出」** — 重启 daemon 再发同一 `@handle`;长上下文先 `@bot /compact`;反复失败就 `/new` 开新会话。
4. **`/cd` / `/new` 报「项目不存在」** — 项目没初始化或 daemon 没加载:`cd <repo> && ccteam init` → 重启 daemon → `/projects` 确认 → `/cd <slug>`。
5. **Web 打不开 / 要令牌** — 用 `ccteam status` 末尾的完整 `web url`(带令牌);或 `--web-bind 127.0.0.1:7331` 只绑本机免令牌。

> IM 路径里的 Claude 会话默认 `skip`(直接执行、无批准门)—— 只把 bot 暴露给可信 chat,bot token 不要进 git。要逐工具审批,用 `/new <vendor> <role> hitl`。
