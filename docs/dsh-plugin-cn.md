# ccteam 的 DSH 插件：安装与使用

> English: [dsh-plugin.md](dsh-plugin.md)

这两个独立插件把 DeepSeek Harness（DSH）和 ccteam 接起来。本文讲的是
手动启动的 `dsh web`，以及 DSH 页面里的 ccteam 面板。ccteam 自带的
**DSH** 页面、从 ccteam 反向雇用 DSH 会话等内容，请看
[usage.md](usage.md) 的“DSH Web”一节。

## 1. 你会得到什么

| 插件 | 面向谁 | 提供什么 |
|---|---|---|
| `@ccteam/dsh-client` | DSH 会话里的 agent（LLM） | 在 DSH 会话中注册 ccteam 的 8 个 MCP 工具；同时提供 ccteam 雇用 DSH 会话所需的传输通道。 |
| `@ccteam/dsh-team` | 使用 DSH Web 的人 | 在 DSH 里提供 ccteam 面板：跨 vendor 会话树、内嵌聊天、一键创建会话。入口是 DSH 自己侧栏底部的 ccteam 按钮。 |

两个包互不依赖，可以只装一个，也可以一起装。

## 2. 方式一：由 ccteam 管理（推荐，零步骤）

如果 DSH 是通过 ccteam 使用的——例如 `/new dsh`、ccteam 的 DSH 页面，
或 `session_spawn` 传入 `vendor:"dsh"`——ccteam 会自动把两个插件和对应
凭据物化到你这个身份的 DSH 运行环境里。不需要安装，也不需要手工粘贴。

可以这样确认已经生效：

- DSH 自己的侧栏底部出现 **ccteam** 按钮。
- ccteam 雇用的 DSH 会话能够回答 `status` 工具调用。

## 3. 方式二：使用你自己启动的 `dsh web`

### 3.1 安装插件

在这个 web 实例实际使用的 profile 中执行：

```bash
dsh plugin --profile web add @ccteam/dsh-client
dsh plugin --profile web add @ccteam/dsh-team
```

重启对应的 `dsh web` 进程，再在浏览器中强制刷新（Ctrl+Shift+R 或
Cmd+Shift+R）。如果要让 DSH agent 调用 ccteam 工具，就安装 client；
如果要在人用的 DSH 页面里打开团队面板，就安装 team。两者都装即可获得
完整的双向连接。

还有一个快捷办法：管理员在 ccteam web 打开 **Settings → Hosts**，对检测到
的本机 DSH 点击 **Register DSH plugin**。这个按钮注册的是
`@ccteam/dsh-client`；DSH 进程仍需你自己重启。

### 3.2 分别配置凭据

打开 DSH 的 **Settings**，填写对应插件自己的设置卡：

| 插件 | 需要填写 | 从哪里获取 |
|---|---|---|
| `@ccteam/dsh-client` | `daemonUrl`，以及 `ccteam-enroll:<id>:<secret>` enrollment 字符串 | ccteam web → **Settings → Access**，复制 enrollment 值 |
| `@ccteam/dsh-team` | `daemonUrl`，以及个人 REST API token | ccteam web → **Settings → Account** 的开发者 REST 卡片（可直接粘贴不带前缀的 token） |

两个插件通常使用同一个 daemon 地址，例如 `http://127.0.0.1:7331`。但凭据
不是一回事：enrollment 用来标识 MCP 的 DSH 进程，REST token 用来标识面板
访问的 ccteam 账号。二者不能互换。

## 4. 使用面板

日常操作就是四步：

1. 点击 DSH 侧栏底部的 ccteam 按钮。
2. 在会话树中选择会话。会话按项目分组；活动点显示工作中、空闲或过期，
   委派出来的子会话会缩进显示在父会话下面。
3. 点击一行进入内嵌聊天。
4. 输入内容后按 **Enter** 发送；**Shift+Enter** 换行；**Esc** 返回会话树
   （在会话树视图再按一次则关闭面板）。

回执不会粉饰状态：排队会显示排队信息（如果有，还会说明排在谁后面），
失败会显示错误类型。新会话即使首个任务失败，仍会打开该会话，方便你检查
并重新尝试。

要创建新的 vendor 会话，点击会话树标题栏的 **+**。未安装的 vendor 会在
选择器中灰显；如果你只有一个可见项目，项目选择器会隐藏。**Advanced**
里可以展开设置 model、effort 和 mode；按 **Enter** 创建并进入聊天，按
**Esc** 取消。

面板关闭时，ccteam 按钮会显示自上次打开以来完成的 turn 数量。打开面板后，
徽标会清零。

较老的 DSH 如果没有新的侧栏入口，会把同一个面板降级为右侧边缘的悬浮把手。
这是预期的兼容行为，功能本身不变。

## 5. 排查问题

| 现象 | 处理方式 |
|---|---|
| **未连接** | 执行 `ccteam start`；面板也会显示可复制的命令。 |
| **401** | HTTP 请求中的 REST 形式是 `Bearer ccteam:<hex>`。插件 1 的设置要填 `ccteam-enroll:<id>:<secret>`；插件 2 要填个人 REST token。它们是不同凭据。面板设置里粘 REST token 时不要加 `Bearer`。 |
| **启动时报 `duplicate loader entry id`** | 同一个插件被插入了两次（常见于 registry 和 bundle patch 都有，或手改了 `cordis.patch.yml`）。只保留一条，删除重复项。 |
| **只有悬浮把手，没有侧栏按钮** | DSH 版本较旧，尚未提供原生侧栏入口。先用悬浮把手，或升级 DSH。 |
| **局域网明文 HTTP 异常** | 参阅 [usage.md](usage.md) 的“Access and security”安全上下文说明。 |
| **DSH 里人手打的 turn 不在 ccteam 账本里** | 这是设计如此：DSH 自己页面里的输入属于 vendor 原生对话；ccteam 只记录自己路由的 turn，完整对话仍保存在 DSH 中。 |

## 6. 版本与更新

需要 **DSH 0.1.0-rc.6 或更高版本**。更新或移除插件时，继续使用同一组
profile 命令（必须写出包名）：

```bash
dsh plugin --profile web update @ccteam/dsh-client
dsh plugin --profile web update @ccteam/dsh-team
dsh plugin --profile web remove @ccteam/dsh-client
dsh plugin --profile web remove @ccteam/dsh-team
```

移除插件是安全的：只会删除该插件自己的条目，不会删除 DSH 会话，也不会改写
DSH 的其他配置。
