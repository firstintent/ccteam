# ccteam 的 DSH 插件：安装与使用

> English: [dsh-plugin.md](dsh-plugin.md)

一个插件 `@ccteam/ccteam-ui` 把 DeepSeek Harness（DSH）和 ccteam 接起来。本文讲
怎么装它——既可以把它当成完整的 ccteam（连引擎一起装），也可以装在你已经在跑的
ccteam 旁边——以及怎么在 DSH 里用 ccteam 工作台。ccteam 自带的 **DSH** 页面、
从 ccteam 反向雇用 DSH 会话等内容，请看 [usage.md](usage.md) 的“DSH Web”一节。

这个插件的定位：把 ccteam 最核心的体验——在一个地方驱动多种
harness——带进 DSH 自己的 UI 里。它从头到尾按 DSH 客户端插件的机制构建
（DSH 槽位、DSH 组件与设计 token、DSH 多语言、DSH 设置卡），不是 ccteam
web 控制台的移植。

## 1. 你会得到什么

装一次，得到三个面和一个引擎托管：

| 面 | 面向谁 | 提供什么 |
|---|---|---|
| **工作台** | 使用 DSH Web 的人 | DSH 里的 ccteam 工作台：整页界面，含跨 harness 团队树、原生级会话（流式 Markdown、工具步骤、选择提示、附件、打断）和详情栏，入口是 DSH 自己侧栏底部的 ccteam 按钮。 |
| **工具** | DSH 会话里的 agent（LLM） | 在 DSH 会话中注册 ccteam 的 8 个 MCP 工具，让 DSH agent 也能雇用与驱动团队其他成员。 |
| **传输** | ccteam | ccteam 雇用 DSH 会话所走的 ACP 通道。只有 profile 行里带 socket 路径时才启用，而那只有 ccteam 托管的运行时才会写。 |
| **引擎** | 你 | ccteam daemon 本身：插件把 `ccteam` 二进制作为平台包一起带来，负责安装、启动，并在设置卡最上面给出「引擎」段（状态、版本、启动 / 停止 / 重启 / 更新引擎）。 |

三个面各自独立生效：没有 DSH web app 的 profile 照样拿到工具面，没有 agent
运行时的 profile 照样拿到工作台。

## 2. 两种安装方式，一个 daemon

ccteam 是一个二进制引擎、两个安装面——`ccteam` CLI 和这个插件。二者把二进制装
到同一个位置，用同一个 `$CCTEAM_HOME`（默认 `~/.ccteam`），跑的是**同一个
daemon**。从哪边开始都行，另一边之后会自动接上（见 §3）。

### 2.1 方式 A——插件自带引擎

不需要先装任何东西。在你的 `dsh web` 实际使用的 profile 中执行：

```bash
dsh plugin --profile web add @ccteam/ccteam-ui
```

然后重启对应的 `dsh web` 进程，再在浏览器中强制刷新（Ctrl+Shift+R 或
Cmd+Shift+R）。插件启动时会：

1. **安装引擎。** 二进制以 `optionalDependencies` 平台包
   `@ccteam/engine-<os>-<cpu>` 的形式随插件而来（`linux`/`darwin` ×
   `x64`/`arm64`；npm 和 pnpm 只会下载匹配本机的那一个，而且这些包没有任何
   lifecycle 脚本）。插件把其中的 `bin/ccteam` 复制到 `install.sh` 使用的同一
   位置——`$CCTEAM_INSTALL_DIR`（若设置），否则 `PATH` 上已有 `ccteam` 所在的
   目录（先解软链），否则 `~/.local/bin`——复制件先要能回答 `--version` 才会
   被换上。目标若是软链或位于包管理器目录（`node_modules`、Homebrew、nix、
   snap），只报告、绝不覆盖；想装到别处就设 `CCTEAM_INSTALL_DIR`。
2. **启动 daemon。** 「插件启动时自动启动引擎」开着（默认）时执行 `ccteam start --json`——
   与 CLI 同一个脱离终端、幂等的 launcher——并等待 `GET /health` 就绪。若这个
   home 已有 daemon 在应答，则直接接上（§3）。
3. **自动配好凭据。** 同一台机器、同一个 OS 用户下，工作台不需要粘贴任何
   东西：插件只读一个文件 `$CCTEAM_HOME/secrets/web-token`（daemon 为自己的
   operator 写下的控制台 token），并通过 REST 向 daemon 领取工具面的
   enrollment 凭据、存进设置卡。你在卡里手填的 token 永远优先于文件。
4. **把工作台闸在引擎上**（§4）：daemon 应答之前，首屏显示「ccteam 引擎未运行」和一个
   「启动引擎」按钮；还没有项目时显示「添加工作区」。

这个开关可在设置卡里关掉；关闭后插件只探测，不安装、不启动，正在运行的 daemon 不受影响。

### 2.2 方式 B——先装 CLI

先装 `ccteam` CLI 并启动（`curl -sSL
https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh`，
然后 `ccteam start`——见 [usage.md](usage.md)）。插件随后有三种来法：

- **由 ccteam 物化（零步骤）。** 如果 DSH 是通过 ccteam 使用的——`/new dsh`、
  ccteam 的 **DSH** 页面，或 `session_spawn` 传入 `vendor:"dsh"`——ccteam 会
  把这个插件和对应凭据物化到你这个身份的 DSH 运行环境里。确认：DSH 侧栏底部
  出现 **ccteam** 按钮；ccteam 雇用的 DSH 会话能回答 `status` 工具调用。
- **从 ccteam web 注册。** 管理员打开 **Settings → Hosts**，对检测到的本机
  DSH 点击 **Register DSH plugin**；DSH 进程仍需你自己重启（ccteam 从不重启
  不是它起的进程）。
- **手动安装。** `dsh plugin --profile web add @ccteam/ccteam-ui`，然后重启
  `dsh web`。插件会找到正在跑的 daemon 并接上。

方式 B 的三种来法下，「引擎」段都显示「已挂靠」（提示行「由 CLI/其他入口启动，插件已
接管显示」）：插件不管任何不是它起的东西（§3）。

## 3. 共存：一个 `$CCTEAM_HOME`，一个 daemon

同一个 OS 用户下，装了插件的原生 DSH、ccteam web、CLI、systemd unit 共用一个
home、一个 daemon。规则如下：

- **谁先起谁赢，其余 attach。** 插件动手前先探 `GET /health`。应答里的
  `home` 与插件解析出的 `$CCTEAM_HOME` 相同，那就是*这个* daemon：「引擎」段显示
  「已挂靠」（别处起的）或「运行中」（本插件起的）。插件的 daemon 在跑时
  敲 `ccteam start`，得到的是 `alreadyRunning{pid,home}`、同一个 pid——不会有
  第二个实例，也没有前台模式可以起出第二个。
- **插件被释放不停 daemon。** DSH 重启、`dsh plugin update`、禁用插件，都只
  释放插件自己的探针，别的一概不动；你的 Telegram 网关和进行中的委派照常，
  插件下次启动重新 attach。
- **停止是显式的、整体的。** 「引擎」段的「停止」先确认（「停止引擎？」——「同时会
  停止 ccteam web 与 IM 网关；已有会话不受影响，下条消息自动恢复。」），再执行
  `ccteam daemon stop`：停的是那唯一的 daemon（agent 进程永不被杀）。「重启」同样
  先确认，因为它也会把 daemon 拉下来。systemd 托管时 unit 可能把它再拉起来。
- **home 不同只报告、不修。** `/health` 报的 `home` 与插件的不一致时，「引擎」段
  显示「家目录不一致」（首屏：「引擎家目录不一致」并列出两个 home），插件不会去起第二个 daemon；把 `CCTEAM_HOME`（或卡上
  的 daemon 地址）指向同一个 home，两边就共用一个引擎。
- **你自己装的插件绝不被重复安装。** ccteam 往你的 `~/.dsh` web profile 注册
  或物化插件时，若发现 `@ccteam/ccteam-ui` 已由你自己 `dsh plugin add` 装过，
  就只写自己的配置行——不装第二份、不加第二条 bundle 行，DSH 也就不会报
  `duplicate loader entry id`。版本与 ccteam 内嵌的那份不一致时，`ccteam
  doctor` 和 **Hosts** 页报 `plugin_version_mismatch{installed,embedded}`，
  留给你自己的 `dsh plugin --profile web update @ccteam/ccteam-ui` 处理。
- **插件不写 `~/.dsh`。** 把它装进去的是你的 `dsh plugin add`；ccteam 只追加
  自己的 override 行。
- **远端或受管 daemon 只 attach。** daemon 地址不是 loopback、运行时本身由
  ccteam 起（daemon 是它的父进程）、或 profile 行是 ccteam 带着凭据物化的——
  这些都意味着引擎属于别人：「引擎」段只显示状态和一句原因，没有按钮。

## 4. 「引擎」段、首屏与横幅

**设置卡。** DSH **设置 → 插件 → 插件配置 → ccteam-ui** 卡的最上面就是「引擎」段：

- **状态** —— 一个状态点加下列之一：「正在读取引擎状态…」·「运行中」（本插件起的）·
  「已挂靠」（别处起的；提示行「由 CLI/其他入口启动，插件已接管显示」）·「启动中」·
  「安装中」·「已停止」（已装、daemon 未跑）·「未安装」·「不支持此平台」·
  「家目录不一致」·「版本不一致」（§3 / §7）。
- **事实行** —— `引擎 v<已装版本>`（悬停显示二进制路径）、`daemon v<运行版本>`（只在
  运行中的 daemon 与已装二进制版本不同时出现）、`pid <n>`、`$CCTEAM_HOME`（中截；
  悬停显示全文）、web 绑定地址，以及 daemon 应答时的「打开 ccteam web」链接。
- **启动 · 停止 · 重启 · 更新引擎** —— 分别是 `ccteam start`、`ccteam daemon stop`、
  先停后起、以及 `ccteam update --channel npm --binary <平台包里的 bin>`。「更新引擎」
  只在它就是修法时出现（版本不一致且引擎较旧）。「停止」和「重启」都先确认——
  「停止引擎？」/「重启引擎？」——因为两者都会把 daemon 拉下来：「同时会停止
  ccteam web 与 IM 网关；已有会话不受影响，下条消息自动恢复。」更新刻意走引擎
  自己的更新器：它会等在飞的 turn 跑完、优雅重启、核对新版本——直接把文件盖到
  正在跑的 daemon 上做不到这些。
- **插件启动时自动启动引擎** —— 自动启动开关，默认开；「关闭后插件只探测，不安装、
  不启动；正在运行的 daemon 不受影响。」
- **高级** → **引擎路径覆盖**（「留空 = 先找 PATH 里的 ccteam，再找默认安装位置；
  保存后生效。」）和**当前解析到的二进制** —— 实际在用的 `ccteam` 及其来路
  （`configured` · `path` · `canonical`）。
- **引擎日志** —— `$CCTEAM_HOME/daemon.log` 的最后 50 行（与 `ccteam daemon logs`
  读的是同一个文件），带「刷新」。

插件不托管引擎时（§3），按钮换成一句说明原因的话——例如「此 DSH 由 ccteam 启动；
引擎的生命周期不由插件管理，这里只显示状态。」或「引擎不在本机；这里没有可启动或
停止的进程。」

**工作台内。** 头部有一个「引擎：<状态>」状态点；点它在工作台内打开同一个引擎面板
（DSH 没给插件跳到设置页的入口，所以面板末尾写着「引擎设置在 DSH 设置 → 插件 →
插件配置 → ccteam-ui」）；**Esc** 关闭。首次使用时工作台闸在引擎上：

- **「ccteam 引擎未运行」** —— 附原因（「引擎已安装，但 daemon 没有运行。」或
  「还没有安装引擎；启动时会从插件自带的平台包安装。」）和一个「启动引擎」按钮。
  「引擎家目录不一致」会并列插件与 daemon 两个 home，并提示「统一 CCTEAM_HOME 后
  重启 DSH」；「不支持此平台」说明「ccteam 只为 linux 与 macOS 的 x64 / arm64
  发布引擎。」
- **「添加工作区」** —— 引擎应答后、还没有项目时显示：「目录（绝对路径）」、
  「slug（可选）」（留空按目录名生成）、「添加」。DSH 自己有工作区时，会多出一个
  「从 DSH 导入」列表，每个工作区一行、一键添加；没有工作区则这个列表不出现。
- 顶部的**版本横幅**：「引擎 v… 低于插件要求 v…」配「更新引擎」按钮，或
  「插件 v… 低于引擎 v…」配可复制的 `dsh plugin update @ccteam/ccteam-ui`（DSH 要求
  带 profile，实际请执行 `dsh plugin --profile web update @ccteam/ccteam-ui`）；
  「关闭提示」隐藏它。修复是单向的——正在跑的二进制绝不会被悄悄换掉。

## 5. 配置三个面

连接设置在同一张卡上。DSH 只对它认为是「操作者本机」的浏览器显示这一页：手起
的 `dsh web` 请从 `127.0.0.1` 打开，或经 ccteam 的 DSH 页访问（ccteam 会声明
该页拥有其 Host，见 usage.md → DSH Web；对 0.1.1-rc.2 及更早版本，ccteam 把这
处读取回填进下发的客户端 bundle）；从局域网地址直接打开原生 `dsh web`，这一页
会是空白。

| 字段 | 是什么 | 什么时候填 |
|---|---|---|
| **ccteam daemon 地址** | 三个面共用的 daemon 地址；默认 `http://127.0.0.1:7331` | 只在 daemon 在别处时填（别的端口、局域网另一台机器）；非 loopback 地址会让「引擎」段变成只 attach |
| **REST API token** | 标识**你这个人**，工作台凭它读你的团队 | 同机时自动从 `$CCTEAM_HOME/secrets/web-token` 读取；只有 daemon 的 home 你读不到时才需要粘贴（ccteam web → **Settings → Account**，可不带前缀） |
| **Enrollment 凭据** | 标识**这个 DSH 进程**，它的 agent 凭此调用 ccteam 工具 | 本机 daemon 会自动领取并存在这里；daemon 在别处时从 ccteam web → **Settings → Access** 复制一枚 `ccteam-enroll:<id>:<secret>` 粘贴 |
| **默认项目**（可选） | 工作台没点名项目时新会话落到哪个项目 | 一个项目 slug；留空则每次询问 |

两个凭据**不能互换**——一个是人，一个是进程。凭据字段是只写的：卡片只显示
**已配置** / **未配置**，从不回显值；留空即保持现值。由 ccteam 启动或注册
（Hosts → 注册 DSH 插件）的实例已经带上你自己的 REST token。

## 6. 使用工作台

点击 DSH 侧栏底部的 ccteam 按钮，工作台以**停靠在 DSH 旁边**的窗格打开：
左边 DSH 自己的侧栏、会话、详情照常可用，右边同时跑 ccteam（对 DSH 而言，
这个窗格只是把窗口变窄了一点）。拖窗格左边缘可调宽度，按 **⤢** 展开成整页、
再按一次停靠回去；整个切换就是一条宽度动画。窗格内部按自身宽度自适应：
约 1240px 起三栏，约 880px 起两栏（详情为滑出面板），更窄时单栏——没选会话
时团队树占满窗格，选中后用头部的返回键回到团队，详情面板从右侧滑出覆盖在对话上。

- **团队**（左）：**新建会话**、搜索框，以及按项目分组的全部会话——
  harness 双字母格、标题、`harness · 模型 · 时间`、状态点和累计成本；委派出
  的子会话缩进在父会话下面。状态点先说活动（daemon 的判定：动画 = 工作中、
  绿 = 空闲、琥珀 = 停滞、红 = 卡住）；空闲会话再显示常驻状态——空心圈表示
  进程已在 harness 的 prompt-cache TTL 到期后被释放、下条消息按 sid 自动恢复，
  灰点（附「已停止」）表示你停掉了它；悬停行会解释当前状态。项目头可折叠并
  显示该项目的总成本；悬停项目头会出现 **⋯**（新建会话、复制 slug、仅展开此
  项目、折叠全部）和 **+**（打开新建会话页并预选该项目）——与 DSH 自己的
  工作区行同一套交互。
- **主栏**（中）：当前会话的对话；没有选中会话时是新建会话页。
- **详情**（右，头部按钮开关）：身份（sid、项目、harness、模型、effort、
  角色、主机）、状态、用量（成本、token、来自实时状态行的上下文窗口）、
  委派关系（可点跳转），以及操作——重命名、打断当前回合、停止会话（两步
  确认）、复制 sid。点击对话里的某个步骤行，这里会显示该步骤。

**新建会话**与 DSH 自己的空会话页同形：选**项目**、**harness**（未安装的
灰显）和可选的**角色**（项目 `.claude/agents/*.md`），在输入框底栏从
harness 的目录里选**模型**和 **effort**（留空即 harness 默认），写下第一件
事按 **Enter**。标题取自任务首行；会话一创建就打开。校验内联显示，daemon
的报错原文显示在输入框下方。

**对话**：用户消息是气泡；助手消息用 DSH 自己的渲染器显示 Markdown，回合
进行中实时流式，回合的步骤（工具调用、命令、文件改动、搜索、思考）以紧凑
行列在文本上方——进行中转圈，完成变绿。需要人决定的提示显示为带选项的
卡片，点一个即回答。排队会说明排在谁后面；失败显示错误类型；会话生命周
期与委派事件显示为小字提示。回合进行中发送键变成**停止**（非破坏性打断，
会话上下文保留）。**Enter** 发送，**Shift+Enter** 换行，开头输入 `/` 会
列出透传命令（`/compact`、`/new`、`/clear`、`/role`、`/model`），回形针
添加附件（图片内联显示）。**加载更早的消息**向前翻页。ccteam 只记录步骤
的名称与摘要，完整的工具输入输出在 harness 自己的会话里；步骤只在直播时
显示，不从历史回放。

**中途切换模型 / effort**：输入框底栏的 `harness · 模型 · effort ▾` 控件列出
该 harness 的模型（来自 `/models` 目录）和 effort 阶梯；选一个就发送与人手输
一样的 `/model <id> [effort]` 指令——由 harness 自己完成切换并回一条回执行，
标签随实时状态行刷新。这是所有入口（IM、ccteam web、MCP、DSH）共用的同一条
路；harness 本身不能切换。

**会话行**带与 DSH 自己一样的 **⋯** 菜单：打开、重命名（行内）、复制 sid、
打断当前回合、详情、停止（有确认；停掉的会话仍可按 sid 恢复）。

**Esc** 先离开文本框，再关闭详情，整页时先停靠回去，最后关闭窗格。工作台关闭时，ccteam
按钮会显示自上次打开以来完成的回合数；打开后徽标清零。

工作台需要 DSH 0.1.0-rc.7 起提供的原生侧栏底部座位与 overlay 座位。

## 7. 排查问题

| 现象 | 处理方式 |
|---|---|
| **未连接** / **「ccteam 引擎未运行」** | 首屏点「启动引擎」（或设置卡「引擎」段的「启动」，或执行 `ccteam start`；面板也会显示可复制的命令）。「已停止」= 二进制在、daemon 没跑；「未安装」= 没有二进制——启动会先从平台包把它装上。 |
| **「家目录不一致」**（首屏：「引擎家目录不一致」） | 配置的地址上有个 daemon 在应答，但它的 `$CCTEAM_HOME` 不是插件的；面板并列两个 home。插件不会起第二个。统一 `CCTEAM_HOME` 后重启 DSH，或把卡上的 daemon 地址指向你要的那个 daemon。 |
| **「版本不一致」** / 版本横幅 | 正在跑的引擎不是本插件钉死的版本。「引擎 … 低于插件要求 …」→ 点「更新引擎」（等 turn 跑完 + 优雅重启 + 核对版本）。「插件 … 低于引擎 …」→ `dsh plugin --profile web update @ccteam/ccteam-ui`（横幅的复制片没带 DSH 要求的 profile 参数）后重启 DSH。正在跑的二进制绝不会被悄悄换掉。 |
| **「不支持此平台」** | ccteam 只为 Linux 和 macOS 的 x64 / arm64 发布引擎；其它平台什么都不装（Windows 不支持；WSL 算 Linux）。请另行安装 ccteam 并把卡指向它，或直接用 ccteam web。 |
| **安装被拒：目标是软链 / 归包管理器** | 安装梯度落到了一个属于别人的 `ccteam`（软链、Homebrew、nix、snap、`node_modules` 目录）。用那个工具更新它，或给 DSH 进程设 `CCTEAM_INSTALL_DIR=<dir>` 让插件装到别处。 |
| **401** | HTTP 请求中的 REST 形式是 `Bearer ccteam:<hex>`。卡片里的 **REST API token** 就是这枚个人 token（粘贴时不要加 `Bearer`）；**Enrollment 凭据** 是 `ccteam-enroll:<id>:<secret>`。二者是不同凭据——先确认没有把其中一个填进了另一个的框。 |
| **启动时报 `duplicate loader entry id`** | 同一个插件被插入了两次（常见于 registry 和 bundle patch 都有，或手改了 `cordis.patch.yml`）。只保留一条，删除重复项。ccteam 自己的注册绝不会在你装的那份旁边再加一条。 |
| **`ccteam doctor` 报 `plugin_version_mismatch`** | 你自己 `dsh plugin add` 的那份与这个 ccteam 内嵌的版本不一致。ccteam 不动它；执行 `dsh plugin --profile web update @ccteam/ccteam-ui`。 |
| **侧栏里没有 ccteam 按钮** | 插件需要 DSH 0.1.0-rc.7 或更新（原生侧栏底部座位与 overlay 座位）。升级 DSH 后，到 设置 → 插件 → 插件列表 确认 `ccteam-ui` 为 Enabled。 |
| **局域网明文 HTTP 异常** | 参阅 [usage.md](usage.md) 的“Access and security”安全上下文说明。 |
| **DSH 里人手打的 turn 不在 ccteam 账本里** | 这是设计如此：DSH 自己页面里的输入属于 harness 原生对话；ccteam 只记录自己路由的 turn，完整对话仍保存在 DSH 中。 |

## 8. 版本与更新

需要 **DSH 0.1.0-rc.7 或更高版本**。

**版本锁步。** 每个插件发布都钉死它一起发布的引擎版本（`package.json` 里的
`ccteam.engine`，与它依赖的 `@ccteam/engine-*` 版本相同）。没有 daemon 在跑
时，插件会先把已装二进制换到这个版本再启动；有 daemon 在跑时插件绝不碰二进
制：版本不同显示为「不匹配」，修复是单向的——引擎旧就点卡上的「更新引擎」，
插件旧就 `dsh plugin --profile web update @ccteam/ccteam-ui`。CLI 的
`ccteam update` 更新的是同一个引擎，`ccteam status` 显示「运行中 vs 二进制」
的版本。

**平台。** `@ccteam/engine-linux-x64`、`-linux-arm64`、`-darwin-x64`、
`-darwin-arm64`。Windows 没有引擎包；「引擎」段显示「不支持此平台」，不装任何东西。

更新或移除插件时，继续使用同一组 profile 命令（必须写出包名）：

```bash
dsh plugin --profile web update @ccteam/ccteam-ui
dsh plugin --profile web remove @ccteam/ccteam-ui
```

移除插件是安全的：只会删除该插件自己的条目，不会删除 DSH 会话，不会改写
DSH 的其他配置，不会停止 daemon，也不会卸载 `ccteam` 二进制（不再需要时用
`install.sh --uninstall` 卸掉）。
