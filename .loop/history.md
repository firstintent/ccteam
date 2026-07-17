# ccteam 版本蒸馏史(`.loop/history.md`)

> 每版一行,ship 时由规划(控制)会话回填;只留「可指导未来的」,详档 = `docs-local/versions/v0-X-Y/`
> (gitignored 本机)+ `git log`。root README 不含版本进展(§三红线)——本文件是 repo 内唯一版本时间轴。

- **v0.9.3 增量**(未单独 bump)四 vendor 全局 MCP 对称注册(Claude/Codex/Grok/OpenCode 任意主会话可编排)+ `/mcp` 出 auth_layer 修委派父边 + spawn 响应 `caller` 字段
- **v0.9.2** 项目↔主机绑定(host 归 project、spawn 去 host 参、`project_init` op、卫星项目 import)· live 容量 50 LRU 挤停 · 团队拓扑树 · A2A 返回限幅防父会话膨胀 · 新建项目自由选主机
- **v0.9.1** MCP 单前缀修复(去双前缀)+ 非 ccteam 主会话 admin fallback
- **v0.9.0** Agent2Agent 底座:`session_*` 5 工具 + per-session principal `(sid,secret)` + 委派语义(路由非引擎)+ 反向连接跨机 + 引擎零内置 persona(废除 cto)
- **v0.8.x 系列** 协议轴(stream-json 默认 / terminal 维护期)· 插件市场(track-upstream + plugin type)· 多租户软分区(档0/档1)· web chat-shell 统一重写 · IM 通用模式 → `git log` + `docs-local/versions/v0-8-*/`
