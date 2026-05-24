# V0.6.6 — PRD(8 finding 完整需求)

> **Status:** doc-first(Wave 0)。每个 finding 6 段固定:**痛点 / 现状缺口 / 设计 / 文件 / 验收 / 风险**。
> **基线起点:** `1583 / 1`(V0.6.5 ship 数 + 已知 `workflow_summary_reflects_agent_spawn_and_done_events` flake)/ clippy `-D warnings` clean / workspace `0.6.5`。
> **基线目标:** `1660 / 1`(+~80,各 finding 估算 §README §0)/ clippy `-D warnings` clean / workspace `0.6.6`。

---

## F166 — GH Releases 预编译 binary + `install.sh` 一键装

**痛点:** 用户痛点 4(零摩擦上手)。
当前 README `quickstart` 要求 `cargo install --git https://github.com/firstintent/ccteam` ── 用户必须先装 Rust toolchain(rustup + 一次 `cargo install` 编译耗 5-15 min,小机器 OOM 风险)。非开发者(运维 / 业务方 / 想试 IM 助理的 PM)被卡在第一步。

**现状缺口:**
- 仓库无 `.github/workflows/release.yml`。
- 无 `install.sh`(一键 OS+arch 探测 + 下载 + 校验 + PATH 写入)。
- README quickstart Step 1 = "`cargo install --git ...`",新用户首次互动门槛过高。
- GH Releases 页面历史 tag(v0.6.0 - v0.6.5)只有 source tarball,无 prebuilt artifact。

**设计:**

### Sub-1:GH Actions release workflow

新文件 `.github/workflows/release.yml`,trigger = `push: tags: [v*]`。

```yaml
name: release
on:
  push:
    tags: ['v*']
jobs:
  build:
    strategy:
      matrix:
        include:
          - { os: ubuntu-latest,  target: x86_64-unknown-linux-gnu,    suffix: linux-x64    }
          - { os: macos-14,       target: aarch64-apple-darwin,        suffix: macos-arm64  }
          - { os: macos-13,       target: x86_64-apple-darwin,         suffix: macos-x64    }
          - { os: windows-latest, target: x86_64-pc-windows-msvc,      suffix: windows-x64  }
    runs-on: ${{ matrix.os }}
    steps:
      - checkout
      - cargo build --release --locked --target ${{ matrix.target }} --bin ccteam
      - 打包 tar.gz(linux/macOS)/ zip(windows)+ sha256 checksum
      - upload-artifact
  release:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - download-artifact
      - 汇总所有 *.sha256 到 SHA256SUMS
      - gh release create $TAG --notes-file <release-notes.md> *.tar.gz *.zip SHA256SUMS
```

**关键细节:**
- macOS notarization **不做**(stretch / V0.7)── 用户首次跑会有 Gatekeeper warning;README 给出 `xattr -d com.apple.quarantine` 指引(临时解)。
- musl static linux build **不做**(只 gnu)── glibc ≥ 2.31(Ubuntu 20.04+ / Debian 11+)覆盖足够;musl 留 V0.7 stretch。
- windows MSVC build:`ccteam-imd` 当前依赖 `tokio::signal::unix` 路径需 `#[cfg(unix)]` gate(F163 引入)── verify windows build pass,必要时加 `#[cfg(windows)]` stub。
- release notes 模板:从 `docs/versions/v0-6-6/README.md` §0 概览段抽取 + binary 下载指引 + checksum 验证示例。

### Sub-2:`install.sh`

新文件 `install.sh`(repo root),`curl ... | sh` 一行装。

```sh
#!/usr/bin/env sh
set -e
REPO="firstintent/ccteam"
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$OS-$ARCH" in
  linux-x86_64)  SUFFIX="linux-x64";   EXT="tar.gz" ;;
  darwin-arm64)  SUFFIX="macos-arm64"; EXT="tar.gz" ;;
  darwin-x86_64) SUFFIX="macos-x64";   EXT="tar.gz" ;;
  *) echo "Unsupported: $OS-$ARCH. Use cargo install --git fallback."; exit 1 ;;
esac
LATEST=$(curl -sSL "https://api.github.com/repos/$REPO/releases/latest" | grep tag_name | head -1 | cut -d'"' -f4)
URL="https://github.com/$REPO/releases/download/$LATEST/ccteam-$LATEST-$SUFFIX.$EXT"
SUMS_URL="https://github.com/$REPO/releases/download/$LATEST/SHA256SUMS"
TMP=$(mktemp -d)
curl -sSL "$URL" -o "$TMP/pkg.$EXT"
curl -sSL "$SUMS_URL" -o "$TMP/SHA256SUMS"
# checksum verify
(cd "$TMP" && grep "$SUFFIX.$EXT" SHA256SUMS | sha256sum -c -) || { echo "checksum mismatch"; exit 1; }
tar -xzf "$TMP/pkg.$EXT" -C "$TMP"
INSTALL_DIR="${CCTEAM_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$INSTALL_DIR"
mv "$TMP/ccteam" "$INSTALL_DIR/ccteam"
chmod +x "$INSTALL_DIR/ccteam"
echo "Installed: $INSTALL_DIR/ccteam"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "NOTE: add $INSTALL_DIR to your PATH:"
     echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
esac
ccteam --version
```

**安全细节:**
- 强 sha256 校验,失败直接 abort(不 silent skip)。
- 不需要 sudo(默认装 `~/.local/bin`)。
- `CCTEAM_INSTALL_DIR` env override 给系统级安装的用户。
- windows 不支持(用户走 GH Release 页手动下载 + 解压 zip)── 文档说明。

### Sub-3:README + quickstart 改造

`README.md` quickstart Step 1 改为:

```markdown
## Install
```sh
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```
Or download from [Releases](https://github.com/firstintent/ccteam/releases).
Building from source (Rust 1.85+ required):
```sh
cargo install --git https://github.com/firstintent/ccteam ccteam
```
```

`docs/quickstart.md` 中文版同步改;`docs/troubleshooting.md` 加 macOS Gatekeeper 段。

**文件:**
- 新:`.github/workflows/release.yml`、`install.sh`、`docs/versions/v0-6-6/host-probe.md`(Wave 2)
- 改:`README.md` quickstart 段、`docs/quickstart.md`、`docs/troubleshooting.md`
- 测试:`.github/workflows/release.yml` 内 `--dry-run`(可选)+ install.sh 在 nas-box005 真验

**验收:**
- tag `v0.6.6-rc1` 推 → 4 matrix build 全绿 + GH Release 自动创建 + 含 4 个 tarball/zip + SHA256SUMS
- nas-box005 fresh wipe 跑 `curl ... | sh` → `ccteam --version` 输出 `0.6.6`
- checksum 故意改坏 → install.sh abort + exit 1
- macOS-arm64 host 真验(可借开发者自机)→ `xattr -d com.apple.quarantine` 后 binary 可跑
- 测试 +~12(GH Actions workflow yml syntax + install.sh dry-run unit)

**风险:**
- windows MSVC build 可能因 `tokio::signal::unix` cfg 漏洞失败 ── mitigate:本 finding 接到 ⇒ 第一 build attempt 不通过即加 `#[cfg(unix)]` gate 修复(预算 0.3 d)
- macOS-arm64 GH runner 有时排队长 ── 不 block,异步等
- release.yml 触发条件 `tags: [v*]` 会让历史 tag(v0.6.0 - v0.6.5)若 force-push 触发重 release ── mitigate:`if: github.repository == 'firstintent/ccteam'` 守 + 文档说明只 `v0.6.6+` 才有 prebuilt(老 tag 不补 release)

---

## F167 — `/ccteam-creator` 默认 sensible defaults(轻量,非完整 template library)

**痛点:** 用户痛点 4 + 11。
当前 `/ccteam-creator` 落出的默认 workflow.yaml + role md 是「one-size-fits-all」── 用户不论做 monorepo 后端 reviewer / 单 repo bug-hunt squad / TG 聊天助理,拿到的初始模板基本一样,需手改才能用。

**现状缺口:**
- `crates/ccteam-core/src/templates/workflow_templates/` 有 5 个 preset(bg-overnight / chat-pocket / chat-squad / inproc-solo / inproc-team),但里面变量都是空白默认。
- `skills/ccteam-creator/SKILL.md` Phase 4(LLM 生成 yaml)对 project 上下文(repo 结构 / 语言栈 / 是否 monorepo)无探测,LLM 完全靠 user prompt 字面意思猜。
- 用户首次 ship 的 yaml `scope:` 段经常空(V0.6.2 F140 引入,本应是 per-role 重要约束)。

**设计:**

### 边界(防滚雪球)

**F167 = 启发式 + 默认值微调。** **不**做:
- LLM-assisted 完整 role auto-gen(V0.7 epic)
- 新增 preset / template 文件(沿用现有 5 个)
- 跨语言栈适配(只针对当前 5 个 preset 的填充值)

### Sub-1:project-type 探测 helper

新 `crates/ccteam-core/src/templates/project_probe.rs`(~150 LOC):

```rust
pub struct ProjectProbe {
    pub kind: ProjectKind,           // Monorepo / SingleRepo / DocsOnly / ScriptsOnly / Empty
    pub languages: Vec<Language>,    // Rust / TypeScript / Python / Go ...(top 3)
    pub has_tests: bool,
    pub probable_scope: Vec<PathBuf>, // 启发式:src/ + crates/<top-level>/src/ + lib/ ...
}

pub fn probe(repo_root: &Path) -> Result<ProjectProbe>;
```

探测信号(纯文件存在性,不 parse 任何代码):
- `Cargo.toml` workspace.members + `package.json` workspaces + `pnpm-workspace.yaml` + `go.work` → Monorepo
- 单 `Cargo.toml` / 单 `package.json` / `pyproject.toml` → SingleRepo
- 只有 `*.md` + 无 source dir → DocsOnly
- 只有 `*.sh` / `*.py` script 文件 → ScriptsOnly

### Sub-2:workflow_templates 默认值 enhancer

`crates/ccteam-core/src/templates/workflow_templates/mod.rs` `render_workflow_template` 加 ctx argument:

```rust
pub fn render_workflow_template(
    preset: &str,
    ctx: &TemplateCtx,
    probe: Option<&ProjectProbe>,  // new
) -> Result<String>;
```

每个 preset 的 sensible default(示例):

| preset | probe = Monorepo | probe = SingleRepo | probe = DocsOnly |
|---|---|---|---|
| `bg-overnight` | `scope: [<top-3 workspace member crates>]` | `scope: [src/, tests/]` | `scope: [docs/]` |
| `inproc-team` | per-role scope 按 monorepo 子树自动分发 | per-role 同 scope | scope = docs/ |
| `chat-pocket` | bot mention scope = repo root | 同 | 同 |

### Sub-3:`ccteam-creator` SKILL.md Phase 4 集成

Phase 4(yaml 生成)前加 Phase 3.6 "Project probe":

```markdown
## 3.6  Project probe
Run `ccteam probe-project --json` to detect:
- kind (monorepo / single / docs-only)
- languages (top-3)
- probable scope paths

Feed into Phase 4 template ctx so the generated yaml's
`scope:` field is pre-populated with sensible defaults.
```

新 CLI 子命令 `ccteam probe-project --json`(`crates/ccteam-cli/src/commands.rs`,~50 LOC wrapper)。

**文件:**
- 新:`crates/ccteam-core/src/templates/project_probe.rs`、`crates/ccteam-cli/tests/probe_project_test.rs`
- 改:`crates/ccteam-core/src/templates/workflow_templates/mod.rs`、`crates/ccteam-cli/src/commands.rs`(新 sub-cmd)、`crates/ccteam-cli/src/main.rs`(clap 暴露)、`skills/ccteam-creator/SKILL.md`(Phase 3.6 + Phase 4 ctx)

**验收:**
- 在 ccteam repo 本身跑 `ccteam probe-project --json` → `{"kind": "Monorepo", "languages": ["Rust"], "probable_scope": ["crates/ccteam-core/src", "crates/ccteam-cli/src", ...]}`
- fresh tmp repo `git init` + `cargo new --bin foo` → probe = SingleRepo + Rust + scope = `["src", "tests"]`
- `/ccteam-creator "做个 monorepo 后端 reviewer"` 在 monorepo 内跑 → 生成的 workflow.yaml `scope:` 非空
- 测试 +~6(probe 4 种 project kind + 2 template render 集成)

**风险:**
- 探测启发式漏判(如 hybrid monorepo)── mitigate:user prompt 仍可 override probe 结果,probe 只提供「合理初值」
- ccteam 自身 monorepo 探测出 10+ crate,scope 默认值过宽 ── mitigate:probable_scope 截 top-3(按 LOC 排序 / fallback alphabetical)

---

## F168 — active TODO sweep(9 site,逐条决断)

**痛点:** 用户痛点 14。
codebase 内 TODO/FIXME/HACK/Wave-marker 散布,V0.6.5 post-ship-stub-inventory.md Cat 4 列了 11 production site + Cat 7 列了 4 stale doc-comment site + Cat 6 列了 3 adapter delegation site。本 finding **不只**清 inventory 列的,实际 grep `crates/*/src/` 全 production 路径,逐条决断。

**现状缺口:**

实际 grep 结果(2026-05-24 HEAD = V0.6.5 ship)的 9 site(strict `(// TODO|// FIXME|// HACK|TODO\()` pattern,过滤 template 字符串):

| # | Location | 来源 | 当前归属 |
|---|---|---|---|
| 1 | `crates/ccteam-imd/src/daemon.rs:84` | `// TODO(wave-3 codex-exec-impl): swap to CodexAppServerAdapter once it lands` | F173 修(本版,见下) |
| 2 | `crates/ccteam-imd/src/daemon.rs:409` | `/// 3. (slack / discord — TODO in V0.7: providers exist but the host probe's first round only exercises telegram)` | V0.7 显式 defer-with-justification(Epic C 国内 IM 同 wave) |
| 3 | `crates/ccteam-imd/src/daemon.rs:458` | `// (single-bot host probe), V0.7 will cache.` | V0.7 显式 defer-with-justification(perf-rework,单 bot 时无影响) |
| 4 | `crates/ccteam-imd/src/daemon.rs:559-561` | `// V0.7 will land per-bot custom handles. For F132 the typical … collision is theoretical until V0.7.` | V0.7 显式 defer-with-justification(workflow.yaml `chat_handle:` schema 扩展) |
| 5 | `crates/ccteam-imd/src/nl_admin.rs:271` | `// V0.7 wires the full ccteam_cost rollup here.` | F169 修(本版) |
| 6 | `crates/ccteam-core/src/orchestrator.rs:684` | `// TODO(F124 full scope, post-F98): introduce a dedicated HumanApprovalAdapter wrapper` | V0.7 显式 defer-with-justification(F124 full scope 在 V0.6.1 已 explicit defer) |
| 7 | `crates/ccteam-web/src/routes/dashboard.rs:10` | `//! see the TODO marker there for V0.3.3 cleanup.` | F170 修(本版 doc-scrub,marker 早已不存在) |
| 8 | `crates/ccteam-imd/src/three_layer_sec.rs:111` | `// Slack inbound HTTP receiver is V0.7 scope` | V0.7 显式 defer-with-justification(Slack HMAC 与 Epic C 同 wave) |
| 9 | `crates/ccteam-imd/src/transport/providers/slack.rs:5` | `// Switch to Socket Mode is a V0.7 decision per docs/versions/v0-6-0/wave-2-decisions.md §5` | V0.7 显式 defer-with-justification(同上) |

**实数验证**:doc-first session 实测 `grep -rnE "(// TODO|// FIXME|// HACK|TODO\()" crates/*/src/` 命中数 = **8**(`crates/ccteam-core/src/handoff.rs` 内 4 处 `- TODO` 是 HANDOFF_TEMPLATE 内文,不算;`crates/ccteam-core/src/team.rs:1431` 内 `"    pattern: TODO\\n"` 是 test fixture 字符串,不算)。**已加** `daemon.rs:458` 与 `daemon.rs:559-561` 是同 inventory Cat 4 列的 V0.7 marker,被 grep pattern 漏(不是 `// TODO` 起头,而是 `// (single-bot host probe), V0.7 will cache.` 类 inline marker)── 本 finding 决断时**一并扫**(扩展 grep 到 `V0\.[7-9]\+?` 在 src 内),最终决断列表 9 项。

### 决断分布

- **本版 fix(2 项)**:#1 → F173 直接覆盖;#5 → F169 直接覆盖;#7 → F170 doc-scrub
- **EOL 删(0 项)**:本版无「彻底删除决断」
- **V0.7 显式 defer-with-justification(6 项)**:#2 / #3 / #4 / #6 / #8 / #9

**决断模板**(每条 V0.7 defer 必含):
```rust
// V0.7 deferred (justification):
//   <为什么本版不做的 1-2 sentence 业务 / 架构理由>
// Tracked in: docs/versions/v0-6-6/prd.md §F168
// Original marker: <原 TODO 字面意思>
```

**设计:**

### Sub-1:执行决断

每个 V0.7 defer site 改注释:

例如 `daemon.rs:84` 不被 F173 覆盖的 fallback(if F173 实际只动 Codex chat adapter,bg 路径仍 stub):
```rust
// V0.7 deferred (justification):
//   Codex bg-mode adapter swap is bundled with F156/F173 critic wiring;
//   bg-mode users still get ClaudeTuiAdapter fallback which works for
//   non-Codex personas. V0.7 unifies via daemon-routed Codex adoption.
// Tracked in: docs/versions/v0-6-6/prd.md §F168 + §F173
// Original marker: TODO(wave-3 codex-exec-impl)
```

### Sub-2:回归 grep 测试

新 `crates/ccteam-cli/tests/no_silent_todo_test.rs`(~30 LOC):
```rust
#[test]
fn no_silent_todo_in_production_src() {
    let out = std::process::Command::new("grep")
        .args(["-rnE", "(// TODO|// FIXME|// HACK)(?!.*V0\\.[7-9])"])
        .args(["crates/ccteam-core/src", "crates/ccteam-cli/src", ...])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 允许 V0.7+ deferred markers(必含 "V0.X" 词)
    let lines: Vec<_> = stdout.lines()
        .filter(|l| !l.contains("V0.7") && !l.contains("V0.8"))
        .collect();
    assert!(lines.is_empty(), "Silent TODOs found:\n{}", lines.join("\n"));
}
```

测试 invariant:**每个剩余 TODO/FIXME/HACK marker 必须在同行或邻行含 `V0.X` deferred-with-justification 标记**,否则失败。

**文件:**
- 改:`crates/ccteam-imd/src/daemon.rs`(#2/#3/#4)、`crates/ccteam-imd/src/three_layer_sec.rs`(#8)、`crates/ccteam-imd/src/transport/providers/slack.rs`(#9)、`crates/ccteam-core/src/orchestrator.rs`(#6)
- 新:`crates/ccteam-cli/tests/no_silent_todo_test.rs`
- 测试 +~10(no_silent_todo + 决断 site grep verify)

**验收:**
- `grep -rnE "(// TODO|// FIXME|// HACK|TODO\()" crates/*/src/` 输出 ≤ 2 个 site,其余全转 `V0.7 deferred` 格式
- `cargo test no_silent_todo` 通过
- F168 决断列表(9 项 + 三档分类)写入 `docs/versions/v0-6-6/wave-1-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段固定)

**风险:**
- grep regex 误报(template string 内含 `TODO` 字面)── mitigate:测试用 ripgrep 排除 `\.rs` 内已知 template fixture 行;现 `team.rs:1431` 与 `handoff.rs:49-61` 已知 false positive,whitelist
- subagent 发现实际不是 9 而是更多 site → 实数为准,在 wave-handoff 列实际数字 + 全部决断;**禁止悄悄不决断**

---

## F169 — `nl_admin::cost_today` 接真 `ccteam_cost` ledger

**痛点:** 用户痛点 9 + 14。
V0.6.5 ship gate item #9 承诺 `ccteam doctor` 出 "MCP tool surface: 26 active, 0 stubs",但 IM admin `@ccteam cost today` 仍走 V0.6.1 占位 ── 返「bot count」而非真 USD。F169 接真 ledger,end-to-end 闭环。

**现状缺口:**
- `crates/ccteam-imd/src/nl_admin.rs:265-296` `cost_today()` 返:
  ```
  ccteam cost today
    bots: <count>
    detailed per-vendor breakdown: `/ccteam-control show-cost`.
  ```
- 注释 line 271 explicit 说 "V0.7 wires the full `ccteam_cost` rollup here"。
- `<ccteam_root>/cost-budget.json` ledger 已在 V0.6.5 F152 引入(advise_today_usd + per-vendor rows);`ccteam-core::advise::load_budget_ledger` API 已 export。
- 用户 IM 输入 `@ccteam cost today` 得不到真数字 → 痛点 9(团队不依赖人在场)用户面 visibility gap。

**设计:**

### Sub-1:`cost_today` 函数重写

```rust
async fn cost_today(&self, slug_filter: Option<&str>) -> AdminReply {
    use ccteam_core::advise::load_budget_ledger;
    let ccteam_root = ccteam_paths_root();  // 已存在 helper
    let ledger = load_budget_ledger(&ccteam_root).unwrap_or_default();
    let bots = list_bots().unwrap_or_default();
    let filtered: Vec<&BotRegistration> = match slug_filter {
        Some(s) => bots.iter().filter(|b| b.workflow_slug == s).collect(),
        None => bots.iter().collect(),
    };
    let header = match slug_filter {
        Some(s) => format!("ccteam cost today — slug `{s}`"),
        None => "ccteam cost today".to_string(),
    };
    let claude_24h = ledger.sum_24h(Vendor::Claude);
    let codex_24h  = ledger.sum_24h(Vendor::Codex);
    let total = claude_24h + codex_24h;
    let cap = ledger.cap_usd;  // 默认 0.50 USD/24h(F152)
    let msg = format!(
        "{header}\n\
        rolling 24h cost: ${total:.4} / ${cap:.2} cap\n\
        - claude: ${claude_24h:.4}\n\
        - codex:  ${codex_24h:.4}\n\
        active bots: {} (filter: {})\n\
        full breakdown: `/ccteam-control show-cost`",
        filtered.len(),
        slug_filter.unwrap_or("none"),
    );
    AdminReply { message: msg, side_effect: AdminSideEffect::None }
}
```

### Sub-2:`ledger.sum_24h(vendor)` helper

`crates/ccteam-core/src/advise.rs` 加方法:
```rust
impl BudgetLedger {
    pub fn sum_24h(&self, vendor: Vendor) -> f64 {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
        self.rows.iter()
            .filter(|r| r.ts >= cutoff && r.vendor == vendor)
            .map(|r| r.cost_usd)
            .sum()
    }
}
```

(若已存在 `sum_24h` API 则复用;若返 total only,加 per-vendor 版本)

### Sub-3:budget cap 集成

显示 `total / cap`,当 `total > 0.9 * cap` 时 message 加 warning prefix `⚠️ approaching daily budget cap`(对齐 F152 budget enforcement UX)。

**文件:**
- 改:`crates/ccteam-imd/src/nl_admin.rs`(`cost_today` 函数,~50 LOC)
- 改:`crates/ccteam-core/src/advise.rs`(`sum_24h` per-vendor helper,可能已有)
- 新:`crates/ccteam-imd/tests/nl_admin_cost_today_test.rs`(4 test:empty ledger / 单 vendor / dual vendor / cap warning)

**验收:**
- 测试 +4 全通
- nas-box005 跑一次 `mcp__ccteam__advise_vote` → `@ccteam cost today` IM 回真 USD(不再是 bot count)
- 与 `ccteam-control show-cost` 输出数字一致(同源 ledger)
- 测试 `daemon.rs:271` TODO marker 已删(F168 #5 决断)

**风险:**
- `BudgetLedger` schema 与 `cost_today` 期望的 row.ts/row.vendor 字段不完全 align ── mitigate:本 finding 第一步 read ledger 实际 struct,必要时加薄 adapter(预算 ~10 LOC)
- 多 ccteam_root(用户跨 home 跑两份 daemon)── mitigate:nl_admin 在 daemon 内部,daemon 起时已绑定单一 ccteam_root,无歧义

---

## F170 — 陈旧 doc-comment scrub(Cat 7,实测 4 site)

**痛点:** 用户痛点 14。
post-ship-stub-inventory.md Cat 7 列 4 个 stale doc-comment ── 引用早已 ship/landed 的 work,新 contributor 读 src 时被误导。

**现状缺口:**

实测 4 site(2026-05-24 doc-first session grep 确认):

| # | Location | 字面 | 现状 | 修法 |
|---|---|---|---|---|
| 1 | `crates/ccteam-web/src/routes/dashboard.rs:10` | `//! see the TODO marker there for V0.3.3 cleanup.` | `assets.rs` 早已 V0.3.3 cleanup 完毕,无 TODO marker | 删 line 10 整段 reference |
| 2 | `crates/ccteam-core/src/team.rs:1503` | `// F47 ship: schema accepts harness: codex even though spawn is still NotImplemented. F49 wires the runtime path.` | F49 长期 shipped(V0.4.x) | 改为 `// Schema test: harness: codex parsing (F47 schema + F49 runtime, both shipped)` |
| 3 | `crates/ccteam-cost/src/pricing.rs:51` | `// ccteam-core will re-export ccteam_cost::Vendor once Wave 2 lands.` | Wave 2 of V0.6.0 long-shipped;verify re-export | 验 `ccteam-core::lib.rs` 已 re-export,改 doc 为「现状 fact」陈述 |
| 4 | `crates/ccteam-core/src/templates/project_mcp_json.rs:18` | `// Wave 1 lands this helper alone; Wave 2 wires it into the ccteam-creator skill execute phase` | F148(V0.6.5)已 wire,verify | 验 + 改 doc 为「F148 wires it into the ccteam-creator skill Phase 5」(指向当前 SoT) |

**实数 verify**:doc-first session grep 确认 = 4 site(与 post-ship-stub-inventory Cat 7 一致)。

### inventory Cat 7 还提到 `crates/ccteam-imd/src/transport/mod.rs:3` 「V0.6.0 Wave 2 Option-C implementation」── 标 "n/a (keep — historical doc)",本 finding **不动**(尊重 inventory 决断)。

**设计:**

5 site 各自 patch(独立 1-line 改),无新增测试 ── 纯文档 chore,行为零变化。

**文件:**
- 改:4 个文件各 1 处 doc 注释

**验收:**
- `grep -rn "V0.3.3 cleanup\|F49 wires\|once Wave 2 lands\|Wave 2 wires it into the" crates/*/src/` 0 命中
- baseline 不退(无代码改)
- clippy 0

**风险:**
- 验 #3 `Vendor` re-export 时发现还没 re-export(就是 Cat 7 「verify」原意)── mitigate:**若**未 re-export,本 finding 范围扩 ~5 LOC `pub use ccteam_cost::Vendor`,然后再改 doc;若 re-export 存在,纯 doc fix

---

## F171 — `ccteam doctor --verify-mcp` flag,stub-counter parity check

**痛点:** 用户痛点 14。
V0.6.5 ship gate item #9 承诺 `ccteam doctor` 输出 "MCP tool surface: 26 active, 0 stubs",但实际 doctor 没有专门 stub-counter sub-mode ── inventory 显示 V0.6.5 ship 时是 manual 验。F171 加 `--verify-mcp` flag 把这条 invariant 自动化 + 加 0-STUB assertion,catch 未来回归。

**现状缺口:**
- `ccteam doctor` 现支持多 sub-mode(`--validate-team` / `--check-codex-auto-critic` / `--check-pricing` ...)。
- 无 `--verify-mcp` flag,无自动 stub counter 输出。
- 用户 / CI 验「MCP 表面是否仍 0 stub」需手跑 `cargo run -- doctor` + grep 输出 ── 不机械化。

**设计:**

### Sub-1:`--verify-mcp` flag

`crates/ccteam-cli/src/commands.rs` `DoctorOptions` 加 `pub verify_mcp: bool`;`crates/ccteam-cli/src/main.rs` clap 暴露 `--verify-mcp`。

### Sub-2:`run_verify_mcp` 函数

`crates/ccteam-cli/src/commands.rs`(~80 LOC):
```rust
pub fn run_verify_mcp() -> Result<VerifyMcpReport> {
    // 1. List all registered MCP tools via mcp_tool_groups::all_tools()
    // 2. For each tool, classify:
    //    - Active: dispatch path returns real result (not Err("not implemented"))
    //    - Stub: dispatch path is unwired
    // 3. Count + per-group breakdown
    // 4. Cross-check against expected count(26 tools per V0.6.1 F128)
    Ok(VerifyMcpReport {
        total: 26,
        active: 26,
        stubs: 0,
        per_group: ...,
        unexpected_stubs: vec![],  // 非空 → exit 1
    })
}

pub struct VerifyMcpReport {
    pub total: usize,
    pub active: usize,
    pub stubs: usize,
    pub per_group: BTreeMap<String, GroupStats>,
    pub unexpected_stubs: Vec<String>,
}

impl VerifyMcpReport {
    pub fn render(&self) -> String { /* human-readable */ }
    pub fn ok(&self) -> bool { self.unexpected_stubs.is_empty() }
}
```

### Sub-3:输出格式

```
MCP tool surface verification
===
total tools:    26 (expected 26)
active:         26
stubs:          0

per-group breakdown:
  workflow_:    7 active / 0 stub
  chat_:        9 active / 0 stub
  advise_:      2 active / 0 stub
  admin_:       7 active / 0 stub  (incl. change_persona, add_tool)
  screenshot:   1 active / 0 stub

verdict: PASS — all 26 tools live, no production STUBs.
```

非 0 stub → `verdict: FAIL` + exit code 1。

### Sub-4:dispatch path 自检

「stub」判定:每个 tool name 对应 dispatch fn,fn body 含 `Err(NotImplemented { .. })` 或 `return stub_response()` → 标 stub。

实现:`mcp_tool_groups` 暴露每 tool 的 dispatch fn pointer / async closure;`run_verify_mcp` 用一个 sentinel input 调用 + 检查 result variant。**Notes**:不能真调外部副作用(如 register_bot 写文件),需 design dispatch fn 支持 `dry_run = true` hint,或用 mock fixture(F171 内 ~30 LOC test helper)。

替代实现(更简单):**static lookup table** ── `mcp_tool_groups::STUB_TOOLS: &[&str] = &[]`(V0.6.5 ship 后是空 list)。`run_verify_mcp` 读这个 const + 与 active list 对账。任何 PR 加 stub tool 必同步加 list,grep test 守。

**文件:**
- 改:`crates/ccteam-cli/src/main.rs`(clap)、`crates/ccteam-cli/src/commands.rs`(`run_verify_mcp` + `VerifyMcpReport`)、`crates/ccteam-cli/src/mcp_tool_groups.rs`(`STUB_TOOLS` const)
- 新:`crates/ccteam-cli/tests/doctor_verify_mcp_test.rs`(6 test:expected count / 0-stub assertion / per-group breakdown / FAIL exit code / render format / static-table parity)

**验收:**
- `cargo run --release -- doctor --verify-mcp` 输出含 `MCP tool surface: 26 active, 0 stubs`
- `mcp_tool_groups::STUB_TOOLS` const = `&[]`
- 故意临时把一个 dispatch 改成 stub → `doctor --verify-mcp` 输出 FAIL + exit 1
- 测试 +6 全通
- V0.6.5 ship gate item #9 提到的措辞通过 grep 验证

**风险:**
- 「real dispatch 检查」过度复杂 ── mitigate:走 Sub-4 替代实现(static lookup table + grep 守),更轻量
- 26 tool 总数本版可能因 F167 加 `ccteam probe-project` 而变 27 ── mitigate:`probe-project` 是 CLI sub-cmd,**不**是 MCP tool;MCP 总数仍 26(确认无新 MCP 工具),否则 expected count 同 PR 同步改

---

## F172 — tmux mode-3 上下文恢复(`chat_snapshot` event)

**痛点:** 用户痛点 9 + 14。
V0.6.5 F163(SIGTERM graceful)+ F164(tmux reattach)解决了「daemon 物理重启不丢 tmux session」,但 **chat bot 已累积的对话上下文若发生 tmux session 死亡(进程崩 / 物理重启 / OOM kill)只能走 F118 `session_recovery::build_recovery_prompt` 回放 last-N turns**。F172 加 proactive snapshotting,让 recovery 不只靠 turns.jsonl 重放,还能续上「最后一次 snapshot 起的增量」── 把长跑 bot 的 context 损失从「last-N turns」收缩到「last snapshot 起的新增 turn 数」。

**现状缺口:**
- F118 已落 `chat_session_reset_with_recovery` event + `session_recovery::build_recovery_prompt` ── 仅在 session-id invalidated(compaction failure / corruption)时触发,**不是周期性** + recovery prompt 是 last-N turn 回放,不包含 bot 内部 working memory / 已下决断 / 引用的中间 artifact。
- progress.jsonl 现有 chat-mode 子事件家族(F108 / F118):`chat_session_started` / `chat_turn_user_prompt` / `chat_turn_completed` / `chat_session_reset` / `chat_session_reset_with_recovery` / `chat_compact_done` / `chat_hop_escalate` ── 都是「事件发生时记一笔」,无周期性 snapshot 概念。
- daemon 重启 → `claude_tui::start_thread` 若 session alive → F164 reattach;若 session dead → F118 recovery 回放 last-N turns。**Loss**:重启窗口内的 in-pane working memory(已用的工具结果 / 已展开的 plan / mid-conversation TODO list)全失。

**设计:**

### 红线核对(详 README §3)

- **不**新建第 9 类业务 event:`chat_snapshot` 加入 chat-mode 子事件家族(与 F108 / F118 同 category),progress.jsonl SoT 红线守。
- **不**主动 kill long session:snapshot 是非破坏旁路 dump,不触 tmux pane,不发 send-keys,不影响 bot 当前活动。
- **不**解析 tmux 终端输出:snapshot 源 = `turns.jsonl`(F108 mirror)+ 既有 progress.jsonl event,不调 `tmux capture-pane`。
- **不**注入 system prompt:recovery 时注入的是 user prompt 形式(对齐 F118 `build_recovery_prompt` 既有路径),`.claude/agents/<role>.md` 仍是 agent 行为唯一 SoT。

### Sub-1:`chat_snapshot` event schema

`crates/ccteam-core/src/progress.rs` 加:

```rust
/// `chat_snapshot` — V0.6.6 F172: periodic context snapshot dumped by
/// the BotSupervisor (mode-3 chat). Used by daemon-restart recovery
/// path to rebuild context beyond the last-N-turns fallback in F118.
///
/// Cadence: every N turns OR every M minutes, whichever first
/// (defaults N=10, M=30). Payload references turns.jsonl byte offset
/// instead of inlining transcript to keep progress.jsonl scannable.
pub const CHAT_SNAPSHOT: &str = "chat_snapshot";

pub fn build_chat_snapshot_event(
    role: &str,
    turn_id_range: (String, String),  // (first_turn_id, last_turn_id)
    turns_jsonl_byte_offset: u64,     // resume point in turns.jsonl
    context_size_tokens: u32,         // approx tokens in window
    triggers: SnapshotTriggers,       // Periodic { every_n_turns } | TimeElapsed | Manual
) -> Value {
    serde_json::json!({
        "event": CHAT_SNAPSHOT,
        "role": role,
        "turn_id_range": [turn_id_range.0, turn_id_range.1],
        "turns_jsonl_byte_offset": turns_jsonl_byte_offset,
        "context_size_tokens": context_size_tokens,
        "triggers": triggers,
        "ts": Utc::now().to_rfc3339(),
    })
}
```

**关键 schema 设计**:
- **turns_jsonl_byte_offset**:snapshot 不内联 transcript(避免 progress.jsonl 膨胀),只记 resume point;recovery 时 `seek(offset)` 即可 stream-read 后续 turn。
- **context_size_tokens**:approx 计数(由 UnifiedTokenUsage 累加),帮 recovery 判断「context 已超模型窗口?要先 compact?」。
- **triggers**:why-this-snapshot 元数据(便于 dashboard 显示)。

### Sub-2:dump cadence

`crates/ccteam-imd/src/supervisor.rs` `BotSupervisor` 加:

```rust
pub struct SnapshotPolicy {
    pub every_n_turns: u32,        // default 10
    pub max_elapsed: Duration,     // default 30 min
    pub max_context_tokens: u32,   // emergency snapshot when > 80% window
}

impl BotSupervisor {
    async fn maybe_snapshot(&mut self, role: &str) {
        let since_last = self.turns_since_last_snapshot(role);
        let elapsed = self.elapsed_since_last_snapshot(role);
        let tokens = self.approx_context_tokens(role);
        let trigger = match (since_last, elapsed, tokens) {
            (n, _, _) if n >= self.policy.every_n_turns => Some(...),
            (_, e, _) if e >= self.policy.max_elapsed => Some(...),
            (_, _, t) if t >= self.policy.max_context_tokens => Some(...),
            _ => None,
        };
        if let Some(triggers) = trigger {
            self.emit_snapshot_event(role, triggers).await;
        }
    }
}
```

`maybe_snapshot` 在 `chat_turn_completed` handler 末尾调(已是 BotSupervisor 现有 hook 点)。

### Sub-3:daemon restart recovery flow

`crates/ccteam-imd/src/daemon.rs` 起 daemon 时 per-bot 走:

```
1. tail progress.jsonl(F164 既有)→ 找 latest chat_snapshot event for this role
2. if found:
   a. claude_tui::start_thread(role)
      - F164 path: if tmux session alive → reattach
      - else: spawn new + run recovery prompt
   b. read turns.jsonl from snapshot.turns_jsonl_byte_offset onwards
   c. compose recovery prompt:
      "[ccteam recovery] Resuming after daemon restart.
       Last snapshot at turn_id <last_turn_id>, ~<N> turns ago.
       Recent turns since snapshot: <inline last 5 turns from byte offset>.
       Please continue from where we left off."
   d. push as user prompt to tmux pane(对齐 F118 既有路径)
3. if no snapshot:fallback F118 build_recovery_prompt(last-N turns mode)
4. emit chat_session_reset_with_recovery event(F118 既有,扩 payload 加 from_snapshot: true 字段)
```

### Sub-4:idempotent re-attach

`chat_handle` 必须 idempotent:重复调 `start_thread(role)` 时:
- 若 tmux session alive + snapshot already applied this restart cycle → no-op
- 否则:走完整 recovery flow

引入 per-role `recovery_applied_in_restart_cycle: BoolMap`(daemon 内存),重启时清空。

### Sub-5:边界(防滚雪球)

**F172 不做**:
- 主动恢复 in-tmux-pane 用户已输入未发送的 input(不可观测,放弃)
- 跨机器迁移 chat context(V0.7+ chat memory sync)
- recovery 时自动 compact(用户决定)── 仅在 `context_size_tokens` 超 90% 阈值时 warn

**文件:**
- 改:`crates/ccteam-core/src/progress.rs`(`CHAT_SNAPSHOT` const + `build_chat_snapshot_event` + `SnapshotTriggers` enum)、`crates/ccteam-core/src/execution/session_recovery.rs`(`build_recovery_prompt` 加 snapshot-aware overload)、`crates/ccteam-imd/src/supervisor.rs`(`SnapshotPolicy` + `maybe_snapshot`)、`crates/ccteam-imd/src/daemon.rs`(restart recovery flow)、`crates/ccteam-core/src/execution/claude_tui.rs`(idempotent re-attach)
- 新:`crates/ccteam-imd/tests/chat_snapshot_test.rs`(periodic trigger / time elapsed trigger / token cap trigger)、`crates/ccteam-imd/tests/daemon_restart_recovery_test.rs`(snapshot exists / no snapshot fallback / idempotent re-attach / from_snapshot field set)

**验收:**
- 测试 +~30 全通(snapshot 8 + recovery 12 + idempotent 4 + progress event helper 6)
- nas-box005 host-probe:跑 mode-3 chat ≥10 turn → progress.jsonl 有 ≥1 `chat_snapshot` event(periodic trigger);`kill -TERM` daemon → `ccteam start` → 第 11 turn 输入 "continue from where we left off" → bot reply 引用早 turn 内容(测试人确认语义续接);progress.jsonl 含 `chat_session_reset_with_recovery` event with `from_snapshot: true`
- baseline 不退
- 红线核对:`grep -rn "tmux capture-pane" crates/ccteam-core/src/execution/session_recovery.rs crates/ccteam-imd/src/supervisor.rs crates/ccteam-imd/src/daemon.rs` 0 命中

**风险:**
- snapshot 频率过高 → progress.jsonl 膨胀 ── mitigate:默认 every-10-turns OR 30-min,benchmark 实测 < 100 line/day per active bot
- recovery prompt 过长 → 浪费 context 窗口 ── mitigate:Sub-3 只 inline "since snapshot" 部分(typically 1-9 turns 间隔),完整历史在 turns.jsonl 由 LLM 按需 grep
- byte_offset 失效(若 turns.jsonl 被外部 truncate)── mitigate:recovery flow 验 offset 在文件长度内,若否 fallback F118 build_recovery_prompt(last-N) + emit warning
- 与 V0.6.5 F164 `start_thread` reattach 行为耦合 ── mitigate:F172 Sub-4 idempotent guard 独立 BoolMap,不影响 F164 alive-session 探测;严格区分 "session alive but no recovery applied yet"(走 recovery)vs "session alive + recovery already done"(no-op)

---

## F173 — Codex daemon-routed critic 统一 cost rollup(F156 follow-through)

**痛点:** 用户痛点 6 + 14。
V0.6.3 F156 ship 时说 "Daemon-routed variant (route critic through `CodexExecAdapter` for unified cost accounting) **explicitly deferred past V0.6.5**"(`docs/versions/v0-6-3/README.md` line 16 + V0.6.5 wave-2-handoff R8)。当时理由:advise_* MCP 还没 ship,cost rollup 前提不足。**现在前提齐备**(V0.6.5 F152 引入 `<root>/cost-budget.json` ledger + `CodexExecAdapter` 已用于其他路径),F173 补完 ── critic 调用统一计入同账,end-to-end 闭环。

**现状缺口:**
- `crates/ccteam-imd/src/daemon.rs:78-89` `default_adapter_factory` Codex arm 仍返 `ClaudeTuiAdapter`(`// TODO(wave-3 codex-exec-impl): swap to CodexAppServerAdapter`)── chat-mode Codex bot **silently** 跑 Claude adapter,critic spawn 不走 Codex 路径。
- `crates/ccteam-core/src/orchestrator.rs:671-677` `adapter_for_chat` `(Codex, Chat)` arm 同 fallback。
- `skills/ccteam-team/SKILL.md` §3.5 N≥3 critic 是 bash spawn 直跑 `codex exec --json`,**不走** ledger 账户 ── 这次调用花的 USD 不计入 `<root>/cost-budget.json::advise_today_usd`(对比 F152 `advise_vote` 路径走 ledger)。
- 用户面影响:`@ccteam cost today`(F169 修后)显示 advise call 真数,但 critic 调用「漏算」── cost visibility 仍有 leak;长跑 Codex 调用累计未被 cap enforce → 痛点 14 长跑可控性 leak。

**设计:**

### Sub-1:critic spawn 路由 `CodexExecAdapter`

`skills/ccteam-team/SKILL.md` §3.5 N≥3 critic 部分:从「bash 直跑 codex exec」改为「调 MCP tool `mcp__ccteam__advise_vote` with vendors=[claude, codex]」,或新 MCP tool `mcp__ccteam__advise_critic`(若 advise_vote 不适用 N≥3 路径)。

**决策**:本版**复用** `advise_vote` ── critic 本质就是「找另一个 vendor 提第二意见」,与 advise_vote 语义重合;无需新 MCP tool。SKILL.md §3.5 文案改为:

```markdown
For N≥3 review setups, the critic call should go through
`mcp__ccteam__advise_vote` (which routes through CodexExecAdapter +
ledger). Bash `codex exec` direct spawn is deprecated as of V0.6.6;
existing bash path will be removed in V0.7.
```

### Sub-2:`default_adapter_factory` Codex arm 修

`crates/ccteam-imd/src/daemon.rs:78-89`:

```rust
fn default_adapter_factory(vendor: AgentVendor) -> Box<dyn HarnessAdapter> {
    match vendor {
        AgentVendor::Claude => Box::new(ClaudeTuiAdapter::new()),
        AgentVendor::Codex => Box::new(CodexExecAdapter::new()),  // was: ClaudeTuiAdapter fallback
    }
}
```

`crates/ccteam-core/src/orchestrator.rs:671-677` 同步:
```rust
fn adapter_for_chat(exec: ExecutionMode, vendor: AgentVendor) -> Box<dyn HarnessAdapter> {
    match (vendor, exec) {
        (Codex, Chat) => Box::new(CodexExecAdapter::new()),  // was: bg fallback
        ...
    }
}
```

### Sub-3:ledger 统一接入

`CodexExecAdapter` `submit_turn` 路径加 ledger update hook:

```rust
async fn submit_turn(&self, h: &ThreadHandle, prompt: &str) -> Result<TurnId> {
    let ccteam_root = ccteam_paths_root();
    let pre_spent = load_budget_ledger(&ccteam_root).map(|l| l.advise_today_usd).unwrap_or(0.0);
    let cap = load_budget_ledger(&ccteam_root).map(|l| l.cap_usd).unwrap_or(DEFAULT_ADVISE_CAP);
    if pre_spent >= cap {
        return Err(HarnessError::BudgetExceeded { spent: pre_spent, cap });
    }
    // ... existing codex exec spawn ...
    // post-turn: parse turn.completed JSONL → extract token usage → estimate cost
    let cost = estimate_cost(usage, AgentVendor::Codex);
    append_budget_ledger_row(&ccteam_root, AgentVendor::Codex, cost)?;
    Ok(turn_id)
}
```

Helper `append_budget_ledger_row`(`crates/ccteam-core/src/advise.rs` 加):
```rust
pub fn append_budget_ledger_row(
    ccteam_root: &Path,
    vendor: Vendor,
    cost_usd: f64,
) -> Result<()>;
```

(若 F152 已有等价 API,复用 + 必要时小重构)

### Sub-4:`ccteam doctor` cost-orphan 检查

`crates/ccteam-cli/src/commands.rs::run_doctor` 加 cost-orphan invariant:
- 扫描 progress.jsonl 内 24h 内 `agent_done` event(含 Codex vendor)→ 对账 ledger row 数
- 不对账 → warn "cost orphan: N Codex calls in progress.jsonl, M rows in ledger"
- 完全对账 → silent OK

`ccteam doctor --verify-mcp`(F171)同时调 cost-orphan 检查,double protection。

### Sub-5:F156 explicit defer cleanup

`skills/ccteam-team/SKILL.md` 内「daemon-routed Codex critic with unified cost accounting **explicitly deferred past V0.6.5**」文案删除 → 改为「daemon-routed Codex critic shipped V0.6.6 F173 — see `docs/versions/v0-6-6/prd.md` §F173」。

V0.6.5 wave-2-handoff.md R8 不动(历史文档归档,只在新版本归档反映现状)。

**文件:**
- 改:`crates/ccteam-imd/src/daemon.rs`(`default_adapter_factory`)、`crates/ccteam-core/src/orchestrator.rs`(`adapter_for_chat`)、`crates/ccteam-core/src/execution/codex_exec.rs`(ledger hook in `submit_turn`)、`crates/ccteam-core/src/advise.rs`(`append_budget_ledger_row` helper if needed)、`crates/ccteam-cli/src/commands.rs`(`run_doctor` cost-orphan)、`skills/ccteam-team/SKILL.md`(F156 文案 cleanup)
- 新:`crates/ccteam-core/tests/codex_critic_ledger_test.rs`(critic 调用记 ledger 验证,~120 LOC)、`crates/ccteam-cli/tests/doctor_cost_orphan_test.rs`(orphan detect / clean state,~80 LOC)
- 测试 +~15

**验收:**
- 测试 +15 全通
- nas-box005 host-probe:跑 `mcp__ccteam__advise_vote claude+codex same question` → `<root>/cost-budget.json::advise_today_usd` 含 Claude 与 Codex 两 row,数字 = 实际 token 用量 estimate;`@ccteam cost today` 显示同样数字(F169);`ccteam doctor --verify-mcp` 无 cost-orphan warning
- `daemon.rs:84` TODO marker 已 cleanup(F168 #1 决断)
- `orchestrator.rs:684` HumanApprovalAdapter TODO **不动**(本 finding 不动 F124 scope,F168 #6 已 V0.7 defer)
- `skills/ccteam-team/SKILL.md` F156 文案改为「shipped V0.6.6 F173」
- baseline 不退

**风险:**
- `CodexExecAdapter` post-turn 解析 `turn.completed` JSONL token usage 字段名 / shape 与 estimate_cost helper 期望不匹配 ── mitigate:本 finding 第一步实测 codex 实际 JSONL output(`crates/ccteam-core/src/execution/codex_exec.rs` 已有解析逻辑可复用)+ 必要时加 `serde(alias)` 兼容
- `BudgetExceeded` 错误对 critic 调用是 hard fail ── mitigate:N≥3 critic 是 advise call,失败 user-visible OK(用户已知 budget 设定);**不**自动 bypass
- F156 跨版本 cross-ref:本 finding 必须验 V0.6.5 wave-2-handoff R8 在 codebase 实际存在(doc-first session 已 verify line 36 含 R8)── 若不存在,本 finding 立即停 + escalate(per task spec block 模式)

---

## 附录 A:8 finding 间依赖图

```
F166 (release CI + install.sh)      ─┐  完全独立(8 worktree 全并行)
F167 (sensible defaults)             ─┤
F168 (TODO sweep)                    ─┤  与 F169 + F170 + F173 有 site-overlap
F169 (cost_today ledger)             ─┤  → F168 #5 决断同 PR
F170 (doc scrub)                     ─┤  → F168 #7 决断同 PR
F171 (doctor --verify-mcp)           ─┤
F172 (chat_snapshot)                 ─┤
F173 (Codex critic ledger)           ─┘  → F168 #1 决断同 PR + 与 F169 共享 ledger schema(F169 read,F173 write)

site-overlap 处理:每 PR review 时主会话(dispatch agent)负责 cross-check;
worktree 间不直接 cross-merge,主会话依次 merge 顺序解决冲突。
```

## 附录 B:Ship gate 复盘(关 §README §5)

15 项 ship gate 中,**本 PRD 验收**对应每 finding 的「验收」段;**集成 ship gate**(baseline / clippy / GH Actions release CI / host-probe / CLAUDE.md baseline 回填 / dev-coupling-audit 索引补 / tag)由 Wave 2 doc-syncer 负责。详 `dev-plan.md`。
