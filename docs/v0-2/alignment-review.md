# V0.2 Claude-Code-native Alignment Review

> 配套 `docs/v0-2/prd.md` 的设计依据文档。基于 2026-05-07 五路并行 fork
> 研究(claude-code 设计模式 / ccteam 反模式 audit / plugin-marketplace
> 机制 / hooks lifecycle 全集 / layered settings 加载)综合产出。
>
> PRD 中各章节会 reference 本文具体段落作为决策依据。

> ⚠ **重要前提**:`references/claude-code/` 是 decompiled / reverse-engineered
> 仓(顶层 CLAUDE.md 明说)。**顶层设计模式可信**(file-system convention、
> frontmatter+body、plugin = .claude/ 打包等),但**细节级行为**(如
> async hook stdout-JSON 协议、bundled skill 解压机制)只能当作"当前
> 观察到的行为",不能当作公开协议合约。ccteam 借鉴时只依赖文档化的合约
> 字段,绕过未文档化细节。

---

## 1. Claude Code 设计哲学 — 8 条铁律

源自 Fork 1 提炼,各条带源码引证。

### 1.1 Layered config sources with explicit precedence enum

所有可扩展点(settings / skills / agents / commands / output-styles / hooks)
共用同一个 `SettingSource` 枚举,顺序定义优先级:`userSettings →
projectSettings → localSettings → flagSettings → policySettings`。merge
用 lodash `mergeWith` + custom array-concat-and-dedup customizer。

- 源码:`src/utils/settings/constants.ts:7-22`(`SETTING_SOURCES` 列表 +
  注释 "Order matters - later sources override earlier ones")
- 设计原则:精确一份 source enum 是单一真相;扩展点只在数据层用同一组
  source key 标注来源,绝不为某个扩展点单独发明优先级机制

### 1.2 File-system convention as discovery

每种扩展点都是 `<root>/.claude/<subdir>/...`;一个共用的目录-向上-walker
从 cwd 一路 walk 到 git root 或 home,沿途收集 `.claude/<subdir>` 存在的目录。

- 源码:`src/utils/markdownConfigLoader.ts:234-289`(`getProjectDirsUpToHome` —
  agents/commands/skills/output-styles 共用)
- 设计原则:目录约定 = 协议;loader 永远是"列目录 → parse frontmatter →
  包成统一对象",不写 registry-style register-by-import

### 1.3 Frontmatter declarative + body free-form

declarative 字段(`name`、`description`、`tools`、`paths`、`hooks`、
`permissionMode` …)放 frontmatter,prompt body 是自由 markdown。

- 源码:`src/skills/loadSkillsDir.ts:185-265`(16 个字段统一解析);
  `loadAgentsDir.ts:542-650`(每个非法字段 debug-log 后舍弃,不抛)
- 设计原则:frontmatter = 机器读的配置面;markdown body = LLM 读的指令面;
  新行为多数只是加 frontmatter 字段而不动 prompt 模板

### 1.4 Plugin manifest = sum of extensibility shapes

`plugin.json` 不是新协议,是把所有现有扩展点的 schema **partial-shape-merge**
成一个对象。

- 源码:`src/utils/plugins/schemas.ts:886-900`(`PluginManifestSchema =
  z.object({ ...PluginManifestMetadataSchema().shape, ...HooksPartial,
  ...CommandsPartial, ...AgentsSchema, ...SkillsSchema, ... })`)
- 设计原则:**Plugin 不是 alt-mechanism,就是把单个用户的 `.claude/` 打包**

### 1.5 Discriminated-union schema for hooks

Hook 用 `z.discriminatedUnion('type', [...])` 一刀切出 `command` / `prompt` /
`agent` / `http` 四种执行后端,每种独立 schema 但共用 matcher / event 协议。

- 源码:`src/schemas/hooks.ts:174-181`;事件名闭枚举 `HOOK_EVENTS`(27 种)
- 设计原则:扩展点协议正交分解(事件 × matcher × 执行类型)

### 1.6 Conditional / lazy activation via path globs

Skill frontmatter `paths:` glob → `conditionalSkills` map,直到 LLM 实际操作
匹配文件时才 promote 到 `dynamicSkills`(向 model 暴露)。

- 源码:`src/skills/loadSkillsDir.ts:159-178, 771-796, 861-915`
- 设计原则:扩展点在 LLM 不需要时**保持 invisible** 是一等设计目标

### 1.7 Fail-loud at user surface, fail-soft at discovery edge

Schema 错误用结构化 `ValidationError` 在 `/status` `/doctor` UI 展示;但
discovery 路径上 ENOENT/EACCES/YAML 错误**只 debug-log 跳过**,不让一个坏
文件炸掉整批扩展加载。

- 源码:`src/utils/settings/validation.ts:46-72`;`loadSkillsDir.ts:417-445`
- 设计原则:区分 "discovery resilience" 和 "user-facing diagnostics"

### 1.8 All sources collapse into unified Command/AgentDefinition

7 种来源(`'commands_DEPRECATED' | 'skills' | 'plugin' | 'managed' |
'bundled' | 'mcp' | <SettingSource>`)经各自 loader 后都被装成同一个
`Command` 对象,`source` / `loadedFrom` 字段标注出处;下游 UI / dispatcher
完全不区分。

- 源码:`src/skills/loadSkillsDir.ts:67-73`(LoadedFrom union),`createSkillCommand`
  factory(`loadSkillsDir.ts:270-401`)是所有路径汇合点
- 设计原则:Source 是元数据而非类型分支

---

## 2. Plugin / Marketplace 机制 — V0.2 §4 工厂决策依据

源自 Fork 3。Plugin 模型是 V0.2 §4 用户自定义工作流的正解。

### 2.1 Plugin 目录结构 = `.claude/` 镜像 + 可选 manifest

约定子目录(`commands/` / `agents/` / `skills/` / `hooks/hooks.json` /
`output-styles/` / `.mcp.json`)与 `.claude-plugin/plugin.json` manifest
**并存,不互斥**。manifest 字段可补充指定额外路径,自动探测的约定目录优先。

- 源码:`pluginLoader.ts:1349-1400`(`createPluginFromPath` 自动探测);
  `schemas.ts:886-900`(7 个 partial schema 拼接)

### 2.2 Discovery 不靠 ln -sf,靠 in-memory plugin pipeline

Claude Code **不**把 plugin 内容 symlink/copy 到 `~/.claude/skills/` 或
`~/.claude/agents/`。两条独立 pipeline:`loadSkillsFromSkillsDir` 扫
`~/.claude/skills/`(用户目录),`loadPluginAgents` 遍历 enabled plugins
直接从 `plugin.path` 读取,**namespace 自动加 `pluginName:` 前缀**。

- 源码:`loadPluginAgents.ts:88-90`(namespace);`loadSkillsDir.ts:638-720`
  (用户 skill 路径与 plugin pipeline 完全分离)

**对 ccteam 的关键含义**:`RECOMMENDED_AGENTS` ln -sf 8 个 plugin agent
进 `~/.claude/agents/` 是因为 ccteam 自己 spawned 的 project session **没启用
plugin pipeline**(其 `enabledPlugins` 没设置)。**真正修法不是固化 ln -sf 协议,
而是给 spawned session 写 `enabledPlugins: {"<plugin>@<mkt>": true}` 进
`.claude/settings.json`**。

### 2.3 Marketplace = 注册中心 + 远程 fetch + 缓存

Marketplace 是 plugin 元数据集合(`marketplace.json`),7 种 source:
`github` / `git` / `url` / `npm` / `file` / `directory` / `settings`(inline)。
其中 **`directory` source 让任何本地目录被识别为 marketplace**,零额外协议。

- 源码:`schemas.ts:908-1046`(`MarketplaceSourceSchema`);
  `marketplaceManager.ts:10-19`(file layout)

**对 ccteam 的关键含义**:用户 share team 时,**不需要 ccteam 自营注册中心**。
`directory` source 指向用户的 `~/.config/ccteam/teams/` 即可识别为 marketplace;
或推 GitHub repo,引用为 `source: 'github'`。

### 2.4 Plugin `userConfig` — 自定义字段一等支持

manifest `userConfig` 声明用户可配置项(type / title / description / sensitive /
min / max),enable 时通过 PluginOptionsFlow 弹窗收集,变量以
`${user_config.KEY}` 注入 MCP env / hook command / skill content。

- 源码:`schemas.ts:587-655`(`PluginUserConfigOptionSchema`)

**对 ccteam 的关键含义**:V0.2 §4 工厂的"用户填表选项"(team prompt 变量、
cost 上限、phase 顺序选择)直接 map 到 `userConfig`,免造轮子。

### 2.5 Plugin 安装路径 = `~/.claude/plugins/`,全 copy/clone,不 symlink 跨边界

基础目录 `~/.claude/plugins/`(可被 `CLAUDE_CODE_PLUGIN_CACHE_DIR` 覆盖);
git/url/npm 安装到该目录子路径。**Symlink 仅 plugin 内部,不出 plugin 边界**。

- 源码:`pluginDirectories.ts:53-63, 98-123`;`pluginLoader.ts:306-345`

### 2.6 Plugin 依赖 / 冲突 — apt-style + cycle 检测

`dependencies: ["nameA", "nameB@mkt"]`,bare 名继承 declaring plugin 的
marketplace;`resolveDependencyClosure` 装时 DFS + cycle 检测;依赖未启用 →
session-local 降级(不写 settings)。

- 源码:`dependencyResolver.ts:38-46, 60-65, 177-260`

**对 ccteam 的关键含义**:team-plugin 间复用(eg `product-research-team`
依赖 `core-rules@ccteam`)有现成语义,不必造 ccteam 自己的 team-deps 协议。

### 2.7 Plugin manifest `settings` 字段 — 安全名单制

plugin 可在 manifest `settings` 字段提供片段,但 pluginLoader **只接受
allowlist key**(当前仅 `agent`),其他被 strip。hooks 单独通过
`hooks/hooks.json` 注入;MCP 通过 `.mcp.json` / inline / MCPB / 外部 JSON。

- 源码:`schemas.ts:858-869`(注释明示 allowlist)

**对 ccteam 的关键含义**:`team.yaml` 作为 plugin 根目录的 unknown 顶级字段
(zod 默认 strip,plugin pipeline 忽略,ccteam-core 自己读)— **不要**走
plugin settings 注入路径(被 strip)。

---

## 3. Hooks Lifecycle — V0.2 §2 自循环 + §3 watchdog 落地依据

源自 Fork 4。Claude Code 27 个 hook 事件全集,本节只列 V0.2 直接用到的。

### 3.1 Stop hook = 自循环兜底正解

ccteam 现已用 Stop hook(`progress.jsonl append`)。V0.2 §2 自循环 default-on
直接基于此机制。三种控制能力:

1. **Exit code 2** → `outcome: 'blocking'` + stderr 作为 blockingError 反馈给模型,**模型被强制继续**(`hooks.ts:2784-2805`)
2. **JSON `decision: 'block'`** → 同上(`hooks.ts:608-625`)
3. **JSON `additionalContext`** → 注入到下一轮 prompt

**防递归机制**:Stop hook 二次进入时,payload 里 `stop_hook_active: true`
(`query.ts:1567`)。hook 自身负责检测此字段,避免无限 block。

**Stop hook 拿到 transcript 不用读盘**:payload `last_assistant_message`
直接是最后一条 assistant 文本(`coreSchemas.ts:525-531`)。

**对 V0.2 §2 的实施含义**:Stop hook 检查 `.ccteam/` 这一轮 phase 是否产出
phase_done / escalate / outbox 任一。三种都没有 → exit 2 + stderr 注入
"phase 未正常收尾,请输出 PHASE_DONE / ESCALATE / 写 outbox 之一"。第二次
检测到 `stop_hook_active: true` → 不再 block,append `needs_attention.outbox`
让 watchdog 接力。

### 3.2 PreToolUse 可拦截 AskUserQuestion(机制确定可行)

`AskUserQuestion` 是合法 tool_name(`AskUserQuestionTool/prompt.ts:3`)。
PreToolUse hook 配 `matcher: "AskUserQuestion"`,返回:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "本 phase 应自决。改写 outbox 询问用户。"
  }
}
```

模型立刻收到 deny 反馈走自决路径。**V0.2 把"PreToolUse 拦截视漂移数据决定"
从已知未决项升级到必做** — 机制零成本、确定可行,不必等漂移数据。

### 3.3 SessionEnd 不适合做 watchdog 触发源

`SessionEnd.exit_reason` 枚举只有 6 个:`clear` / `resume` / `logout` /
`prompt_input_exit` / `other` / `bypass_permissions_disabled`(`coreSchemas.ts:754-761`)。
**全是用户主动事件,stall 不会触发它**。

**V0.2 §3 watchdog 实施含义**:stall 探测靠 ccteam Rust orchestrator 的
**外部 timer**(已有,`stall.rs`)+ Stop hook 兜底"phase 完结但没写完成事件"。
SubagentStop 可作子 agent 级别 stall 上报点(若 phase 用 Task subagent)。

### 3.4 多 hook 并行执行,通过 `hookDedupKey` 去重

`matchingHooks.map(async function*) → Promise.all`(`hooks.ts:2278-2868`)。
没有声明的 ordering;settings.json 数组顺序仅决定 dedup 时谁覆盖谁。任一 hook
返回 `preventContinuation` / `blockingError` 即 short-circuit。

---

## 4. Layered Settings — V0.2 §5.2 三层优先级借鉴

源自 Fork 5。

### 4.1 借鉴 pattern,不照搬 schema

| 借鉴 | 不照搬 |
|---|---|
| **明确的 SourceEnum 数组**(`SETTING_SOURCES`)定义优先级 | **字段维度 deep-merge / array concat-dedup** — Claude Code 是字段独立才需要;ccteam 是整团维度,套上反而割裂"哪个 phase 在哪一层" |
| **缺层静默 fall-through**(ENOENT debug-log 不报错);只在全部 miss 才 fail-loud | **5 层** — `flagSettings` / `policySettings` 是 enterprise 特殊化,ccteam 3 层(project / user / repo)够用 |
| **读容错 + 写严格** — 读时坏 yaml 跳过该层往下 fall-back;写时 yaml 坏直接 reject 不覆盖 | **`undefined`-as-deletion** sentinel — Claude Code 因 `mergeWith` 接受 partial diff 才需要;ccteam 整团替换无需 |
| **per-source cache + 显式 invalidate** | **first-source-wins for `policySettings`** vs 其他 source last-wins 不一致 — ccteam 三层全部 first-source-wins(project > user > repo,撞名 project 赢)统一 |

### 4.2 V0.2 §5.2 三层加载架构

```rust
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // ~/projects/<slug>/.ccteam/team/
    TeamSource::User,     // ~/.claude/plugins/marketplaces/.../plugins/<team-plugin>/
    TeamSource::Repo,     // teams/<name>/  (仓内 ship 的 dev / product-research)
];

// 整团维度,first-source-wins:project 内同名 team 完全覆盖 user / repo
fn resolve_team(name: &str) -> Result<TeamSpec> {
    for source in TEAM_SOURCES {
        match source.try_load(name) {
            Ok(Some(spec)) => return Ok(spec),
            Ok(None) => continue,           // ENOENT,fall-through
            Err(e) if e.is_yaml_error() => {
                tracing::warn!("team {name} at {source:?} unreadable: {e}");
                continue;                    // 读容错,fall-back 下一层
            }
            Err(e) => return Err(e),         // 其他 IO 错严重 fail-loud
        }
    }
    Err(anyhow!("team {name} not found in any source"))
}
```

写路径(`ccteam team save` / 工厂产出)严格:目标层 yaml 坏 reject 不覆盖。

---

## 5. ccteam 反模式重构清单 — V0.2 §6 实施依据

源自 Fork 2 audit。8 个候选,按优先级排序。详细位置 / 不对齐分析见 V0.2 §6。

| # | 反模式 | 跟哪条哲学冲突 | 优先级 | V0.2/V0.3 |
|---|---|---|---|---|
| 1 | `PHASE_DONE`/`ESCALATE` 关键词在 Rust + prompt + 正文三处镜像 | §1.3 frontmatter+body | 高 | V0.2(§5.3 D 前置) |
| 2 | `render_project_claude_md` `match team` 写死团队语义 | §1.2 file-system + ccteam 红线"core 无 team 字面量" | 高 | V0.2 |
| 3 | `TEAM_BUNDLES` 编译时常量(team 注册中心反模式) | §1.2 file-system,§1.4 plugin pipeline | 高 | V0.2 |
| 4 | golden_rules team vs phase 是 xor 而非 layered merge | §1.1 layered config | 中 | V0.3 |
| 5 | meta-agent `if team == META_TEAM_NAME` 5 处分叉 | §1.6 composable primitives,§1.7 trust+fail-loud | 高 | V0.2 |
| 6 | `pre_trust_project` 写 `~/.claude.json` 侵入式 | §1.8 additive non-destructive | 中 | V0.3 |
| 7 | `RECOMMENDED_AGENTS` 8 plugin agent 写死 ln -sf | §1.4 plugin pipeline,§1.2 convention | 高 | V0.2(改 enabledPlugins) |
| 8 | `build_phase_prompt` Rust 拼协议串 | §1.3 frontmatter+body | 高 | V0.2(同 §5.3) |

### 关键观察(Fork 2 总结)

ccteam 反模式根因是**协议关键字与团队身份双重镜像**:`PHASE_DONE` /
`ESCALATE` / team 名 / `RECOMMENDED_AGENTS` 同时活在 Rust const、phase YAML、
phase markdown 正文、`teams/*.yaml`、`templates.rs::TEAM_BUNDLES` 多处。
Claude Code 哲学是"frontmatter 声明 + 文件系统发现 + 正文自由",ccteam 的
file-system 发现机制(`load_team_runtimes` 扫 `~/.ccteam/teams/`)实际已就位,
但**注册中心 `TEAM_BUNDLES` + match team 字面量 + 协议串硬编码**让 file-system
路径只能跑半路。

---

## 6. 决策汇总 — 哪些进 V0.2,哪些 deferred

### V0.2 必做(基于以上分析升级 / 新增)

- **§4 重写为 plugin 模型**(2.1-2.7):工厂产物兼容 plugin 格式,share 走
  marketplace `directory|github` source,删 ccteam 自营注册路径
- **§5.2 三层优先级**(4.1-4.2):整团维度 first-source-wins,显式
  SourceEnum,读容错写严格,per-source cache
- **§2 自循环 Stop hook 实现细节**(3.1):exit 2 + stderr + stop_hook_active 防递归
- **§3 watchdog 路径明确**(3.3):不用 SessionEnd,用 ccteam 外部 timer + Stop 兜底
- **§6 已知未决项升级**:PreToolUse 拦截 AskUserQuestion 从待评估 → V0.2 必做(3.2)
- **新加 §6 反模式重构**:候选 1/2/3/5/7/8(高优先级 6 条)

### V0.3 deferred

- 候选 4:golden_rules layered merge(team default + phase override)
- 候选 6:`pre_trust_project` 改 project-level settings.json
- conditional / lazy phase activation via path glob(§1.6 借鉴)— 等多 team
  实际场景驱动
- 反编译细节深挖(eg async hook stdout JSON 协议)— 风险高,等 Anthropic
  公开协议或更多观察样本

---

## 7. 红线检查清单(V0.2 实施时 PR 自查)

实施 V0.2 各章节时自查没破红线:

- [ ] `ccteam-core` grep 不到 `dev` / `product-research` / `meta-agent` 任何字面量
  (除注释 / 测试)— 候选 2、5
- [ ] 不再有"team 注册中心"代码;team 加载 100% 走文件系统扫描 — 候选 3
- [ ] 协议关键字(`PHASE_DONE`、`ESCALATE`)单一 source of truth(team.yaml
  字段 + orchestrator inject prompt template),不在 phase markdown 正文 /
  meta_agent_role.md / parse_phase_end.rs 任何 hardcoded — 候选 1、8
- [ ] plugin agent 不再 ln -sf 进 `~/.claude/agents/`;通过 spawned session
  `enabledPlugins` 启用 — 候选 7
- [ ] Stop hook 实施符合 stop_hook_active 防递归 — §3.1
- [ ] PreToolUse 拦截 AskUserQuestion 落实,prompt 层 + hook 层双保险 — §3.2

---

## Changelog

- 2026-05-07:初稿。基于 5 路并行 fork(claude-code patterns / ccteam
  anti-patterns / plugin-marketplace / hooks lifecycle / layered settings)
  综合产出。
