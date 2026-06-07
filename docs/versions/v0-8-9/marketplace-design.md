# v0.8.9 插件市场 — 设计(逻辑 / 开源接入 / UI 交互)

> 回答 TG 2397:插件市场的逻辑?第三方开源仓库怎么跟 ccteam 结合?UI 交互怎么实现?
> 与既定决策一致([[prd.md]] ★ 架构决策):ccteam repo 零提示词内容;**索引 + 内容都在 ccteam-hub**;开源走 **ingestion 进 hub**(非运行时联邦)。

---

## 一、三个角色

```
┌─────────────┐   读 index + 取内容 + 装到项目   ┌──────────────┐   ingestion(同步进 hub)   ┌────────────────────┐
│  ccteam     │ ───────────────────────────────▶ │  ccteam-hub  │ ◀──────────────────────────│ 第三方开源仓库       │
│  (引擎/UI)  │                                  │  (索引+内容) │                            │ agency-agents / …    │
└─────────────┘                                  └──────────────┘                            └────────────────────┘
      │ 装到用户项目                                                       
      ▼  .claude/agents/<id>.md · .claude/skills/<id>/ · workflows/<id>/   
```

- **ccteam(引擎)**:零提示词内容。带**市场 UI + CLI + 安装逻辑**;只跟 **hub** 打交道(读索引、取内容、装进用户项目)。
- **ccteam-hub**(`firstintent/ccteam-hub`):**唯一的索引 + 内容源**。`index.json`(目录)+ `agents/` `skills/` `workflows/`(内容 = 自建 + 已 ingest 的开源)。
- **第三方开源仓库**(agency-agents 等):外部 Claude-native 插件源。**不在运行时直连**;由 ingestion 管线**同步进 hub**(复制内容 + 记来源/license),ccteam 永远只读 hub。

> 为什么 ingest-进-hub 而不是运行时联邦多源:① 单一真相 + 可离线缓存;② hub = 策展/审核点(开源质量参差);③ 稳定(上游改了不直接炸用户);④ 安全收敛(只信 hub 一个源)。代价 = 要定期 re-sync,且必须保留 license/attribution。

## 二、index.json(hub 的目录索引,schema v1+)

```jsonc
{
  "version": 1,
  "generated_at": "2026-06-07T…",        // ingestion 写
  "plugins": [
    {
      "id": "code-reviewer",              // 唯一,[a-z0-9_-];= 装到项目后的 stem
      "type": "agent",                    // agent | skill | workflow
      "name": "Code Reviewer",
      "description": "逐行 review + 安全检查",
      "path": "agents/code-reviewer.md",  // hub 内相对路径(取内容用)
      "content_sha": "…",                 // 内容指纹(更新检测 + 校验)
      "source": "agency-agents",          // builtin | <开源源名>
      "upstream": "https://github.com/…/code-reviewer.md",  // 开源出处(可点)
      "license": "MIT",
      "tags": ["review","security"]
    }
  ]
}
```

ccteam 只认这个 schema;不管插件原本来自哪,进了 hub 就长这样。

## 三、第三方开源仓库怎么结合 = ingestion 管线

**配置**:hub 里一份 `sources.json` 声明要接的开源源:
```jsonc
{ "sources": [
  { "name":"agency-agents", "repo":"https://github.com/…/agency-agents",
    "license":"MIT", "ref":"<pin 的 commit sha>",
    "map":[ {"type":"agent","glob":"**/*.md"} ] }   // 该仓的布局 → 插件类型映射
]}
```

**同步**(一个 `ccteam-hub sync` 脚本 / GitHub Action / 或 `ccteam internal hub ingest`):
1. 按 `ref`(pin 的 sha,非浮动)clone/拉取源仓;
2. 按 `map` 的 glob 找出插件文件;
3. **verbatim 复制内容**进 hub(`agents/`/`skills/`/`workflows/`),`id` = sanitize stem 到 `[a-z0-9_-]`(撞名加后缀);
4. 算 `content_sha`,写 `index.json` 条目(带 `source`/`upstream`/`license`);
5. **保留 license + attribution**(hub 里留 `LICENSES/` 或每条目 license 字段;开源 MIT 要带版权声明);
6. 幂等:re-sync 只更新变化的条目(按 sha)。

**安全**(沿用 v0.8.7 role-import 的教训 R-L4/5/6):pin sha 不跟浮动 ref;size cap;不跟任意重定向;ingest 是**人工策展点**(不是任意 URL 自动进);内容 = 数据不执行。

## 四、安装流程(ccteam ↔ hub ↔ 用户项目)

1. **浏览**:ccteam(web/CLI)拉 hub `index.json`(HTTPS,github raw 或 hub 服务;**本地缓存**供离线浏览,"刷新目录" 再拉)。
2. **看详情**:点某插件 → 取 `path` 的内容(同样 HTTPS)→ **展示正文 + 出处 + license**(装前可 review —— 装进项目 = 该 persona/skill 会被 agent 执行,所以**先看后装**)。
3. **安装**:确认 → 写进**当前项目**:
   - `agent`/role → `<project>/.claude/agents/<id>.md`
   - `skill` → `<project>/.claude/skills/<id>/SKILL.md`(+ body)
   - `workflow` → `<project>/`(workflow 目录,按现有约定)
   记录已装(`content_sha`)→ 卡片变「已装」。
4. **使用**:装好的 agent/role 立即出现在「新建 session」的 role 选择里;skill 供 agent 用;workflow 可跑。
5. **更新 / 卸载**:hub 的 `content_sha` 变了 → 卡片显「更新」;卸载 = 删项目里那个文件。

> 复用现有:写入 = `write_role`/`ccteam role add` 那套(已有 sanitize + validate);agency-agents 接入 = v0.8.7 的 role-import 升级版(从"直连 github"改成"读 hub")。

## 五、UI 交互(市场浏览器)

**入口**:统一 shell 底部导航「🧩 插件市场」(原 Roles 升级;见 `prototype.html`)。

**布局**:
- 顶:**类目 tab**(Agents/Roles · Skills · Workflows)+ **来源筛选**(全部 / builtin / agency-agents / …)+ **搜索框**。
- 网格:每插件一张**卡**(名 · 描述 · 来源徽标 · license · tags · `[安装]`/`[已装]`/`[更新]`)。
- 点卡 → **详情抽屉/modal**:完整描述 + **正文预览**(.md/SKILL.md body,装前 review)+ upstream 链接 + license + 「安装到 <当前项目>」按钮(多项目时可选)。
- 安装 → 进度 + toast → 卡片翻「已装」;装的 agent 随即进「新建 session」role 下拉。
- 「刷新目录」重新拉 hub index。
- 守 v0.8.8 web UI 质量基线(四态 / 响应式 / 错误可读)。

**交互流(一句话)**:底部点市场 → 选类目/来源/搜 → 点卡看正文 → 安装到当前项目 → 立刻能在新建 session 里选用。

## 六、开放 / 决策点(待定)

1. **取内容传输**:github raw 直取(任意用户可用、需网络)vs hub 提供一个轻 API/CDN。**建议** github raw + 本地缓存(零额外基建)。
2. **ingestion 跑在哪**:hub 的 GitHub Action(定时)vs `ccteam internal hub ingest` 本地命令。**建议** hub 侧 Action(用户无感、hub 自更新);本地命令作 dev/补充。
3. **装到哪一层**:只项目级(`.claude/agents/…`)vs 也支持用户级(`~/.claude/agents`)。**建议**本版只项目级(跟现有 role 装一致)。
4. **skill / workflow 的项目内落点**:agent 明确(`.claude/agents/`);skill / workflow 的项目目录约定要和现有对齐(实现时定)。
5. **更新策略**:手动「更新」按钮 vs 自动。**建议**手动(用户掌控,装的是会执行的 prompt)。
