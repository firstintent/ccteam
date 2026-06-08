# v0.8.11 PRD — 市场改为「跟踪 upstream」(track-upstream marketplace)

> **状态:DRAFT,doc-first**。独立市场版本(user TG 2430「独立」)。实现交 dev session,本文作者**只写文档不开发**。
> **来源**:user TG 2428「跟踪源码 url,而不是拷贝副本,整体跟踪仓库,而不是同步每个文件。全量」+ TG 2430(availability 语义澄清 + 独立版本 + 直接推送)。
> **关系**:**取代**已 ship 的 v0.8.9 市场「vendor 拷贝」模型(见 `docs/versions/v0-8-9/marketplace-design.md`,本版 supersede 之);与 v0.8.10(硬化)、v0.9.0(多机)**正交** —— 不能塞进 v0.8.10(那版铁律=只硬化不加功能),版本号 v0.8.11 仅占位、相对排序灵活(user 定)。
> **代码锚点(as of 当前 dev,实现时先 `git rev-parse origin/dev` + 重 grep)**:ccteam-hub `scripts/sync.py` + `sources.json` + `index.json` + `.github/workflows/sync.yml`;引擎 `crates/ccteam-im/src/hub.rs`(`HubIndex`/`HubPlugin`/`fetch_index`/`fetch_plugin_body`/`install_plugin`/`installed_status`)+ `ccteam_core::{catalog_raw_url, write_role, write_skill, agent_md_path, skill_md_path, sanitize_role_stem}`。

---

## 〇、一句话

市场从「把每个插件文件**拷进 hub**」改成「**跟踪 upstream 仓库**」:一个 source = 一个仓库 @ pinned-sha;hub 的 `index.json` 只存**元数据 + 指向 upstream 源 URL**(零 vendored 副本、整仓跟踪非逐文件、**全量**);引擎在**安装时**从 upstream 拉内容并 vendoring 进**用户自己的项目** `.claude/`。这顺带让**目录型 / 多文件 skill**(SKILL.md + `scripts/`)天然可装 —— 正是 v0.8.9 vendor-copy 模型卡住的地方。

---

## 一、install 语义(关键 —— 直接回答 user TG 2430 Q1)

- **install = 一次性把 upstream 内容拷进用户项目**(`.claude/agents/<id>.md` 或 `.claude/skills/<id>/…`,含多文件)。装完即**本地、永久、vendor 原生**(Claude 自读 `.claude/`),运行期**不再依赖 upstream**。
- **已安装的永不被删**:市场只**往用户项目里加**,从不**删**项目里的东西(删只能是用户自己 `rm`)。upstream 删库 / force-push / 市场变更 **碰不到**已装文件。
- **upstream 删库 / force-push 之后**:① 该插件**装不了新的**(从 upstream@sha 拉 → 404);② 下次 sync 检测到该 source 在该 sha 不可达 → 从 index **剔除 / 标 `unavailable`** → 市场**不再展示**(或标「已失效」)。
- ⇒ **user 的理解正确**:删库 = 「市场不展示 + 装不了新的」,**已装的安然无恙**。availability 代价**很轻**(只损失「再装一份」的能力)→ **不需要**引擎侧缓存兜底(user TG 2430 确认接受)。

---

## 二、模型 + schema

- **source(`sources.json`)**:`{ name, repo, ref(pinned sha), license, map:[{type, glob}] }` —— 整仓 @ 一个 sha;`map` 声明哪些路径是 agent / skill / workflow。**全量** = glob 覆盖全仓该类内容。
- **index.json(只元数据 + upstream 指针,零 vendored body)**:每条
  `{ id, type, name, description, upstream, content_sha, source, license, tags[], manifest? }`
  - **去掉** hub-local `path`(指向 vendored 副本);改存 `upstream` = **可直接 raw-fetch 的 URL** @sha(`raw.githubusercontent.com/<owner>/<repo>/<sha>/<path>`)。
  - `content_sha` 保留(sha256 of 该 body @sha)。
  - **multi-file skill**:加 `manifest:[{ relpath, content_sha }]`(sync 枚举 skill 目录@sha 得到)。单文件 agent / SKILL.md-only skill 无 manifest(就一个 body)。
- **hub 不再存任何插件 body**(只 `index.json` + `sources.json` + `LICENSES/` + `README`);现有 192 个 agency-agents 也改成 upstream 指针(pre-v1.0 直接重建)。

---

## 三、ingestion rework(ccteam-hub `scripts/sync.py`)

- 不再 `shutil.copyfile` body 进 hub。改成:clone 每个 source@ref → glob 匹配 → 每条算 `content_sha`(**读** upstream 文件,**不拷进 hub**)+ 抽 name/description + 构 upstream raw URL → 写 `index.json`。
- **skill id 从目录名取**(不是文件 stem):`skills/<cat>/<name>/SKILL.md` → `id=<name>`(撞名加 `<cat>-` 前缀),`type=skill`,`tags=[<cat>]`。**修当前 file-stem 取法在 `*/SKILL.md` 上全是 "SKILL" → dup-id 崩**的 bug。
- **multi-file**:skill 条目枚举其目录@ref 的全部文件 → `manifest`(每个 `relpath` + `content_sha`)。
- **幂等 + pinned-sha 仍 byte-identical**(同 sha re-run 无 diff);Action `sync.yml`(workflow_dispatch + 周 cron → 跑 sync + commit)结构不变。

---

## 四、引擎 rework(`crates/ccteam-im/src/hub.rs`)

- `fetch_plugin_body` → 从条目的 **upstream raw URL** 拉(不是 hub base);sha256 == `content_sha` 校验**不变**。
- **multi-file 安装**:`install_plugin` 对有 `manifest` 的 skill → 逐 `relpath` raw-fetch + 校验 + 写 `.claude/skills/<id>/<relpath>`(目录型);单文件 agent / SKILL.md-only 同今天(`write_role`/`write_skill`)。
- `installed_status` 对目录型 skill:按 manifest 比对(全部文件在 + sha 匹配 = `Installed`;缺/差 = `UpdateAvailable`;无 = `NotInstalled`)。
- **安全:`hardened_client` 加 host 白名单** —— 只许从已登记 source 的 host(实际即 `raw.githubusercontent.com`)+ 精确 pinned-sha 路径拉;`index.json` 仍从 hub base 拉。既有 size cap / UTF-8 / 非空门保留。

---

## 五、安全 + 红线对齐

- 仍 **No prompt injection**:插件装进项目 `.claude/`(vendor 原生自读),ccteam **不注入**。
- **content_sha 防篡改门保留**:从 upstream 拉的 body 必须匹配 index 记录的 sha(整链:pinned-sha 锁内容 + content_sha 锁字节)。
- **host 白名单 + pinned-sha**:fetch 面收窄到「已登记 source 的 host @ 不可变 sha」—— curation gate 从「我们 vendored 了它」变成「我们登记了该仓库 + pin 了 sha + 装时 sha 校验」。
- **反转的旧理由(offline / 单源 / 信任单源)诚实记录**:换成「装时一次性 vendor 进用户项目 + content_sha 门」;offline 性质改由「装完即本地永久」兑现(§一)。

---

## 六、首个 upstream source:`mattpocock/skills`(全量)

```
{ "name": "mattpocock-skills",
  "repo": "https://github.com/mattpocock/skills",
  "ref":  "be55a7970319ede7965edbb02b5e41cba1ca82c9",
  "license": "MIT",
  "map":  [ { "type": "skill", "glob": "skills/**/SKILL.md" } ] }
```

- **全量 29 个 skill**(含带 `scripts/` 的 9 个 → 走 multi-file manifest)+ **category 作 tag**(`engineering`/`productivity`/`misc`/`deprecated`/`in-progress`/`personal`)。
- `deprecated`/`in-progress` **不删、靠 tag** 让用户在 UI 自己过滤(全量 = 整仓进,质量取舍交给浏览侧的 tag filter)。

---

## 七、迁移

- pre-v1.0,**无迁移**:dev 重建 hub —— 删 vendored agent 副本,重跑 reworked `sync.py` 生成 upstream-指针 `index.json`(agency-agents 也变指针)。已 ship 的引擎 install(vendor-copy:从 hub base 拉 body)被 upstream-fetch 取代,**直接改、不留 shim**。

---

## 八、分期(dev 实现概要)

- **P1 · ingestion rework**(ccteam-hub):`sync.py` 改 upstream 指针 + skill id-from-dir + multi-file manifest + content_sha-from-upstream;重建 `index.json`(agency-agents 转指针);hub 删 vendored body。
- **P2 · 引擎 rework**(ccteam dev):`fetch_plugin_body` 从 upstream 拉 + `hardened_client` host 白名单 + `install_plugin`/`installed_status` 支持目录型 multi-file skill;测试用确定性 fake source(本地 HTTP fixture,断 content_sha 校验 + 多文件落地 + host 白名单拒绝表外 host)。
- **P3 · 登记 + 验证**:`sources.json` 加 `mattpocock/skills` 全量 → 跑 `sync` → 验 29 skill(含多文件)browse + install 进 `.claude/skills/<id>/` 文件齐全。
- **跨 repo**:ccteam `dev`(引擎)+ ccteam-hub `main`(sync.py + sources.json + index)。每阶段 `cargo test` ≥ 基线 + clippy/fmt + vitest 不退;Workflow + opus subagent、dev/main **no-PR**、**每 Phase push**。
- **真机验证**(install → `.claude/skills/<id>/` 多文件齐全 + Claude 能用)= nas-box005;**ccteam-hub 公开是 install 真验的前置**(v0.8.10 §九 deferred 项)。

---

## 九、显式不做

- **不做**引擎侧 fetch-cache(user TG 2430 确认 availability 轻代价可接受;装完即本地永久已覆盖 offline)。
- **不做** runtime federation(仍是 ingest-time 登记 source + 装时 upstream fetch;运行时不联邦第三方仓库)。
- **不做** workflow 安装(仍 `UnsupportedType` deferred)。
- **不碰** ccteam 引擎其它红线;不引入 mDNS/自动发现类。

---

## 十、开放问题

1. **raw host 范围**:本版只 GitHub(`raw.githubusercontent.com`);其它 host(GitLab/自建)预留为 future host-allowlist 扩展?
2. **index 体积**:全量多 source 后 index 变大(只元数据,可接受);分页/分片 = future。
3. **update 流**:upstream bump sha → sync 更新 index → 用户 `installed_status=UpdateAvailable` → 重装;本版**手动**(同今天),自动更新 = future。

---

## 变更记录

- **2026-06-08 初版**:track-upstream 市场模型(TG 2428/2430)。install 语义(已装永不删、删库=不展示+装不了新、轻代价无需缓存)+ schema(upstream 指针 + multi-file manifest,去 vendored body)+ sync.py/引擎 rework + host 白名单 + content_sha 门保留 + mattpocock 全量首源 + 无迁移重建。独立 v0.8.11(排序相对 v0.8.10/v0.9.0 灵活)。dev-prompt 待 user review 本 PRD 后再出。
