# ccteam 使用手册

**ccteam 把你已经在用的编程 agent(Claude Code、Codex、Grok、Kimi 等)编成一支团队——任何会话都能跨厂商、跨机器 spawn、派活、收结果,而你从 Telegram、飞书或浏览器里统一指挥。**

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

**1 · 让 agent 装**

粘贴给你在用的任意 agent:

> Install https://github.com/firstintent/ccteam —— 按仓库里的 `INSTALL.md` 执行。

**2 · 源码安装** —— 推荐;需要 Rust 工具链 + Node.js(用于 web 控制台打包):

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam
make install
```

**3 · 一键脚本** —— 预编译二进制,无需工具链(linux + macOS arm/x64,Windows 走 WSL2;同样会问装 systemd):

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

装完验证:

```bash
ccteam --version
claude --version   # 必需,需要时按提示登录
codex --version    # 可选,用 Codex 会话才需要
grok --version     # 可选,用 Grok Build 会话才需要
opencode --version  # 可选,用 OpenCode 会话才需要
kimi --version      # 可选,用 Kimi Code 会话才需要(先 `kimi login`)
pi --version        # 可选,用 Pi 会话才需要(0.83.0 及以上)
```

> 若提示 `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"` 后重开终端。

**装到哪里。** 所有安装方式(一键脚本 / `make install` / `ccteam update`)共用**同一条落点阶梯**,不会给你留下两个互相打架的 `ccteam`:显式 `CCTEAM_INSTALL_DIR` 优先 → 否则装到 `ccteam` 现在所在的目录(**原地升级**,软链会先解析)→ 否则 `~/.local/bin`。装完脚本还会点名 PATH 上**其它**的 `ccteam` 副本(**只报告、绝不删**)—— PATH 里靠前的旧副本,正是「我明明升级了却毫无变化」的元凶。

### 2. 服务

`make install` 已经把服务起好了:唯一的常驻进程(Web 控制台 + IM 网关 + 标准资源 API + MCP)在 Linux 由 systemd `--user` 托管、在 macOS 由 launchd agent 托管 —— 开机/登录自启、崩溃自动重启、退出登录也不死。两个平台都用 `make daemon-status` / `daemon-logs` / `daemon-restart` / `daemon-stop` 管理(macOS 日志在 `~/.ccteam/daemon.log`)。卸载:源码装用 `make uninstall`、预编译装用 `install.sh --uninstall`,都会停掉并删除服务和二进制,但保留 `~/.ccteam`。没有任何 supervisor 的环境用 `ccteam start` 前台跑。

`make install` 结束时(或随时 `ccteam status`)会打印 Web 控制台地址,形如:

```text
web url:   http://<你的局域网IP>:7331/?token=ccteam:<令牌>
```

**点这个链接就进控制台了** —— 下面所有操作都在里面。

---

## 一、Web 控制台(推荐)

打开 `ccteam start` 给出的链接即可。控制台是**无全宽顶栏**的聊天壳:**可折叠侧栏**(⌘K 搜索、新建会话、工作流、会话列表),成本和头像在侧栏底部。**工作流**含 Skills / Roles / 插件市场 / MCP / 自进化(只读)。**设置**含 运维总览(daemon 健康 + 主机接入/注册动作面;全队观察面在团队页)/ 接入(外部 Agent MCP 配置、开发者 REST API、卫星加入、IM 凭据 —— 管理员管全局 bot,普通用户配自己的 bot;用户登录链接仍仅管理员)/ 通用 / 账号(人人可自助重置 token);仅「管理员(用户管理)」为管理员专属。主题**默认浅色**(可切深色)。

> **访问与安全**:默认绑 `0.0.0.0:7331`(局域网可访问)并用令牌鉴权,令牌存在 `~/.ccteam/secrets/web-token`。Web **无 TLS、明文传输**,请只在可信局域网用,**不要暴露公网**。要更严:`ccteam start --web-bind 127.0.0.1:7331` 只绑本机(此时免令牌),远程用 SSH 隧道。

### 注册 MCP(一次性,让 agent 能用 ccteam 的能力)

每次 `ccteam daemon start`(以及前台 `ccteam start`)会**自动**把 ccteam 自己的工具(雇会话/派活、发文件等)注册进**所有已安装 vendor** 的配置——Claude(`~/.claude.json`)、Codex(`~/.codex/config.toml`)、Grok(`~/.grok/config.toml`)、OpenCode(`~/.config/opencode/opencode.json`)、Kimi(`~/.kimi-code/mcp.json`)——任何 vendor 的普通会话都能指挥团队(Grok 侧可用 `grok mcp doctor` 验证连通)。写入幂等且只合并(不碰你其它 MCP server 条目),未安装的 vendor 自动跳过。**Pi 刻意不在这张表里**:它的受管会话由 ccteam 在 spawn 时挂自己的 bridge 扩展拿到团队工具,因此不写你任何 Pi 配置——反过来,你自己在 shell 里起的 `pi` 也就没有 ccteam 工具。需要手动补注册时(比如手改过 vendor 配置)用 `ccteam config mcp`,或进 **主机** 页点 **「注册 ccteam MCP」**;主机页还显示这台机器上各 vendor 装没装、版本、是否就绪。

### 创建项目

在新建会话弹窗里选 **「＋ 新建项目…」**,填 slug(短名)和目录路径,即可把任意目录登记成项目并在其中开会话。同名不同路径会自动累加为 `demo2` / `demo3`。

### 开会话、切换、对话

- **新建会话**:选 vendor(Claude / Codex / Grok / OpenCode / Kimi / Pi)与协议(stream-json / terminal 仅 Claude 管理员 / ACP=Grok·OpenCode·Kimi / Pi 自己的 RPC)、模型与思考强度、spawn 前 HITL 开关。两个菜单都按**所选 vendor 自己最近一次握手自报的目录**渲染(`GET /api/v1/models`)——列的是它自己的模型 id 和它自己的档位;没有强度轴的 vendor 干脆不显示强度菜单,留在**默认**则什么都不发、由 vendor 自己定。**执行主机 = 项目绑定的主机**(会话跟项目走,不再按会话选);每行会话带厂商标记。角色列表来自项目 `.claude/agents/`,spawn 时可选,留空即 roleless。建好回句柄 `s<N>`。
- **每个会话**有 **Chat | 终端** 两个标签页。Chat 里助手消息按 Markdown 渲染(标题/列表/表格/代码块,代码块一键复制);输入框 **Enter 发送、Shift+Enter 换行**,发送中可一键停止。
- **独立会话页**:`/app/chat/s/<sid>`(`<sid>` 与各入口的 `s1`/`s2` 同一命名空间)是某个会话的干净视图 —— 自己的历史、按会话过滤的实时事件,不与别的会话混流。
- **终端标签页**:逐字节保真地镜像会话屏幕(ANSI / 光标 / 对齐都对)。当前只对 Claude 会话开放。
- **历史会话与恢复**:会话列表下点「更多历史 (N) ▸」展开已**停止但未销毁**的会话(灰显)。点任意一个即从磁盘 `meta.json` **冷恢复**(cold-resume) —— 停止的会话、甚至 daemon 重启前的会话都不丢,随时可恢复(手机上 `/use <sid>` 同样能恢复)。「+ 导入历史会话」对话框还能发现你在 ccteam 之外用原生 `claude` 跑过的会话(按工作目录内容匹配),一键**收编**成普通 ccteam 会话,对话原文保留。
- **定时发送**:点输入框旁的**时钟**进入定时模式 → 填写「再过 **N 分钟** / **N 小时**」(或点快捷 `+15m` / `+30m` / `+1h` / `+2h`),也可选一个**本机时钟**上的绝对时间——界面会统一换算成相对延迟,浏览器时区与 daemon 时区不会打架;下方预览预计本机发送时刻 → 写正文 → 发送。排队条在**输入框上方**,按发送时间排序,点 **×** 取消。到点后正文作为**普通用户消息**进入该会话。定时模式**不能**带附件/技能。上限:每会话最多 20 条 pending,最远约 **7 天**。失败条目会标红并保留 24 小时。

> 部分高级选项(terminal/rmux 协议、历史会话恢复与导入)目前仅对管理员开放,普通用户默认用标准 Claude / Codex / Grok 聊天会话;随功能稳定会逐步放开。

### 团队页:拓扑与分工

**团队**页是多 vendor 调度的驾驶舱,两个 tab:

- **拓扑** —— 跨项目、跨主机的实时委派树:每会话一行(vendor 徽章、live 模型(live 会话显示真正在跑的模型)、状态点、成本、轮次),KPI 条下有 per-vendor 小计(live 数 + 花费,点击过滤)、最近委派滚动条,跨机时行上带 host 徽章。每个会话都是**真超链接** —— 右键/中键「打开 ↗」把父子会话开在两个浏览器 tab 里对照看。点行打开详情面板(模型/主机/父会话/深度/成本/实时活动/最近对话)。
- **分工** —— 团队的分工管理:**vendor 名册按主机分组**(local 在前、在线优先、离线卫星默认折叠并显示已离线多久 —— 超 7 天名册会建议清理 —— 且带「移除」按钮;主机 id 恒显,两台机器 OS hostname 撞名也能分清;每家 vendor 列 installed/版本/就绪态与修复提示,npm 目录有新版时低调提示「↑ 可更新」,外加 live 会话数与花费 —— **点 vendor 卡直接跳到按该家过滤的拓扑**)+ **宪章编辑器** —— 就是 agent 经 `status` 工具读的那份项目级 `routing.md`。选项目、markdown 编辑/预览、保存;项目没有宪章时只读展示全局 `~/.ccteam/routing.md`,可一键「拷入起稿」(web 只写项目文件,全局文件的写入口仍在 CLI/文件系统)。诚实语义不变:宪章是 agent 主动 PULL 的建议文本(绝不注入),超 ~4k 字符在 `status` 里节选并给全文指针。
- **编队起手** —— 分工 tab 上的六张卡(总控-工班 / 主力-顾问 / 交叉互审 / 并行竞标 / 调研三角 / 金字塔用工),点「起手」跳到首页 launcher 预填 vendor 阵容 —— 编排本身发生在你 spawn 的会话里。完整编队目录(含监工/值守/跨机三式)见 [orchestration-cn.md](orchestration-cn.md)。

### 插件市场:装角色 / 技能 / 工作流

**插件市场** 页(在**工作流**下;默认打开 Skills 分类,项目选择器只在装进项目的类型(agent/plugin)出现)浏览 [ccteam-hub](https://github.com/firstintent/ccteam-hub) 的精选插件(官方插件置顶,其余如 [agency-agents](https://github.com/wshobson/agents)、[mattpocock/skills](https://github.com/mattpocock/skills) 等开源库依次)。点开看正文预览后一键安装(下载时校验 sha256,带状态标记):**角色装进当前项目** `.claude/agents/`,装完任意入口 `/role <角色>` 切换;**技能装进用户级全局库** `~/.ccteam/skills`(**不进项目**),在会话输入框的 ＋ 菜单按条消息引用——技能菜单分两段:项目自有技能(`.agents/skills/`,兼容读旧 `.claude/skills/` 实体)与全局库;全局库与项目之间不软链、不复制。

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
- 资源:`/api/v1/projects`、`…/projects/{slug}/sessions`、`/sessions/{sid}/{turn,events,stop,scheduled}`、`/marketplace`、`/status`、`/hosts`、`/capabilities`、`/models`(按 vendor 列出它最近一次握手自报的模型(带 `observed_at`)+ 思考强度梯——给 spawn 填 `model`/`effort` 的 advisory 发现面,**永不当白名单**)。
- 鉴权与 Web 同一令牌;会话类端点需要 daemon 在线。

### 外部 Agent 直连 MCP(`POST /mcp`)

任何不由 ccteam 托管的 agent(你自己写的脚本、别处跑的 CLI)都可以拿 **ccteam web token** 直接调 daemon 的 MCP 端点,得到与托管会话相同的 8 个工具:

```
POST http://<host>:7331/mcp
Authorization: Bearer ccteam:<hex>
```

- **admin token**(`ccteam status` 输出的那枚)= owner 前门,fleet 级;**per-user token**(Settings → 用户 里签发)= 该用户身份,所有工具收窄到其自有项目:`session_spawn` 必须显式传 `project` 且只能是自己的项目,`dispatch/collect/stop` 按目标 sid 的归属项目做 ACL,`session_list`/`status` 只聚合可见项目。
- 无权与不存在的项目/会话返回同一错误(防枚举);bearer-only,不收 cookie 或 query 参数。

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
/new [vendor] [role] [hitl] [model=<id>] [effort=<level>]
                           新建会话 → 回一个句柄 s<N>
                             · vendor = claude(默认)| codex | grok | opencode | kimi | pi
                             · 省略 role = 裸 claude(自读项目 CLAUDE.md);写 role 则绑定该角色
                             · grok / opencode / kimi = 无角色 ACP 会话(忽略 role 参数)
                             · pi = 只在本机跑;支持角色
                             · 尾加 hitl = 工具在 IM 里逐个批准(默认 skip = 直接跑)
                             · model= / effort=(或 m= / e=)顺序随意,原文透传给 vendor;
                               不传就吃 vendor 自己的默认。各家梯度不同,`/status` 列出各自自报的档位
/use <id>                  切到会话 s<N>(已停止的会话会自动从磁盘冷恢复)
/role <role>               把当前会话换成另一个角色(原地重启,句柄 s<N> 不变)
/interrupt [id]            打断正在跑的回合,保留会话(省略 id = 当前)
/stop <id>                 销毁一个会话

# 查看 / 接入
/sessions [all]            列当前项目的会话(带 vendor · role · model · 上下文用量);`all` = 跨所有项目
/status                    全队健康:每个会话 idle / working / stuck + model · 强度 · ctx
                           ctx 只在**真测到**时才显示:vendor 还没报过就是「未知」而非 0%,
                           且 daemon 重启后不丢
/help                      列出网关命令

# 定时发送(一次性 user turn)
/inbox                     列出你可见的全部定时消息(自己的 + web 池),按发送时间排序
/inbox <时间> <正文>        约到**当前**会话(没有当前会话时先 /use 或 /sessions)
/inbox cancel <dN>         按 list 里的短 id 取消(或关掉失败条目)
```

`<时间>` 写法(按 **daemon 本机时区**;过去时刻直接拒绝,裸 `HH:MM` **不会**自动滚到明天):

```text
/inbox +30m 提醒我打开那个 PR
/inbox +2h 跑一遍夜间检查清单
/inbox 22:30 写今日日报
/inbox 明天 09:00 晨会纪要
/inbox 2026-07-26 09:00 发版 checklist
```

list 每行形如 `d3 · s12 · 2026-07-26 09:00 · 预览…`(失败会带原因)。到点成功时 IM **不会**再刷「已发送」——正文直接以普通用户消息进会话;失败会通知你,并在 list 里留 24 小时。与 Web 相同上限(每会话 20 条、最远 7 天)。空正文拒绝;正文以 `/` 开头时到点仍当 agent 的普通输入(不再当网关指令解析)。

### 寻址

```text
@<role>          切到该角色的会话并设为当前(单独 @role 只切换,不发消息)
@<role> <消息>    切到它并发一条
```

`@` 永远指向一个会话。确定性控制走上面的斜杠命令面(`/status` `/sessions` `/stop` …);自由形式的运维问题("今天哪个项目烧钱最多?")直接跟会话聊 —— 任何会话都能用 ccteam 的 MCP 工具回答。

### 直接对话 + 收发文件

- **不带前缀的消息** → 发给当前会话。
- **非网关的 `/命令`**(`/compact`、`/clear`、`/model` …)→ 透传给当前 agent;弹窗型(如 `/model`)会弹**选项按钮**,点一下即应用。
- **发图 / 发文件 + 一句说明** → agent 自动读取(报错截图、日志都行);agent 也能把文件发回你的 chat。
- **回合进行中** → 一条活的进度消息(形如 `⏳ working… · 🔧 bash ×3`),最终答案单独成条(会提醒);超长回答自动分片;agent 中途要你拿主意时会弹**选项按钮**,点一下喂回答案、它继续往下跑。

### 人工批准(HITL)

默认会话是「直接执行」(`skip`)。用 `/new <vendor> <role> hitl` 起一个需审批的会话:它跑非自动放行的工具前,会把「要跑什么」+ `[✅ 同意] [⛔ 拒绝]` 发到你 chat,点同意才执行,拒绝只挡这一次(不杀整个回合)。Codex 会话自带 sandbox,忽略此模式。Grok 会话本版仅支持 `skip`(自动放行);IM 审批已规划但尚未接入。

### 让任何会话派活

每个会话都能通过 `mcp__ccteam__session_*` 工具雇同事、派任务、收结论 —— 不用你手动切来切去,也不需要特殊角色,直接用自然语言交代:

```text
起一个 codex 会话,按 docs/rfc-12.md 实现,测试全过后汇报给我
```

不需要装任何 skill——ccteam 的 MCP server 自带使用说明,任何连上的会话天生就会整个闭环(spawn、盯进度、拿汇报)。想要常驻指挥官 persona,从插件市场装 `team-brain`。人话版指南——该说什么、最佳实践、附录给 persona/skill 作者的工具速查——见 [orchestration-cn.md](orchestration-cn.md)。

### 模型路由

会话决定叫谁干活时不必猜。一次 `status` 调用(MCP 工具,另有响应逐字节等同的发现别名 `grok_claude_codex_kimi`)就返回**厂商面板**——按你当前项目绑定的主机出:各 vendor 装没装、版本、诚实的 auth 信号(`ready` / `not_ready` / `unknown`——躺在 PATH 里绝不冒充已登录)、预算态、快照新鲜还是过期。面板旁边还有一份 **advisory 模型目录**(runtime 最近所见 + hub `models.json`,来源分开标注,永不当 spawn 白名单),以及你的**分工笔记**,原文透传。

你的分工是你自己写的 dumb markdown;ccteam 负责把它带给任何开口问的会话(在任何主机上都拿到同一份),但永不解析、不合并、不执行:

- `~/.ccteam/routing.md` —— 全局 fallback;统一 home 初始化会在缺失时生成一份中立模板,绝不覆盖你的内容。
- `<project>/.ccteam/routing.md` —— 可选的项目级覆盖;只要存在就完整取代全局文件,二者不合并。

写法就是一张朴素的表:任务类型 → vendor/model/effort → 理由。默认姿态:**spawn 时不传 `model`**,吃 vendor 默认值(厂商发新模型你自动升级);路由表里只写例外和升档。完整套路——能力核对、扇出对比、综合、成本——见编排指南的[模型路由章](orchestration-cn.md)。

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
ccteam config mcp              # 注册/刷新 ccteam MCP(五个 vendor 全写:Claude/Codex/Grok/OpenCode/Kimi;Pi 走受管会话 bridge、不写配置;无 TTY 用这个)
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
ccteam project new demo            # 在 <projects_root>/demo/ 下新建并 init(撞名累加 demo2、demo3…)
ccteam project stop demo           # 停该项目所有会话(可按 id 恢复;非删除)
ccteam project rm demo             # 注销项目(仅摘登记 + 清 ccteam 状态)
ccteam project rm demo --dry-run   # 先预览会停什么、删什么
ccteam project rm demo --purge     # 注销 + 删 ccteam 在项目里建的痕迹
```

`rm --purge` 只删 ccteam 建的(项目 `.ccteam/` + settings.local.json 里 ccteam 的 hook 段);**永远保留**你的 work-role、`CLAUDE.md`/`AGENTS.md`、`.env`、业务代码、你的 `.claude/settings.json`。

### `session`(会话)

```bash
ccteam session ls                # 列网关会话(SLUG·SID·ROLE·VENDOR·STATUS),标出 orphan
ccteam session attach demo [sid] # attach 到 terminal 协议会话的 tmux pane
```

> `attach` 只对 `terminal` 协议会话有意义(有 tmux pane);默认 `stream-json` 会话无 pane——用 web 聊天台或 IM 驱动。换某会话的角色走 IM `/role <role>`。

### `role`(从插件市场装角色)

```bash
ccteam role search backend         # 搜插件市场(官方插件置顶;--format json 可机读)
ccteam role add backend-architect  # 拉取该角色 .md(sha256 校验)写进当前项目 .claude/agents/
ccteam role add data-scientist --project demo   # 装到指定项目
ccteam role list                   # 列当前项目已装角色(= /role 可切的)
```

读 ccteam-hub 的目录(HTTPS + 本地缓存 `~/.ccteam/cache/hub/`),从上游仓库 @固定提交拉取、校验 sha256 后写入,已存在不覆盖(`--force` 才覆盖)。技能装进全局库:多文件 skill 整批校验后整目录落 `~/.ccteam/skills/<id>/`。Web 控制台插件市场页是同一来源的图形入口。

### `skill`(全局技能库;`role add` 遇技能会拒绝并指到这里)

```bash
ccteam skill search research        # 搜市场技能
ccteam skill add deep-research      # 装进全局库 ~/.ccteam/skills
ccteam skill ls                     # 列库(id 可嵌套,如 baoyu-skills/baoyu-comic)
ccteam skill rm <id>                # 删单个技能;整树需 --force
ccteam skill update --all           # 按 hub pin 重同步有更新的技能
ccteam skill source add <git-url>   # 整仓登记进库(update/ls/rm 同组)
ccteam skill ensure-project         # 项目自有技能面:.agents/skills + .claude/skills 软链
ccteam skill migrate-project        # 旧 .claude/skills 实体搬进 .agents/skills
```

全局库与项目**不相灌**:库里的技能只在会话里按条消息引用(web ＋ 菜单),要进 git 的项目技能自己放 `.agents/skills/`。

### 运维

```bash
ccteam status                  # daemon + 项目/会话 + 末尾两行 web token/url
ccteam session ls              # 网关会话状态(daemon 离线降级标注)
ccteam doctor --verify-mcp     # MCP 表面验收(8 工具 / 0 stub,漂移退出码 1)
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

# 反注册卫星(团队 → 分工 名册里每台主机也有「移除」按钮)。在线主机须 --force;
# local 永不可删(它就是主 daemon 本机)。
ccteam host rm <host-id> --daemon http://daemon-host:7331 --web-token <admin-hex> [--force]
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
