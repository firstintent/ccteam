# ccteam 使用指南

**ccteam —— 自托管、7×24 常驻的后台智能体团队:从 Telegram、飞书或网页端远程驱动你的 Claude Code / Codex。** 本文是一份命令为主、对照当前 CLI 校验过的端到端指南(install → init → config → start → 接入 IM → 日常用 → 运维),核心模型「一个 chat = 一台终端」,围绕 chat / project / session / role 四个对象,命令分 shell 侧 `ccteam <子命令>`(安装 + 运维)与 IM/web 网关命令(日常对话)两类。

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

| 路径 | 支持 |
|---|---|
| Claude harness + Claude 家族模型(`claude-*` / `sonnet` / `opus` / `haiku`) | 一等支持。角色 frontmatter 的 `model:` 用这些值最稳。 |
| Codex harness + Codex / OpenAI 模型 | best-effort。 |
| Claude harness + 非 Claude 模型 | 未验证。启动时提示「模型未验证」;若空转,改回 `sonnet`/`opus`/`haiku` 再 `/new`。 |

`mcp__ccteam__*` 工具由 `ccteam config`(§3)注册,给 Claude(`~/.claude.json`)和 Codex(`~/.codex/config.toml`)都写入 ccteam MCP server。

多实例:`ccteam --home ~/.ccteam2 start` 跑一个完全独立的实例(独立配置 / 租户 / 会话 / socket)。

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
ccteam init --owner user:u8e29d424      # 多用户:把项目归属给某用户租户(裸值补 user:;含 : 原样;re-init 覆盖无需 --force)
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

### 多用户:per-user web 登录 + 每人自己的 IM bot

一台机器、一个 daemon 给多个人用(同一 OS 账号下是**软隔离**、UX 非安全边界):

- **owner(admin)** 在 web Settings → 用户管理建用户,得到一次性个人链接 `?token=ccteam:<hex>`,发给对方;对方打开即以自己身份登录,只见自己的项目/会话。`ccteam status` 在本机随时列出**所有租户的登录链接**(admin/operator 视角;租户之间互不可见)。
- **每个用户自己的 IM bot**:租户登录 web → Settings →「我的 IM bot」填**自己的** Telegram bot token(或 Lark app),保存即 `getMe` 校验 + 该 bot 监听**即时**起(不重启 daemon)。这个 bot **只**驱动该用户自己的会话,跟别人、跟全局 bot 互不相干。**每个 bot 的 token 必须各不相同**(同 token 两处用会 `getUpdates` 409 冲突)。全局 `~/.ccteam/im/credentials.json` 的 bot 不再共享 —— 它现在是 owner(admin)自己的 bot。
- **`ccteam init --owner`**:CLI 起手就把项目归给某租户(见 §2)。

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
# 重启(只停 daemon;重启后按 session id 自动接回)
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

> `--web-bind` 是 `ccteam start` 的 web 地址参数;独立的内部 web 服务用 `ccteam internal web --bind`。两者别搞混。

---

## 6. 日常使用(IM 网关命令)

配对后,日常操作都在聊天框里:**网关命令**(`/…`,网关自己处理)、**寻址前缀**(`@…`),或**直接发消息 / 图片 / 文件**给当前会话。随时 `/help` 看清单(Telegram 里敲 `/` 也会弹候选)。

全部网关命令:

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
/use <id>                  切到会话 s<N>
/role <role>               把当前会话换成另一个角色(原地重启,句柄 s<N> 不变)
/interrupt [id]            打断正在跑的回合,保留会话(省略 id = 当前)
/stop <id>                 销毁一个会话
/screen [id]               截图一个会话的当前屏幕(省略 id = 当前)

# 查看 / 接入
/sessions                  列当前 chat 的会话(带 vendor · role · model · 上下文用量)
/status                    全队健康:每个会话 idle / working / stuck + model · ctx
/pair <code>               配对当前 chat(code 任意,如 phone)
/help                      列出网关命令
```

寻址前缀:

```text
@<role>          切到该角色的会话并设为当前(单独 @role 只切换,不发消息)
@<role> <消息>    切到它并发一条
@ccteam <verb>   管理:status · cost [today] · list · bots · pause / resume / stop <slug>[/role] · confirm
```

其它消息:

- **直接发消息**(不带前缀)→ 发给当前会话。
- **非网关的 `/命令`**(`/compact`、`/clear`、`/model` …)→ 透传给当前 agent;弹窗型(如 `/model`)会弹**选项按钮**,点一下即应用。
- **发图 / 发文件 + 一句说明** → agent 自动读取(报错截图、日志都行);agent 也能把文件 / 截图发回你的 chat。
- **回合进行中** → 一条活的进度消息(形如 `⏳ working… · 🔧 bash ×3`),最终答案单独成条(会 ping);超长回答自动分片;agent 中途要你拿主意时会弹**选项按钮**,点一下喂回答案、它继续往下跑。

让 cto 派活:默认 cto 管家能自己起 work-role 子会话、派任务、收结论 —— 不用你手动切来切去,直接用自然语言交代:

```text
@cto 起一个 backend-architect,评审 src/ 的接口设计,把结论汇总给我
```

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
- `role add` 从该条目的 `upstream` URL(已登记仓库 @pinned-sha)取原文、**sha256 校验内容完整性**后写入 `.claude/agents/<role>.md`(零改写)。**skill 同理**:单文件落 `.claude/skills/<id>/SKILL.md`,**多文件 skill**(带 `manifest`,如 mattpocock 的部分 skill)整目录落 `.claude/skills/<id>/<…>`。已存在同名 → 拒绝覆盖,加 `--force` 才覆盖。装完打印 `/role <role>` 提示,IM 里直接 `/role <role>` 切过去用。
- 插件**目录**住独立的 `firstintent/ccteam-hub`(curated marketplace)。市场是 **track-upstream** 模型:`index.json` 只存元数据 + 每条 `upstream`(指向上游仓库 @pinned-sha 的 raw URL)+ `content_sha`,**不存内容副本**;`sources.json` 声明跟踪的上游仓库(agency-agents + mattpocock/skills,均 MIT),装时才从 upstream 拉(只信白名单 host `raw.githubusercontent.com`)。ccteam repo 本身不带任何 role/skill 内容(唯一例外默认 `cto`)。web 控制台的「插件市场」页是同一来源的图形入口(见 §8)。

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

**统一界面**:本版 web 是**一个 chat 风格外壳** —— 顶栏有面包屑 + 连接状态 + **cost pill**(今日成本 / 预算,实时);每个 session 有 **Chat | 终端** 两个 tab;底部全局导航**四页** = **插件市场 / Status / 主机 / Settings**(旧的多页 operator 仪表盘已收敛掉)。**界面语言**在左上**头像**里切 **中文 / English**(默认中文,导航随之渲染),头像里还有个人设置(显示名 / 头像 / **明暗主题**(一个 Sun/Moon 图标切换,默认暗)/ 登出)。会话里**助手消息按 markdown 渲染**(标题 / 列表 / 表格 / 代码块,代码块右上角一键复制);输入框 **Enter 发送 · Shift+Enter 换行 · 输入法选词回车不误发**,发送中可一键停止。

**控制台页签**(浏览器里点点就能用,不必记命令):

- **新建项目**:新建会话弹窗里选「＋ 新建项目…」,填 slug(名)+ 路径即可在任意目录 scaffold 一个项目(走 `POST /api/v1/projects`),建好直接在里头起会话。
- **新建会话弹窗**:role 是从该项目 `.claude/agents/` 拉的**真实 role 下拉**(显示 role + 说明),外加一个「(无角色 / 裸 claude)」选项起 roleless 会话;不选则默认 cto。
- **插件市场页**:浏览 ccteam-hub 的 role/skill/workflow 插件(**官方 ccteam 插件置顶**,其余如 agency-agents 等开源依次),**点开看正文预览**(install 前 review),**一键装进当前项目**(sha256 校验,带「已装」状态标)。取代了旧的只读 Roles 页 —— 装完 IM 里 `/role <role>` 即用。
- **Status 页**:轻量状态总览 —— daemon 健康 + 会话 live/idle 数 + **每条会话的成本**(舰队骨架,best-effort)+ 今日成本/预算(同 `GET /api/v1/status`,也是 cost pill 的来源)。
- **主机 页**(v0.8.18):这台机器(host=`local`,将来分布式会列多台)的 agent 状态 —— hostname / 系统 / ccteam 版本,每个 vendor(claude / codex)装没装(带 `--version`)、ccteam MCP 注册没、**就绪 / 需配置 / 未安装**。唯一可写动作 = **「注册 ccteam MCP」**(把 ccteam 自己的 MCP server 写进 vendor 配置,幂等);ccteam **绝不**从 web 写 vendor 登录、绝不装 CLI。读 `GET /api/v1/hosts`。
- **Settings 页**:在浏览器里配 IM 凭证 —— Telegram(bot token + 异步抓 chat_id:存好 token 后给 bot 发条消息,页面轮询自动捕获)与 Lark/飞书(App ID / Secret / region / allowlist)。**秘密只显示掩码**(`…last4`),永不回显明文。**改完需重启 daemon 才生效**(凭证仅 daemon 启动时加载一次,无热重载)—— 页面会提示 `restart required`,照 §5 `ccteam stop && ccteam start`。
- **web 终端**(per-session):按会话解析到对应 pane,稳定连,逐字节保真(裸 ANSI / 光标 / 换行 / 对齐都对,连上回放当前屏幕)。终端 UI 当前只对 claude 会话开放。

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

GET    /api/v1/status                        daemon 健康 + 会话 live/idle + 今日 cost/budget + 每会话成本
GET    /api/v1/capabilities                  当前可用 harness(× provider)动态列表(PATH 探测)

GET    /api/v1/hosts                          host-keyed agent 报告(本机 local;将来多台)
GET    /api/v1/hosts/{host}                   单 host 详情（?refresh=true 重探;hostname/os/arch/version + 每 vendor 装/版本/MCP/就绪)
POST   /api/v1/hosts/{host}/register-mcp      注册 ccteam 自身 MCP 进 vendor 配置(唯一可写、幂等;?vendor=claude|codex)

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
ccteam stop                    # 优雅停 daemon
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

> IM 路径里的 Claude session 默认 `skip`(`--dangerously-skip-permissions`,YOLO 模式、无批准门)——只把 bot 暴露给可信 chat,bot token 不进 git。要逐工具人工批准,用 `/new <vendor> <role> hitl` 起一个 HITL session(见 §6)。
