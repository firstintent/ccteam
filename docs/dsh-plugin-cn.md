# ccteam 的 DSH 插件：安装与使用

> English: [dsh-plugin.md](dsh-plugin.md)

一个插件 `@ccteam/ccteam-ui` 把 DeepSeek Harness（DSH）和 ccteam 接起来。本文讲的是
手动启动的 `dsh web`，以及 DSH 页面里的 ccteam 工作台。ccteam 自带的
**DSH** 页面、从 ccteam 反向雇用 DSH 会话等内容，请看
[usage.md](usage.md) 的“DSH Web”一节。

这个插件的定位：把 ccteam 最核心的体验——在一个地方驱动多种
harness——带进 DSH 自己的 UI 里。它从头到尾按 DSH 客户端插件的机制构建
（DSH 槽位、DSH 组件与设计 token、DSH 多语言、DSH 设置卡），不是 ccteam
web 控制台的移植。

## 1. 你会得到什么

装一次，得到三个面：

| 面 | 面向谁 | 提供什么 |
|---|---|---|
| **工作台** | 使用 DSH Web 的人 | DSH 里的 ccteam 工作台：整页界面，含跨 harness 团队树、原生级会话（流式 Markdown、工具步骤、选择提示、附件、打断）和详情栏，入口是 DSH 自己侧栏底部的 ccteam 按钮。 |
| **工具** | DSH 会话里的 agent（LLM） | 在 DSH 会话中注册 ccteam 的 8 个 MCP 工具，让 DSH agent 也能雇用与驱动团队其他成员。 |
| **传输** | ccteam | ccteam 雇用 DSH 会话所走的 ACP 通道。只有 profile 行里带 socket 路径时才启用，而那只有 ccteam 托管的运行时才会写。 |

三个面各自独立生效：没有 DSH web app 的 profile 照样拿到工具面，没有 agent
运行时的 profile 照样拿到工作台。

## 2. 方式一：由 ccteam 管理（推荐，零步骤）

如果 DSH 是通过 ccteam 使用的——例如 `/new dsh`、ccteam 的 DSH 页面，
或 `session_spawn` 传入 `vendor:"dsh"`——ccteam 会自动把这个插件和对应
凭据物化到你这个身份的 DSH 运行环境里。不需要安装，也不需要手工粘贴。

可以这样确认已经生效：

- DSH 自己的侧栏底部出现 **ccteam** 按钮。
- ccteam 雇用的 DSH 会话能够回答 `status` 工具调用。

## 3. 方式二：使用你自己启动的 `dsh web`

### 3.1 安装插件

在这个 web 实例实际使用的 profile 中执行：

```bash
dsh plugin --profile web add @ccteam/ccteam-ui
```

重启对应的 `dsh web` 进程，再在浏览器中强制刷新（Ctrl+Shift+R 或
Cmd+Shift+R）。DSH 的 设置 → 插件 → **插件列表** 会把它显示为 `ccteam-ui`。

还有一个快捷办法：管理员在 ccteam web 打开 **Settings → Hosts**，对检测到
的本机 DSH 点击 **Register DSH plugin**；DSH 进程仍需你自己重启。

### 3.2 配置

打开 DSH 的 **设置 → 插件 → 插件配置**，填好 **ccteam-ui** 这张卡再点
**保存**。DSH 只对它认为是「操作者本机」的浏览器显示这一页：手起的 `dsh web` 请从 `127.0.0.1` 打开，或经 ccteam 的 DSH 页访问（ccteam 会声明该页拥有其 Host，见 usage.md → DSH Web；对 0.1.1-rc.2 及更早版本，ccteam 把这处读取回填进下发的客户端 bundle）；从局域网地址直接打开原生 `dsh web`，这一页会是空白。

| 字段 | 是什么 | 从哪里获取 |
|---|---|---|
| **ccteam daemon 地址** | 三个面共用的 daemon 地址，通常是 `http://127.0.0.1:7331` | 你自己的 ccteam daemon |
| **REST API token** | 标识**你这个人**，工作台凭它读你的团队 | ccteam web → **Settings → Account** 的开发者 REST 卡片（可直接粘贴不带前缀的 token） |
| **Enrollment 凭据** | 标识**这个 DSH 进程**，它的 agent 凭此调用 ccteam 工具 | ccteam web → **Settings → Access**，复制 `ccteam-enroll:<id>:<secret>` |
| **默认项目**（可选） | 工作台没点名项目时新会话落到哪个项目 | 一个项目 slug；留空则每次询问 |

两个凭据**不能互换**——一个是人，一个是进程。用得上哪个面就填哪个，留空
只意味着那个面会继续问你要。

凭据字段是只写的：卡片只显示 **已配置** / **未配置**，从不回显值；留空即
保持现值。由 ccteam 启动或注册（Hosts → 注册 DSH 插件）的实例已经带上你自己
的 REST token，只有手起的 `dsh web` 才需要手工粘贴。

## 4. 使用工作台

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

## 5. 排查问题

| 现象 | 处理方式 |
|---|---|
| **未连接** | 执行 `ccteam start`；面板也会显示可复制的命令。 |
| **401** | HTTP 请求中的 REST 形式是 `Bearer ccteam:<hex>`。卡片里的 **REST API token** 就是这枚个人 token（粘贴时不要加 `Bearer`）；**Enrollment 凭据** 是 `ccteam-enroll:<id>:<secret>`。二者是不同凭据——先确认没有把其中一个填进了另一个的框。 |
| **启动时报 `duplicate loader entry id`** | 同一个插件被插入了两次（常见于 registry 和 bundle patch 都有，或手改了 `cordis.patch.yml`）。只保留一条，删除重复项。 |
| **侧栏里没有 ccteam 按钮** | 插件需要 DSH 0.1.0-rc.7 或更新（原生侧栏底部座位与 overlay 座位）。升级 DSH 后，到 设置 → 插件 → 插件列表 确认 `ccteam-ui` 为 Enabled。 |
| **局域网明文 HTTP 异常** | 参阅 [usage.md](usage.md) 的“Access and security”安全上下文说明。 |
| **DSH 里人手打的 turn 不在 ccteam 账本里** | 这是设计如此：DSH 自己页面里的输入属于 harness 原生对话；ccteam 只记录自己路由的 turn，完整对话仍保存在 DSH 中。 |

## 6. 版本与更新

需要 **DSH 0.1.0-rc.7 或更高版本**。更新或移除插件时，继续使用同一组
profile 命令（必须写出包名）：

```bash
dsh plugin --profile web update @ccteam/ccteam-ui
dsh plugin --profile web remove @ccteam/ccteam-ui
```

移除插件是安全的：只会删除该插件自己的条目，不会删除 DSH 会话，也不会改写
DSH 的其他配置。
