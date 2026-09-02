# ccteam 使用手册

**ccteam 把你已经在用的编程 agent(Claude Code、Codex、Grok、Kimi、DSH 等)编成一支团队——任何会话都能跨厂商、跨机器 spawn、派活、收结果,而你从 Telegram、飞书或浏览器里统一指挥。**

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

ccteam 调用你机器上**已装好并完成认证的** Claude Code / Codex / Grok Build / OpenCode / Kimi Code / DSH / Pi,自己不打包它们。

**先装好并认证其中至少一个** —— 没装、或装了但没认证的 vendor,在那台机器上起不了会话:

| Vendor | 安装 | 认证 |
|---|---|---|
| Claude Code | [docs.claude.com/en/docs/claude-code](https://docs.claude.com/en/docs/claude-code) | `claude auth login` |
| Codex | [github.com/openai/codex](https://github.com/openai/codex) | `codex login` |
| Grok Build | [docs.x.ai/build/overview](https://docs.x.ai/build/overview) | `grok login` |
| OpenCode | [opencode.ai](https://opencode.ai) | `opencode auth login` |
| Kimi Code | [moonshotai.github.io/kimi-code](https://moonshotai.github.io/kimi-code/) | `kimi login` |
| DSH | [npmjs.com/package/@deepseek-ai/dsh](https://www.npmjs.com/package/@deepseek-ai/dsh) | `npm i -g @deepseek-ai/dsh`;DSH 会话与 DSH Web 优先用 `DEEPSEEK_API_KEY`,否则用该身份在 DSH Settings → Models 里的配置 |
| Pi | [pi.dev](https://pi.dev/) | 配好 provider API key,用 `pi auth check --provider <provider>` 验证 |

**1 · 让 agent 装**

粘贴给你在用的任意 agent:

> Install https://github.com/firstintent/ccteam —— 按仓库里的 `INSTALL.md` 执行。

**2 · 源码安装** —— 推荐;需要 Rust 工具链 + Node.js(用于 web 控制台打包):

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam
make install
```

**3 · 一键脚本** —— 预编译二进制,无需工具链(linux + macOS arm/x64,Windows 走 WSL2):

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

**4 · 从 DeepSeek Harness 安装** —— 不需要工具链,也不用单独装 ccteam:`dsh plugin --profile web add @ccteam/ccteam-ui`,重启 `dsh web`,插件会从它的平台包把引擎装到与脚本相同的位置并启动 daemon。它与其它安装方式是同一个二进制、同一个 daemon —— 之后再装的 `ccteam` CLI 会直接接上它。细节见 [dsh-plugin-cn.md](dsh-plugin-cn.md)。

装完验证:

```bash
ccteam --version
claude --version   # 可选,用 Claude 会话才需要
codex --version    # 可选,用 Codex 会话才需要
grok --version     # 可选,用 Grok Build 会话才需要
opencode --version  # 可选,用 OpenCode 会话才需要
kimi --version      # 可选,用 Kimi Code 会话才需要(先 `kimi login`)
dsh --version       # 可选,用 DSH 会话才需要(0.1.0-rc.6 或以上)
pi --version        # 可选,用 Pi 会话才需要(0.83.0 及以上)
```

> 若提示 `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"` 后重开终端。

**装到哪里。** 所有安装方式(一键脚本 / `make install` / `ccteam update` / DSH 插件)共用**同一条落点阶梯**,不会给你留下两个互相打架的 `ccteam`:显式 `CCTEAM_INSTALL_DIR` 优先 → 否则装到 `ccteam` 现在所在的目录(**原地升级**,软链会先解析)→ 否则 `~/.local/bin`。装完脚本还会点名 PATH 上**其它**的 `ccteam` 副本(**只报告、绝不删**)—— PATH 里靠前的旧副本,正是「我明明升级了却毫无变化」的元凶。

### 2. 服务

`make install`(以及一键脚本检测到升级时)已经用 **`ccteam start`** 把服务起好了:唯一的常驻进程(Web 控制台 + IM 网关 + 标准资源 API + MCP socket)。daemon **只有一种起法 = launcher**:`ccteam start` 就是 `ccteam daemon start` —— 用 `setsid` 把 daemon 脱离终端(关掉 shell、断开 SSH 都不死),等它就绪,把 pid 记进 `~/.ccteam/state/orchestrator.pid`,打印 Web 控制台链接,然后退出。它是幂等的 —— 再敲一次 `ccteam start` 只会找到正在跑的 daemon 并报告它,不会再起一个 —— 而且**没有前台模式**(`nohup ccteam start &` 也只是 launcher,daemon 起来后它就退出)。DSH 插件的自动启动跑的也是同一个 launcher,所以从 DSH 起的 daemon 和从 CLI 起的是同一个(见 [dsh-plugin-cn.md](dsh-plugin-cn.md) 的「共存」一节)。Linux / macOS / WSL **同一套机制** —— ccteam **不安装 systemd 或 launchd unit**。各平台一样管:

```bash
ccteam start             # ≡ ccteam daemon start:脱离终端、等就绪、打印 web 链接(幂等)
ccteam daemon status     # pid · ready · 运行中版本 vs 二进制版本(脚本用 --json)
ccteam daemon logs -f    # 跟踪 ~/.ccteam/daemon.log
ccteam daemon restart    # 优雅 SIGTERM 停 + 重新脱离(也可 `make daemon-restart`)
ccteam stop              # ≡ ccteam daemon stop:优雅停;agent 进程永不被杀 —— 见下文「daemon 重启对活会话意味着什么」
```

运行时 flag(`--web-bind`、`--dsh-web-bind`、`--no-web`、`--no-imd`、`--web-no-auth`、`--web-token-file`、`--no-clipboard`)都是 launcher flag,逐字转发给 daemon;`ccteam daemon restart` 不带 flag 时重放 daemon 当初启动用的 flag。带 `--json` 时 `ccteam start` 恰好打印一行:`{"status":"started","pid":…,"version":"…","home":"…"}` 或 `{"status":"alreadyRunning","pid":…,"version":"…","home":"…"}` —— 两者都算成功(`home` = 规范化的 `$CCTEAM_HOME`,调用方据此判断找到的是不是自己的 daemon);失败则是 `{"status":"error","code":"…","message":"…"}`。

**跑着的是谁。** web 端口上的 `GET /health` 不需要令牌,回答 daemon 的身份:`status`、`version`、`build`(构建时记录了 commit 才有)、`home`(规范化 `$CCTEAM_HOME`)、`pid`、`web_bind`(实际服务的地址 —— 请求 `:0` 时报的是分到的端口)、`dsh_web_bind`(伴生监听关闭时为 `null`)、`uptime_secs`。`ccteam daemon status --json` 打印同一组字段,取自正在跑的 daemon(HTTP 够不着时 —— `--no-web`,或本机拨不通的绑定地址 —— 相应字段为 `null`),外加 `binary`(你调用的这个 `ccteam` 的绝对路径)、`ready`、`managed`、`runningVersion`、`binaryVersion`、`socket`。DSH 插件、CLI 和你自己的脚本都先比对 `home`,再把一个 daemon 当成自己的。

诚实的取舍:没有 OS supervisor,就**没有崩溃自动重启、也不开机自启** —— 重启机器后再跑一次 `ccteam start`(`ccteam status` / `ccteam doctor` 一眼看出 daemon 没起;想要开机自启,自己加一行 `@reboot ccteam start` cron)。想交给 systemd 托管,launcher 的形状就是 `Type=forking`:

```ini
# ~/.config/systemd/user/ccteam.service
[Unit]
Description=ccteam daemon

[Service]
Type=forking
ExecStart=%h/.local/bin/ccteam start
ExecStop=%h/.local/bin/ccteam stop
# 只有 daemon 属于这个 unit:它拉起的 agent 进程必须在 stop 之后活下来。
KillMode=process

[Install]
WantedBy=default.target
```

不要设 `PIDFile=`:`~/.ccteam/state/orchestrator.pid` 是一条 JSON 记录(pid + 进程启动时间 + 版本),不是裸 pid,systemd 会自己在 unit 的 cgroup 里找到 daemon。之后 `systemctl --user enable --now ccteam` 就会一直拉着它;`ccteam daemon status` 仍报它为托管(它是 launcher 起的),在 shell 里敲 `ccteam start` 报 `alreadyRunning`,DSH 插件也像对待任何 daemon 一样 attach 上去 —— 插件「引擎」段的「停止」仍是真的 `ccteam stop`,之后 systemd 可能把 daemon 再拉起来。卸载:源码装用 `make uninstall`、预编译装用 `install.sh --uninstall`,都会停掉并删除二进制,但保留 `~/.ccteam`。

**从曾由 ccteam 写过 systemd/launchd unit 的安装升级**:重跑一次安装器或 `ccteam start` 即可 —— 一次性接管会停用并删除旧的 ccteam service unit,再把 daemon 自管地拉起来;你自己手写的 unit 一动不动(ccteam 会把那个实例报成「非托管」且不去停它)。后续升级见[更新](#更新)。

`make install` 结束时(或随时 `ccteam status`)会打印 Web 控制台地址,形如:

```text
web url:   http://<你的局域网IP>:7331/?token=ccteam:<令牌>
```

**点这个链接就进控制台了** —— 下面所有操作都在里面。

---

## 一、Web 控制台(推荐)

打开 `ccteam start` 给出的链接即可。控制台是**无全宽顶栏**的聊天壳:**可折叠侧栏**(⌘K 搜索、新建会话、工作流、会话列表),成本和头像在侧栏底部。**工作流**含 Skills / Roles / 插件市场 / MCP / 自进化(只读)。**设置**含 运维总览(daemon 健康 + 主机接入/注册动作面;全队观察面在团队页)/ 接入(外部 Agent MCP 配置、开发者 REST API、卫星加入、IM 凭据 —— 管理员管全局 bot,普通用户配自己的 bot;用户登录链接仍仅管理员)/ 通用 / 账号(人人可自助重置 token);仅「管理员(用户管理)」为管理员专属。主题**默认浅色**(可切深色)。

> **访问与安全**:默认绑 `0.0.0.0:7331`(局域网可访问)并用令牌鉴权,令牌存在 `~/.ccteam/secrets/web-token`。Web **无 TLS、明文传输**,请只在可信局域网用,**不要暴露公网**。要更严:`ccteam start --web-bind 127.0.0.1:7331` 只绑本机(此时免令牌),远程用 SSH 隧道。DSH Web 走伴生监听,默认是 web 端口 + 1;用 `--dsh-web-bind <addr:port>` 指定,或 `--dsh-web-bind off` 关闭(此时 `/api/v1/dsh/status` 仍会返回 `disabled`)。

### 注册 MCP(一次性,让 agent 能用 ccteam 的能力)

每次 daemon 启动(`ccteam start` / `ccteam daemon start`,含 DSH 插件的自动启动)会**自动**把 ccteam 自己的工具(雇会话/派活、发文件等)注册进**所有允许 ccteam 写配置的已安装 vendor**——Claude(`~/.claude.json`)、Codex(`~/.codex/config.toml`)、Grok(`~/.grok/config.toml`)、OpenCode(`~/.config/opencode/opencode.json`)、Kimi(`~/.kimi-code/mcp.json`)——这些 vendor 的普通会话都能指挥团队(Grok 侧可用 `grok mcp doctor` 验证连通)。写进去的是一枚**用户域 enrollment 凭据** —— 它只说明「这份配置是谁的」,per-process 身份由 daemon 在该 vendor 的会话连上来时签发,所以同一份配置隔一小时起的两个 agent 是两个 caller、各有自己的账本行。写入幂等且只合并(不碰你其它 MCP server 条目),未安装的 vendor 自动跳过;旧版 ccteam 留下的条目(`Bearer ccteam:<hex>` admin token,或 `command` 形式的 stdio 条目)一律读作**未注册**,下次启动自动替换。

**DSH 刻意不在这张 config-writer 表里**:它没有等价的全局 MCP 配置文件。从 ccteam 雇 DSH 不需要你安装插件:`/new dsh`、Web 的 DSH 页,或 MCP `agent {vendor:"dsh", task:"…"}` 直接接进该身份唯一的 DSH web 运行时——就是 **DSH** 菜单看到的那个空间(普通用户 `$CCTEAM_HOME/runtime/dsh/web/<user>/`,owner 真 `~/.dsh`)——所以雇出来的会话会实时出现在 DSH 侧栏、按项目 workspace 分组,同 sid 可冷恢复,token 用量会入账。ccteam 托管的运行时都预载了 `@ccteam/ccteam-ui`;你自己手起的 `dsh web`,在 **Hosts** 页一键注册插件(或 `dsh plugin --profile web add @ccteam/ccteam-ui`)后自行重启即可。你自己的 DSH 会话也能编排团队:在 daemon 所在机器上,插件会自己向 daemon 领取 enrollment 凭据(每个 DSH profile 一份,`dsh-plugin:<profile>`,幂等由 daemon 保证);daemon 在别处时,到 DSH Settings 粘贴 daemon URL 和 **Settings → Access** 里复制的 enrollment 凭据。两种情况下都拿到同一套 6 个工具;若还没绑定 ccteam 项目,第一次工具调用会要求你点名一个 slug,之后这个会话会记住。

**Pi 也刻意不在 config-writer 表里**:它的受管会话由 ccteam 在 spawn 时挂自己的 bridge 扩展拿到团队工具,因此不写你任何 Pi 配置——反过来,你自己在 shell 里起的 `pi` 也就没有 ccteam 工具。需要给可写配置的 vendor 手动补注册时(比如手改过 vendor 配置)用 `ccteam config mcp`,或进 **主机** 页点 **「注册 ccteam MCP」**;主机页还显示这台机器上各 vendor 装没装、版本、是否就绪。

### DSH Web

**DSH** 页把原生 DeepSeek Harness Web 嵌进 ccteam 控制台,中间走伴生端口反代。它复用 ccteam 登录 cookie;反代会剥掉 ccteam cookie / bearer,不会把它们交给 DSH 进程或日志。

一个身份只跑**一个** DSH 运行时,ccteam 是它的第二个 client:从 ccteam 雇出来的 DSH 会话(`/new dsh`、`agent {vendor:"dsh"}`)就建在这个运行时里,所以它们会实时出现在本页侧栏、挂在项目 workspace 下;agent 干到一半你可以点开围观或插话,agent 的下一次 dispatch 接着同一条对话继续。雇佣会话会加入 DSH 四种 agent preset 之一(`standard` / `ptc` / `minimal` / `creator`,决定工具集),ccteam 默认 `standard`,`/new dsh mode=<m>`(或 spawn API 的 `mode`)可选其它;雇佣会话权限 preset 默认 `danger-full-access`(全文件访问、免审批),spawn 时选 `hitl` 则保留逐次审批。

- **Owner**:使用真实 `~/.dsh` 空间。若本机已有原生 `dsh web` 跑在 `127.0.0.1:3080`,ccteam 会 attach 到它,不会再开第二个写同一个 home 的进程——此时雇 DSH 需要那个实例里装有 ccteam 插件(**Hosts** 页一键注册,然后你自己重启它;ccteam 绝不重启不是它起的进程)。没有原生实例时 ccteam 自己在临时 loopback 端口启动,插件已注册,且 ccteam 工作台已带上你自己的 REST token(即 admin web token,0600 写进你自己的 profile),无需粘贴。浏览器就在本机时可直接打开原生 URL;局域网/远程浏览器经 ccteam 代理访问。
- **普通用户**:每个身份一个 `$CCTEAM_HOME/runtime/dsh/web/<user>/` 空间,预置 DSH base/web app 与 `@ccteam/ccteam-ui`;从 ccteam 雇的 DSH 会话也住这个家,所以会出现在 DSH 页里。profile 是合并式物化:用户自己装的 DSH 插件会保留,ccteam 的插件物化每次启动自愈。只要这台机器已有 DSH 登录,首次打开即可用:ccteam 会从机器的 DSH home 种子该身份的 DSH 配置文件,且在用户未改动时继续跟随这些字节。
- **模型密钥**:在 DSH 原生 **Settings → Models** 里配置自己的 provider。同身份的所有 DSH 会话——你在这页打开的和在 ccteam 里雇出来的——都跑在同一个运行时、用同一份配置,改一次全体生效。ccteam 只逐字节复制和 hash 这些 DSH 配置文件,不解析 vendor YAML。
- **账本**:DSH Web 里原生跑的 turn 不是 ccteam session,不会在 ccteam 账本里伪装成 `$0` 或其它值;同一条规则在雇出来的会话里同样成立——你从 DSH 侧直接输入的 turn 是 vendor 原生 turn,ccteam 的 transcript 与账本只记 ccteam 路由的部分,完整对话以 DSH 家为准。从 DSH 通过 ccteam 插件委派出去的工作照常入账。
- **局域网明文 HTTP**:DSH Web 是按 loopback 源写的,浏览器只在安全上下文里给它 `crypto.randomUUID` 等 API;把 UI 搬离 loopback 的是 ccteam,所以伴生监听会在它下发的 HTML 里补回这一个 API(真 `crypto.getRandomValues` 生成的 UUID v4),浏览器自带时则不介入。走 HTTPS 或在 daemon 本机打开都不需要这些。
- **局域网浏览器改设置**:DSH 把设置文档(所有 Settings 页,含「插件配置」)当作「操作者本机」才可触及的特权面,而且由客户端按页面地址判断——原生 `dsh web` 从局域网地址打开时,「插件配置」页一片空白、设置只读。经伴生监听访问时这个「本机」就是你的 ccteam 身份:实例只属于你,页面已经过 ccteam 鉴权,所以伴生监听用 DSH 自己的传输钩子(`__DSH_TRANSPORT__.ownsHost`)声明「本页拥有其 Host」,任何浏览器都能改设置。DSH 从 0.1.1-rc.2 之后的版本才原生读这个钩子(已在 DSH main 上);对 rc.2 及更早版本(客户端只按页面主机名判断),伴生监听把这一处读取回填进它下发的 client-connection bundle(精确到行的改写,其它版本上零操作),所以 rc.2 上同样任何浏览器都能改设置。一个连带后果:DSH 的产出文件动作会作用在 daemon 所在机器(即工作区机器)上。
- **信任边界**:租户 DSH Web 是同一 OS 用户下的软隔离。DSH agent 能跑 shell,用户自装 DSH 插件也是任意 npm 代码,信任级等同这个系统账号。同一个 OS 用户下的配置可见性只是便利边界,不是硬安全边界。
- **局域网明文访问**:DSH Web 是按 loopback 源写的 —— 浏览器只在安全上下文里给它 `crypto.randomUUID`,而它用这个 API 生成**每一个** RPC 请求 id;换成局域网地址 + 明文 HTTP,这个 API 就没了。把界面搬离 loopback 的是 ccteam,所以伴生监听会在下发的 HTML 里补回这一个 API(用 `crypto.getRandomValues` 实现的标准 v4 UUID,不降随机强度);浏览器本来就提供时它自动让位。用 HTTPS 访问控制台、或直接在 daemon 本机打开,都不需要这层补丁。

如果 ccteam 前面有 HTTPS 反代,**伴生端口也必须一起反代**。DSH Web 没有 base-path 支持,所以用第二个 HTTPS 端口或子域;只反代 `:7331` 会让 iframe 继续加载明文 HTTP,浏览器按 mixed-content 拒绝。

### 创建项目

在新建会话弹窗里选 **「＋ 新建项目…」**,填 slug(短名)和目录路径,即可把任意目录登记成项目并在其中开会话。同名不同路径会自动累加为 `demo2` / `demo3`。

### 开会话、切换、对话

- **新建会话**:选 vendor(Claude / Codex / Grok / OpenCode / Kimi / DSH / Pi)与协议(stream-json / terminal 仅 Claude 管理员 / ACP=Grok·OpenCode·Kimi·DSH / Pi 自己的 RPC)、模型与思考强度、spawn 前 HITL 开关。两个菜单都按**所选 vendor 自己最近一次握手自报的目录**渲染(`GET /api/v1/models`)——列的是它自己的模型 id 和它自己的档位;没有强度轴的 vendor 干脆不显示强度菜单,留在**默认**则什么都不发、由 vendor 自己定。**执行主机 = 项目绑定的主机**(会话跟项目走,不再按会话选);每行会话带厂商标记。角色列表来自项目 `.claude/agents/`,spawn 时可选,留空即 roleless;Grok / OpenCode / Kimi / DSH 当前只支持 roleless。建好回句柄 `s<N>`。
- **每个会话**有 **Chat | 终端** 两个标签页。Chat 里助手消息按 Markdown 渲染(标题/列表/表格/代码块,代码块一键复制);输入框 **Enter 发送、Shift+Enter 换行**,发送中可一键停止。
- **独立会话页**:`/app/chat/s/<sid>`(`<sid>` 与各入口的 `s1`/`s2` 同一命名空间)是某个会话的干净视图 —— 自己的历史、按会话过滤的实时事件,不与别的会话混流。
- **终端标签页**:逐字节保真地镜像会话屏幕(ANSI / 光标 / 对齐都对)。当前只对 Claude 会话开放;Codex / Grok / OpenCode / Kimi / DSH / Pi 都是 chat-only。
- **历史会话与恢复**:会话列表下点「更多历史 (N) ▸」展开已**停止但未销毁**的会话(灰显)。点任意一个即从磁盘 `meta.json` **冷恢复**(cold-resume) —— 停止的会话、甚至 daemon 重启前的会话都不丢,随时可恢复(手机上 `/use <sid>` 同样能恢复)。「+ 导入历史会话」对话框还能发现你在 ccteam 之外用原生 `claude` 跑过的会话(按工作目录内容匹配),一键**收编**成普通 ccteam 会话,对话原文保留。
- **定时发送**:点输入框旁的**时钟**进入定时模式 → 填写「再过 **N 分钟** / **N 小时**」(或点快捷 `+15m` / `+30m` / `+1h` / `+2h`),也可选一个**本机时钟**上的绝对时间——界面会统一换算成相对延迟,浏览器时区与 daemon 时区不会打架;下方预览预计本机发送时刻 → 写正文 → 发送。排队条在**输入框上方**,按发送时间排序,点 **×** 取消。到点后正文作为**普通用户消息**进入该会话。定时模式**不能**带附件/技能。上限:每会话最多 20 条 pending,最远约 **7 天**。失败条目会标红并保留 24 小时。

> 部分高级选项(terminal/rmux 协议、历史会话恢复与导入)目前仅对管理员开放,普通用户默认用标准 Claude / Codex / Grok 聊天会话;随功能稳定会逐步放开。

### 团队页:拓扑、分工与编排

**团队**页是多 vendor 调度的驾驶舱,三个 tab:

- **拓扑** —— 跨项目、跨主机的实时委派树:每会话一行(vendor 徽章、live 模型(live 会话显示真正在跑的模型)、状态点、成本、轮次),KPI 条下有 per-vendor 小计(live 数 + 花费,点击过滤)、最近委派滚动条,跨机时行上带 host 徽章。每个会话都是**真超链接** —— 右键/中键「打开 ↗」把父子会话开在两个浏览器 tab 里对照看。点行打开详情面板(模型/主机/父会话/深度/成本/实时活动/最近对话)。
- **分工** —— 团队的分工管理:**vendor 名册按主机分组**(local 在前、在线优先、离线卫星默认折叠并显示已离线多久 —— 超 7 天名册会建议清理 —— 且带「移除」按钮;主机 id 恒显,两台机器 OS hostname 撞名也能分清;每家 vendor 列 installed/版本/就绪态与修复提示,npm 目录有新版时低调提示「↑ 可更新」,外加 live 会话数与花费 —— **点 vendor 卡直接跳到按该家过滤的拓扑**)+ **宪章编辑器** —— 就是 agent 经 `status` 工具读的那份项目级 `routing.md`。选项目、markdown 编辑/预览、保存;项目没有宪章时只读展示全局 `~/.ccteam/routing.md`,可一键「拷入起稿」(web 只写项目文件,全局文件的写入口仍在 CLI/文件系统)。诚实语义不变:宪章是 agent 主动 PULL 的建议文本(绝不注入),超 ~4k 字符在 `status` 里节选并给全文指针。
- **编排** —— daemon 记到账本上的每一次 `ccteam flow run`,跨你可见的项目按新到旧排:名字、描述、状态(运行中 / 完成 / 出错 / 刹车 —— 刹车是护栏拒绝再雇新 agent,不是崩溃)、雇佣 agent 数、成本、耗时;展开一行看它真正雇了哪些会话(真链接)和触发它的会话。还没有 run 时这个 tab 就是上手入口:发起第一次 run 的三条命令,可复制。flow 只在 shell 里(或由任意会话)编写与启动,web 不启动 run;详见 [hook-dynamic-workflows-cn.md](hook-dynamic-workflows-cn.md)。
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
- 成本按 vendor 分别记账。Claude/Codex/Grok 有价表时用表计价;**OpenCode 只认自报 USD**(无上报或 0 显示「—」,绝不套用他家价表);**DSH 会上报原始 token,但暂时没有 USD 价表**。

### 标准资源 API(给集成方)

控制台本身就建立在一套 **令牌鉴权的 HTTP API** 之上,你也可以直接用它做集成:

- 交互式文档:浏览器开 `http://<host>:7331/api/docs`(Scalar,可直接试调);机读 spec 在 `/api/v1/openapi.json`。
- 资源:`/api/v1/projects`、`…/projects/{slug}/sessions`、`/sessions/{sid}/{turn,events,stop,scheduled}`、`/marketplace`、`/status`、`/hosts`、`/capabilities`、`/models`(按 vendor 列出它最近一次握手自报的模型(带 `observed_at`)+ 思考强度梯——给 spawn 填 `model`/`effort` 的 advisory 发现面,**永不当白名单**)。
- 鉴权与 Web 同一令牌;会话类端点需要 daemon 在线。

### 外部 Agent 直连 MCP(`POST /mcp`)

任何不由 ccteam 托管的 agent(你自己写的脚本、手起的 CLI、别的机器上的 agent)都可以拿 **enrollment 凭据**直接调 daemon 的 MCP 端点,得到与托管会话相同的 6 个工具:

```
POST http://<host>:7331/mcp
Authorization: Bearer ccteam-enroll:<id>:<secret>
Mcp-Session-Id: <initialize 时 daemon 返回的 id>
```

- **凭据只说明「这份配置是谁的」,身份由 daemon 在 `initialize` 时签发**:响应里的 `Mcp-Session-Id` 让**这个进程**成为一个独立 caller —— 它在账本里有自己的会话行(`managed_by: external`),它 spawn 的会话真的挂在它下面,而不是变成一堆根节点。之后每个请求必须同时带凭据与该 id(id 本身不是凭据,且 binding 只对开它的那枚凭据生效);id 过期返回 `404` 提示重新 `initialize`,用完可 `DELETE /mcp` 关闭。
- **两种作用域**:每次 `ccteam daemon start` 会把**用户域**的机器凭据——无 label 的那个槽位,跨重启复用、从不重铸——写进本机各 vendor 配置,所以手起的 Claude/Codex/Grok/OpenCode/Kimi 直接就有工具;DSH 经 `@ccteam/ccteam-ui` 插件走同一套身份模型:插件向本机 daemon 领取自己的用户域凭据(label `dsh-plugin:<profile>`),daemon 在别处时用粘贴的那枚。用户域凭据不钉项目,故首个 `agent` 调用必须显式传 `project`,且只接受本人可见的项目。**项目域**凭据 = 控制台复制按钮发的那枚(设置 → 接入,或 `POST /api/v1/projects/{slug}/enroll`):钉死一个 workspace,贴到别的机器也只够得着它,事后可列出、可吊销;secret 只在签发那一刻显示一次。
- **反复启动的程序用 ensure 而不是 mint**:`POST /api/v1/enroll` 带 `ensure: true` 和 `label` 时按 (身份, label) 幂等——daemon 新建记录答 `201` 并附 bearer,记录已存在则答 `200` 且只附 `bearer_prefix`(secret 永不第二次回传);丢了自己那份的调用方发 `rotate: true` 换掉记录,而不是再堆一条。控制台自己的 mint(不带 `ensure`)仍每次新铸一枚。
- **不做任何推断**:不看工作目录、不看来源地址、没有「最近用过的项目」—— 没有依据就直接拒绝,并告诉你可以点名哪些 slug。无权与不存在的项目/会话返回同一错误(防枚举);bearer-only,不收 cookie 或 query 参数;web 控制台令牌(那是 `/api/v1/**` 的凭据)在这里会被 401 拒绝并说明本端点认哪两族凭据。

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
                             · vendor = claude(默认)| codex | grok | opencode | kimi | dsh | pi
                             · 省略 role = 裸 claude(自读项目 CLAUDE.md);写 role 则绑定该角色
                             · grok / opencode / kimi / dsh = 无角色 ACP 会话(忽略 role 参数)
                             · dsh = 只在本机跑;支持冷恢复和 token 记账
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
/status                    当前会话的深度视图——无论什么状态都是同一张卡:idle / working /
                           stuck,或 💤 released(下条消息即恢复;卡片读的是落盘事实)。
                           含 model · 强度 · ctx、账号用量窗口(5h / 周;本会话没进程时
                           向同 harness 的在线会话读,并按 harness 记住,所以一个进程都不在
                           也照样显示;过了厂商自报的重置时间就丢弃,不显示过期数)、
                           自己的后台工作、直接子会话,
                           以及指向其余舰队的页脚(/sessions、/projects)
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

默认会话是「直接执行」(`skip`)。用 `/new <vendor> <role> hitl` 起一个需审批的会话:它跑非自动放行的工具前,会把「要跑什么」+ `[✅ 同意] [⛔ 拒绝]` 发到你 chat,点同意才执行,拒绝只挡这一次(不杀整个回合)。Claude / DSH / Pi 会话支持这条审批路;DSH 和 Pi 会把自己的 permission dialog 接到同一组同意/拒绝按钮上。Codex 会话自带 sandbox,忽略此模式。Grok / Kimi 当前仅支持 `skip`(自动放行);IM 审批已规划但尚未接入。

### 让任何会话派活

每个会话都能通过 `mcp__ccteam__agent` / `mcp__ccteam__agent_read` 工具雇同事、派任务、读答案 —— 不用你手动切来切去,也不需要特殊角色,直接用自然语言交代:

```text
起一个 codex 会话,按 docs/rfc-12.md 实现,测试全过后汇报给我
```

不需要装任何 skill——ccteam 的 MCP server 自带使用说明,任何连上的会话天生就会整个闭环(spawn、盯进度、拿汇报)。想要常驻指挥官 persona,从插件市场装 `team-brain`。人话版指南——该说什么、最佳实践、附录给 persona/skill 作者的工具速查——见 [orchestration-cn.md](orchestration-cn.md)。

### 模型路由

会话决定叫谁干活时不必猜。`status`(MCP 工具,另有响应逐字节等同的发现别名 `grok_claude_codex_kimi`)返回**分级**的紧凑 JSON。默认 `brief` 档就是「雇谁」这一个答案:你当前项目绑定的主机真正能雇哪些 vendor、团队最近 24 小时花了多少,以及仅在成立时才出现的两件事——卫星主机离线 / 快照过期的标记,和被预算触顶禁用的 vendor。要更厚就传 `detail`,而且只有这一次调用付这笔字节:`models` 加上各 vendor 观测到的模型 id 与思考强度梯,外加 hub `models.json` 目录(两个来源分开标注,都是 advisory,永不当雇人白名单);`vendors` 加上各 vendor 装没装 / 版本 / 诚实的 auth 信号(躺在 PATH 里绝不冒充已登录)/ 预算态与观测时间;`routing` 把你的**分工笔记**原文带上(来源、sha256、是否截断),没有笔记时告诉你它找过哪几个路径;`full` 再加 daemon 健康和你可见的每个项目的 24h 成本。

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
ccteam config mcp              # 注册/刷新可写配置 vendor 的 ccteam MCP;DSH 走插件粘贴凭据,Pi 走受管会话 bridge
ccteam start [--json]          # 后台起 daemon(≡ `ccteam daemon start`:setsid 脱离、等就绪;幂等)
ccteam start --web-bind 127.0.0.1:7331   # 只绑本机(免令牌;launcher flag 会转发给 daemon)
ccteam start --dsh-web-bind off          # 关闭 DSH Web 伴生监听
ccteam start --no-web | --no-imd         # 只要网关 / 只要 web
ccteam stop                    # `ccteam daemon stop` 的别名
ccteam daemon start            # 与 `ccteam start` 同一个 launcher
ccteam daemon stop [--force]   # 优雅停;--force 超时后升级为 SIGKILL(只杀 daemon)
ccteam daemon restart          # 优雅停 + 重新脱离,同一把锁(不带 flag 时重放当初的 flag)
ccteam daemon status [--json]  # pid · ready · 运行中版本 vs 二进制版本;--json = /health 身份 + 二进制路径
ccteam daemon logs [-f] [-n N] # 看 / 跟踪 ~/.ccteam/daemon.log
ccteam update [--now] [--no-restart] [--json]   # 原地更新,然后把 daemon 重启到新二进制上
ccteam update --channel npm --binary <path>     # 把一个已下载的 ccteam(如 DSH 插件的平台包)经同一条落点阶梯装上 + 同一套重启合同
ccteam status                  # daemon 心跳 + 各项目及其会话 + web 链接 + 版本 / 更新提示
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
ccteam doctor --verify-mcp     # MCP 表面验收(6 工具 / 0 stub,漂移退出码 1)
```

只重启 daemon(会话在重启后按 id 自动接回;正在跑 turn 的会话会被等到跑完,绝不重复起进程):

```bash
ccteam daemon restart              # 或 make daemon-restart(先重编译再重启)
```

状态文件速查(`~/.ccteam` 按职责分组:`secrets/` 凭证、`state/` daemon 写的、`cache/` 可删、`run/` 套接字):

```bash
ccteam daemon logs -n 120                        # daemon 日志(~/.ccteam/daemon.log;或 make daemon-logs)
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
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=... CCTEAM_GROK_BIN=... CCTEAM_OPENCODE_BIN=... CCTEAM_DSH_BIN=...
# 覆盖 vendor CLI 路径
```

### 更新

```bash
ccteam update                      # 原地更新,然后把 daemon 重启到新二进制上
ccteam update --no-restart         # 只换二进制;之后自己 `ccteam daemon restart` 生效
ccteam update --now                # 跳过等待在飞 turn 的排空,立即重启
```

`ccteam update` 会识别 ccteam 是怎么装的,再走同一条安装路:一键脚本 / 预编译安装重放 `install.sh`(同一套下载 + SHA-256 校验 + 原子替换;没有第二个下载器)。源码 checkout 不会替你重新编译:它只打印 `git pull && make install`。二进制换好后,若有托管 daemon 在跑,`update` 会等在飞的 turn 空闲(最多 5 分钟;`--now` 跳过),把 daemon 优雅重启到新二进制上,并核对运行中版本一致。

**`ccteam update --channel npm --binary <path>`** 是同一套更新,只是二进制已经在磁盘上 —— DSH 插件的「更新引擎」按钮就是拿它平台包 `@ccteam/engine-<os>-<cpu>` 里的 `bin/ccteam` 跑这条命令,你也可以拿任意 ccteam 二进制自己跑。它先检查 `<path>` 是可执行文件且能回答 `--version`,再经与 `install.sh` 相同的落点阶梯装入(`CCTEAM_INSTALL_DIR` → `ccteam` 现在所在的目录 → `~/.local/bin`;目标是软链或归包管理器所有时拒绝并说明,绝不覆盖;`--binary` 指向已装文件本身时报 `alreadyInstalled`),记录安装渠道,然后跑同一套「排空 + 优雅重启 + 版本核对」合同(`--no-restart`、`--now` 同样适用)。`--binary` 只接受 `--channel npm|bun|pnpm`;standalone 渠道自己下载。

重启永远不会把 daemon 搬到别处:`ccteam update`(与不带 flag 的 `ccteam daemon restart` 一样)重放 launcher 记录的 flag,daemon 早于这份记录时则用它在 `/health` 上报的绑定地址。两者都拿不到 —— 既无 launcher 记录、`/health` 也不应答(用 `--no-web` 起的 daemon,或早于身份面的旧构建)—— 就只换二进制、**拒绝**重启而不是瞎猜,并在提示里写出该跑的那条重启命令:带上 daemon 正在用的 flag 执行 `ccteam stop && ccteam start --web-bind <addr> [...]`,或 `ccteam daemon restart --web-bind <addr>`。

daemon 重启对活会话意味着什么 = 按 id 恢复的合同,再加一条规则 —— **一个会话一个进程**:`terminal`/tmux 会话照常跑(独立进程树);默认 `stream-json` 会话的进程是被「放手」而不是被杀 —— 空闲的自己退出,turn 进行中的跑完这个 turn。新 daemon 凭 body 记录(`<project>/.ccteam/chat/<sid>/body.json`)认出这样的幸存者,绝不为同一会话再起第二个进程:会话显示为 `detached`(web 侧栏、`agent_read activity:detached`、IM `/sessions`),发给它的消息排队、等该进程一退出就投递,`/stop` / `agent_stop` 立即结束它;它退出后 ccteam 从 Claude 自己的 transcript 里找回它这段时间给出的回答并投递(IM/web 回复、委派通知),再按 id 重建会话。ACP(grok/kimi/opencode)与 codex 进程随 daemon 一起结束;在飞的 turn 被打断,下条消息按 id 恢复上下文。`ccteam status` 与 `ccteam doctor` 显示安装渠道、运行中 vs 二进制版本、以及是否有新版本(惰性检查,最多每 ~20h 一次;`preferences.toml` 的 `check_for_update` 可关)。**卫星自己更新** —— 在每台上跑 `ccteam update`;控制台的主机页与 `ccteam status` 会标出版本落后于 daemon 的主机。

### 多机(卫星节点)

每台机器都跑同一个 `ccteam start`。join 过另一台 daemon 的节点就是它的**卫星**——之后由卫星**主动出站**连接 daemon(反向连接):只有 daemon 需要可达地址/端口(`:7331`),卫星零暴露、在 NAT/防火墙后也能用。给 daemon 前置 HTTPS 反代即可全链路 wss。

```bash
# daemon 侧(或 web 控制台 → 主机页):铸 join token
ccteam host mint-token --daemon http://daemon-host:7331 --web-token <admin-hex>

# 卫星侧(任何跑着 ccteam start 的机器):
ccteam host join --daemon http://daemon-host:7331 --token <join-token>
# 本机运行中的 daemon 30 秒内自动拨出上线。

ccteam host ls                     # 查看本机卫星凭据(如已 join)

# 反注册卫星(团队 → 分工 名册里每台主机也有「移除」按钮)。在线主机须 --force;
# local 永不可删(它就是主 daemon 本机)。
ccteam host rm <host-id> --daemon http://daemon-host:7331 --web-token <admin-hex> [--force]
```

卫星每 ~25s 经控制通道上报 agent 探测与已注册项目;主机页实时显示在线状态。**项目绑定主机** —— 要在卫星上跑会话,先让它拥有一个项目,然后往那个项目里 spawn(不再按会话选主机):

- **远程新建**:web 控制台 → 新建项目 → 主机选择器选那台卫星 → 填它机器上的绝对路径,daemon 请卫星就地 bootstrap 并注册。
- **接入既有 checkout**:在那台机器仓库里 `ccteam init`,然后主机页对上报的项目点「接入」。slug 撞名会得到独立 catalog slug(`demo` → `demo2`)——跨机 slug 相同不代表同一项目。

远程执行当前支持 Claude stream-json 会话;连接断了自动退避重连,exec 链路断开后下次 spawn 经 vendor `--resume` 续上下文。Pi 和 DSH 都是 local-only:会话只跑在 daemon 本机,把它们 spawn 进绑定卫星的项目会直接报错,不会悄悄换到本机跑。舰队容量:一个会话超过 `sessions.idle_release_secs`(默认 3600 秒 = 一小时,对齐 Claude 的 prompt-cache TTL —— 进程只握到「再握也省不下缓存」为止;`0` = 永不释放,`sessions.idle_release_by_vendor: {claude: 7200, codex: 0, …}` 可按 harness 覆盖)没人说话,ccteam 就放掉它的 harness 进程。**释放 ≠ 结束**:transcript 还在、`/sessions` 里照样列出(带 💤)、这个 chat 的当前会话也还指着它 —— 下一条消息就按同一个 sid 恢复(约 1 秒)。在飞 turn、等待审批、harness 自报还有后台任务的会话都不会被释放。`sessions.max_live`(默认 50)保留为突发场景的硬上限:超限时优雅释放最久无活动的合格会话。只有 `/stop` 才是真正结束一个会话。

---

## 排错(卡住时)

先跑这三条,八成能定位:

```bash
ccteam doctor
ccteam status
ccteam daemon logs -n 120
```

1. **`ccteam: command not found`** — `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"`。
2. **Telegram 不回 / 日志 `drop msg from non-allowed chat`** — chat id 不在白名单,或改了凭证没重启:修 `~/.ccteam/secrets/im-credentials.json` 的 `allowed_chat_ids`(或重配 Web Settings)→ 重启 daemon。
3. **IM 报「发送失败 / 会话暂时没有产出」** — 重启 daemon 再发同一 `@handle`;长上下文先 `@bot /compact`;反复失败就 `/new` 开新会话。
4. **`/cd` / `/new` 报「项目不存在」** — 项目没初始化或 daemon 没加载:`cd <repo> && ccteam init` → 重启 daemon → `/projects` 确认 → `/cd <slug>`。
5. **Web 打不开 / 要令牌** — 用 `ccteam status` 末尾的完整 `web url`(带令牌);或 `--web-bind 127.0.0.1:7331` 只绑本机免令牌。

> IM 路径里的 Claude 会话默认 `skip`(直接执行、无批准门)—— 只把 bot 暴露给可信 chat,bot token 不要进 git。要逐工具审批,用 `/new <vendor> <role> hitl`。
