# v0.8.18 新会话开发提示词(`/goal`)

> 在一个**新的 Claude Code 会话**里(ccteam 仓库根),粘贴下面 `/goal` 后整块。它会循环开发直到验收全绿。
> 控制会话(本会话)不进工作树;dev 会话独立实现。

---

```
/goal 实现 ccteam v0.8.18(代号「loop 地基」)整版,直到下方「验收」全绿才算完成。

== 起手必读(SoT,按序读完再动手)==
- docs/versions/v0-8-18/README.md —— 本版 PRD,唯一范围权威
- docs/research/multi-user-soft-partition.md —— 多用户设计(§3 分档 + §3.5 跨端身份)
- docs/versions/v0-8-18/multi-user-onboarding.md —— 新用户流程 + 诚实边界
- 视觉 SoT:docs/versions/v0-8-18/prototype/v0818-real-shell.html(控制台+主机+头像个人设置+单语导航,SPA 照这个做)
- CLAUDE.md §一(当前状态/baseline)+ §三(架构红线)

== 交付(两柱 + UI 一致性)==
柱1 · 控制台 + 主机:
1. Status 长成控制台:扩 GET /api/v1/status 加 per-session 成本;SPA StatusView 会话卡加「成本」列。这是 loop 运维台骨架——本版只显示 session,不加 loop 列。
2. 主机页 Hosts:新 GET /api/v1/hosts(列机器,host id=local=this machine)+ GET /api/v1/hosts/{host}(详情:hostname+规格+每 agent 的 装/登录/MCP注册/version)。把 crates/ccteam-web/src/routes/capabilities.rs 从「只探 --version 二态、写死 claude/codex」升级成 host-keyed 报告 + vendor-可扩展(AgentVendor + 每 vendor 一个 ProbeSpec 数据)+ 手动 re-probe(破 daemon-终身 cache)。写端点 POST /api/v1/hosts/{host}/register-mcp(唯一可写,幂等,复用 mcp_serve::install_mcp / install_codex_mcp)。SPA 新增「主机」导航页(顶部 hostname 条 + agent 卡,沿用 StatusView 卡片样式)。

柱2 · 身份(多用户软分区):
3. 档0(必做):ACL 收 own-only —— gateway.rs(chat_can_access,约 :1219)去掉「同 project 互看」+「web-operator 通看」两条漏,session/project 仅 own(+ 显式 share)可见。ProjectState(crates/ccteam-core/src/state.rs)加 owner: Option<String>(serde-default,旧 state.json 照载);项目创建时按身份记 owner;项目列表 / /cd / web 项目视图按 owner 过滤;session 归属从所属 project 继承(GatewaySession.owner 已有,作交叉校验)。归属是显式字段、不看路径(同账号共用 /home/ubuntu,无 ~/username)。
4. 档1(选配,时间允许再做):per-user web token —— crates/ccteam-web/src/auth.rs 的单 expected 比对改成 {token→身份} 查表;IM /web 返回个人链接(复用 TokenEntryPage 的 ?token= URL-shim)。

UI 一致性:
5. SPA:界面语言只 中文 / English(无双语),默认中文,存 per-user(随身份)。点头像 → 个人设置弹层(显示名 / 头像 / 界面语言 / 登出)。全局 Settings 页 = IM token + 预算 + 用户管理(列租户 + 入口 ccteam user add)。导航标签随语言渲染(中文默认,可切英文)。视觉照 v0818-real-shell.html。

== 红线(不得破,违反即回退)==
- 绝不碰 .claude/settings.json(ccteam hook 只写 settings.local.json)
- ccteam executes nothing —— 除「自身 MCP 注册」外不执行任何 vendor 命令;绝不从 web 写 vendor 登录/key、绝不从 web 装 CLI
- No prompt injection(不碰 spawn 注入路径)
- 软隔离诚实标注「非安全」(同 OS uid 仍互读);不拆进程 / 不开沙箱 / 不动 OS
- 不在 Rust 里实现 loop / 编排 / 路由;不动 session 存储 / pane / turns / sid 键

== 不做(留给 loop 版 / 后续)==
loop 运维台的「预言机/门」列、on-ramp loop-skill 库、oracle-diff 门、loop 版本管理、全量 UI i18n(每页每条提示翻译)、真·分布式多 host 调度(本版 host 只「列」,不起跑/路由)、CLI ccteam user 的完整子命令(档1 未做则只留 admin 入口占位)。

== 验收(停止条件,全部满足才完成)==
- cargo test --workspace --exclude ccteam-web 通过数 ≥ 当前 baseline(CLAUDE.md §一,现 2016/0),0 fail
- cargo clippy --workspace --all-targets -- -D warnings:0
- cargo fmt --all -- --check:干净
- ccteam-web:vitest + Playwright 不退(各自现 baseline)
- GET /api/v1/hosts:claude 装/codex 缺的机器 → host=local,claude=ready + codex=not_installed,带 version 串;POST .../register-mcp 幂等(重复调不报错)
- GET /api/v1/status:含 per-session 成本字段;SPA 会话卡渲染成本列
- ACL:两个不同 chat_id 的 session 互不可见(测试用确定性假 chat_id);旧「同 project 互看」漏堵上
- 探测/分区测试用 CCTEAM_CLAUDE_BIN / CCTEAM_CODEX_BIN 假脚本 + 假 chat_id,不依赖真 binary、不碰真实 ~/.claude.json
- SPA(vitest):头像弹个人设置、语言中/英切换、主机页渲染、控制台成本列

== 纪律 ==
- 每个子改动配测试;改 ccteam-core 公共 API(slug/owner/probe 签名等)先 grep 全 caller(tests / mcp_serve.rs / commands.rs / ccteam-web routes)
- env-mutating 测试(set_var CCTEAM_HOME 等)放 crates/*/tests/*.rs integration,不放 lib 内联 mod tests
- 测 bootstrap 前先 disable_tool_surface_bootstrap_for_tests()
- commit 英文、文档/agent 中文;cargo fmt --all 进每次 verify;直接在 dev 提交推送,分步落(后端 hosts/status → SPA 主机+控制台 → ACL own-only → UI 语言/头像/Settings)

== 收尾(版本完成时)==
- workspace Cargo.toml version → 0.8.18;CLAUDE.md §一 baseline + 「当前在做」回填本版;docs/tech-design.md 同步新协议(/api/v1/hosts、ACL owner、status 成本)+ 末尾「协议→代码」指针表;root README.md(英文,不含版本进展)+ docs/usage.md 融入新能力;docs/versions/v0-8-18/ 落 handoff
- 不发 git tag(等 owner review)

按 PRD「落地姿势」三步走,分步提交,每步 verify 不退 baseline。完成后汇总改了哪些文件 + 验收结果。
```

---

## 备注(给 owner)

- 这是 `/goal`(loop-until-done):dev 会话会反复「改→验→修」直到验收全绿或触限。配 `/loop` 心跳或直接 `/goal` 单跑都行。
- 控制会话(本会话)在 dev 会话跑时不碰工作树;有问题在这边问。
- 档1(per-user web token)标了「选配」——若 dev 会话时间/复杂度吃紧,先交档0,档1 下个 patch 补,不阻塞本版。
- ship gate 收尾那段会让 dev 会话顺手 bump 0.8.18 + 同步 tier-1 文档;tag 仍 HOLD 等你 review。
