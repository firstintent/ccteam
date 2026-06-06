# v0.8.7 Wave 3 — role picker/import Handoff

> 直接 dev、无 PR、commit `v0.8.7:` 前缀。对应 PRD §3(Item C, DC.1–DC.3)/ dev-plan W3。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1942/0**(W2/fix 后基线 1919 +23:11 core + 7 im + 5 cli)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `doctor --verify-mcp` 17/17、0 drift(W3 加 CLI 命令、非 MCP 工具)。
> **Live e2e 验证**:`ccteam role add backend-development-backend-architect --as smoke-backend` 真从 raw.githubusercontent 拉取 + verbatim 写入 `.claude/agents/`。

## 概要
`ccteam role search/add/list` 离线浏览 + 一键装 agency-agents role 进 `.claude/agents/`,装完 `/role` 即用。catalog = **vendored 全量 192 entries / 78 divisions**(github.com/wshobson/agents,MIT)。

## Decided
- **Catalog 源 = `wshobson/agents`**(README:26「agency-agents」库,MIT,tree sha `cf6059d0`)。GitHub 可达 → **跑全量 sweep**:`gh api git/trees/HEAD?recursive=1` 过滤 `plugins/<div>/agents/*.md` = 192 文件,逐个取 YAML frontmatter(name+description),**192/192、0 错**。entry `{id,division,display_name,description,raw_path}`(无 body)。manifest = `ccteam-core/src/templates/agency_agents_catalog.json`(90KB,`include_str!`)。
- **`id` = frontmatter `name`**(已 division 前缀、全局唯一)sanitize 到 `[a-z0-9_-]`;裸 stem 拒绝(`backend-architect` 在 6+ division 撞名)。
- **拓扑分层(覆盖 PRD 字面 "core 加 reqwest")**:纯 catalog(parse/search/find_by_id/sanitize)留 **ccteam-core**(leaf,**零 reqwest**);网络 `import_role_from_catalog{,_with_base}` 落 **ccteam-im**(已有 reqwest + `*_with_base` 先例)。理由:给 leaf 加 async HTTP dep 会逼所有 dependent 编译它,破 leaf 纯度红线。
- **import flow**:`catalog_find_by_id`(离线)→ `sanitize_role_stem`(`--as` 覆盖,否则 display_name)→ exists-check(非 `--force` 拒覆盖)→ `reqwest GET {base}/{raw_path}`(默认 `AGENCY_RAW_BASE=raw.githubusercontent.com/wshobson/agents/HEAD`)→ `ccteam_core::write_role` **verbatim**(零 frontmatter 转换;agency .md 本就 Claude-native;`write_role` 首个生产 caller)。honest 错误 `ImportError::{UnknownId,Exists,Http,BadStatus,EmptyBody,Write}`。
- **CLI**:新顶层 `Command::Role { Search, Add, List }`(**非**扩 `session role`——后者是 daemon 内 live 切换,不同 noun;gateway 错误本就承诺 `ccteam role add`)。`role search <q> [--format text|json]`(离线,无匹配 exit 0 + hint)· `role add <id> [--as <role>] [--project <slug>|cwd] [--force]`(成功打印 `/role <role>` hint)· `role list [--project] [--format]`(wrap `list_roles`,空/未 init = 友好提示非 error)。
- **sanitize 落 importer**(新 `ccteam_core::sanitize_role_stem` 做 lowercase+collapse;`validate_bot_name` 是**拒绝**非转换;`write_role` 末端再校验)。
- `AGENCY_RAW_BASE` 钉 HEAD(upstream 无 semver tag);commit-sha pin 记为复现杠杆;base_url override 给测试确定性。

## Rejected
- 不给 ccteam-core 加 reqwest(破 leaf;放 im)。
- 不扩 `session role`(noun 不同)。
- catalog search 不联网(纯 manifest)。

## Risks
- `AGENCY_RAW_BASE=HEAD` 漂移:upstream(2026-06-05 push)若在 manifest re-sweep 前 rename/删文件,baked raw_path 会 404 → import 报 honest `BadStatus 404`,修法 = 重 sweep(chore)。可钉 commit sha 求完全复现。
- manifest 192/78 ≠ PRD「~209」—— upstream 从 flat 重组成 `plugins/<div>/agents/`;取当前 HEAD(今日忠实)。
- `role add` live 网络仅手动 smoke 过(自动测试全用 mock-server + 离线,by design 无 live net)。
- 跨 division 同 display_name → 两个 id 可 sanitize 到同名文件;无 `--as` 时第二次 import 命中 `Exists`(安全拒绝),`--as` 消歧(search hint 已说明)。

## Files
- **新增**:`ccteam-core/src/templates/agency_agents_catalog.json`(192 entries)、`ccteam-core/src/role_catalog.rs`(+11 测试)、`ccteam-im/src/role_import.rs`、`ccteam-im/tests/role_import_test.rs`(7 mock 测试)、`ccteam-cli/tests/role_command_test.rs`(5 测试)。
- ccteam-core:`lib.rs`(re-export catalog API + `sanitize_role_stem`)。ccteam-im:`lib.rs`(`pub mod role_import`)。ccteam-cli:`main.rs`(`Command::Role` + `RoleCommand` + `run_role`)、`commands.rs`(`run_role_{search,add,list}` + `resolve_project_dir`)、`tests/cli_surface_test.rs`(t01 加 role)。

## Remaining
- **Web 跟进**(PRD DC.3,显式推后):`GET /api/v1/catalog/roles?q=` + `POST /api/v1/projects/{slug}/roles/import` —— 非 W3 scope(MVP=CLI)。**W5 OpenAPI 前若加这俩端点,需纳入标注。**
- 可选 IM picker `/role-search` `/role-add`(ChoicePrompt 两步,PRD 称 optional)未建。
- **W6 docs**:usage.md `ccteam role search/add/list`;tech-design「协议→代码」加 role catalog/import 指针。
- manifest 是时点快照;若要周期刷新,加文档化 refresh script/Make target(当前是手动 gh-api sweep,recipe 在 role_catalog.rs module docs)。
