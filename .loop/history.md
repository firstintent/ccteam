# ccteam 版本蒸馏史(`.loop/history.md`)

> 每版一行,ship 时由规划(控制)会话回填;只留「可指导未来的」,详档 = `docs-local/versions/v0-X-Y/`
> (gitignored 本机)+ `git log`。root README 不含版本进展(§三红线)——本文件是 repo 内唯一版本时间轴。

- **v0.9.6**(dev,未合 main)多 vendor 编排发现面:status vendor 面板(probe 下沉 core `host_registry`、daemon-aware caller 收敛)+ advisory 模型目录三源(runtime last-seen `model_catalog.rs` / hub `models.json` / 用户注释,零 spawn 校验)+ routing notes(`<project>/.ccteam/routing.md` 覆盖全局、global 缺失生成不覆盖)+ spawn 失败发现面 · **compare 全链退役**(−1716 行,无 wrapper)+ `is_claude_family`/`model_warning` 删除 · README 吉祥物 logo 入 web(favicon/PWA 全套)· docs 模型路由章 · cct-codex/cct-grok wrapper skill 退役(hub 下架 8024ec4,MCP instructions 原生覆盖)+ 快速开始模板重设(多模型对比/跨 vendor 互审替换 plan/tour);owner 直驱,opus/codex/kimi 三 agent 分工 + fable5 规划 review,基线 lib 1472 / web 325(+3 env-flake)/ vitest 389
- **v0.9.5** kimi-code 第五 vendor harness(`kimi acp` 长驻 stdio 薄壳,复用共享 ACP core:resume→load→new 阶梯 · skip auto-allow / hitl fail-closed · roleless-only · remote NotImplemented)· 五 vendor 全局 MCP 对称注册(+`~/.kimi-code/mcp.json`)· cost None 仿 opencode · 真机 smoke 全链路(spawn/dispatch/collect + live `/model`);dev = kimi-code 单会话一口气(owner 钦点),规划复核确认基线 1433/0
- **v0.9.3 增量**(未单独 bump)四 vendor 全局 MCP 对称注册(Claude/Codex/Grok/OpenCode 任意主会话可编排)+ `/mcp` 出 auth_layer 修委派父边 + spawn 响应 `caller` 字段
- **v0.9.2** 项目↔主机绑定(host 归 project、spawn 去 host 参、`project_init` op、卫星项目 import)· live 容量 50 LRU 挤停 · 团队拓扑树 · A2A 返回限幅防父会话膨胀 · 新建项目自由选主机
- **v0.9.1** MCP 单前缀修复(去双前缀)+ 非 ccteam 主会话 admin fallback
- **v0.9.0** Agent2Agent 底座:`session_*` 5 工具 + per-session principal `(sid,secret)` + 委派语义(路由非引擎)+ 反向连接跨机 + 引擎零内置 persona(废除 cto)
- **v0.8.x 系列** 协议轴(stream-json 默认 / terminal 维护期)· 插件市场(track-upstream + plugin type)· 多租户软分区(档0/档1)· web chat-shell 统一重写 · IM 通用模式 → `git log` + `docs-local/versions/v0-8-*/`
