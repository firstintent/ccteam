# ccteam V0.6.7 — install.sh ship-blocker hot patch (musl static binaries, dual-arch)

> **范围:install-fix-only patch。** F174 一项,改 `release.yml` linux build target 到 musl static + 加 linux-arm64,改 `install.sh` 支持 `linux-aarch64`/`linux-arm64`。**无 ccteam 业务代码改动**,无新 MCP tool,无 schema 变化。

---

## 1. 为什么紧急

V0.6.6 ship 的 `install.sh` 在两台 Ubuntu 22.04 系机器(包括一台 NAS)上均报:

```
ccteam: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by ccteam)
```

= 用户拿到 install.sh 一行命令但 binary 跑不起来 = product 入口完全堵死。

**根因**:`release.yml` 在 `ubuntu-latest`(目前 = Ubuntu 24.04,glibc 2.39)上 build `x86_64-unknown-linux-gnu` target,产物动态依赖 glibc 2.39;用户机普遍是 glibc 2.35(Ubuntu 22.04 / Debian 12 / 多数现役 NAS)甚至更老,无法运行。

V0.6.6 F166 的 prebuilt binary 设计本身没问题(release matrix / SHA256SUMS / install.sh 主流程都 OK),只是 build target 没考虑 host glibc backward-compat。这是 install-path bug,patch 1 行 release.yml + 几行 install.sh 即可彻底解决。

---

## 2. F174 — Linux musl static binaries + linux-arm64 prebuilt

| Action | 文件 | 改动 |
|---|---|---|
| Build target 改 musl(x86_64) | `.github/workflows/release.yml` | `x86_64-unknown-linux-gnu` → `x86_64-unknown-linux-musl`,runner 留 `ubuntu-latest` + `apt install musl-tools` |
| 新增 linux-arm64 build | `.github/workflows/release.yml` | matrix 加 `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`(GitHub-hosted ARM runner,public repo 免费) |
| npm lockfile fast-path 收紧 | `.github/workflows/release.yml` | `runner.os == 'Linux'` → `runner.os == 'Linux' && runner.arch == 'X64'`;arm64 走 macOS 同款 drop-lockfile + `npm install --include=optional`(lockfile 是 linux-x64 生成,不含 arm64 rolldown binding) |
| Static-link sanity step | `.github/workflows/release.yml` | build 完跑 `file <binary> \| grep -E 'statically linked\|static-pie linked'`,不命中 fail build(防 dep 升级 silent 退化成 dynamic) |
| install.sh 支持 linux-arm64 | `install.sh` | `linux-aarch64\|linux-arm64` 从 error → `SUFFIX=linux-arm64`;清理 fallback hint 文案 |
| Release notes 文案 | `.github/workflows/release.yml` | "Linux x86_64 + aarch64, statically linked against musl libc so they run on any glibc version" |

**用户面命名保持透明** `linux-x64` / `linux-arm64`,**不**加 `-musl` 后缀。用户感知 = "Linux 二进制现在直接能装",musl 是实现细节。

**为什么 musl 而非 ubuntu-22.04 runner**:musl static binary 完全无 glibc 依赖,覆盖所有 Linux 发行版(包括 Alpine、老 CentOS、各种 NAS)。`ubuntu-22.04` runner 只把兼容下限拉到 glibc 2.35,仍排除老系统;且 GitHub 已宣布 `ubuntu-22.04` image deprecation 在 2026 年某时,1-2 年内还要再迁。一步到位。

**依赖审计**(本地预证 musl 路线无坑):TLS = rustls(`reqwest` default-features=false + `rustls-tls`,`tokio-tungstenite` + `rustls-tls-webpki-roots`),无 OpenSSL;`nix` 0.29 用 `signal`/`fs`/`process` features 全 POSIX 标准;`notify = "8"`(inotify-rs 0.10)musl 完整支持;libc 调用走 `libc` crate。本地 `cargo build --release --target x86_64-unknown-linux-musl --bin ccteam` 3 分钟 build 成功,`file` 报 `static-pie linked`,`ldd` 报 `statically linked`,在本机 glibc 2.35 上 `--version` OK。

---

## 3. 红线核对(CLAUDE.md §三)

无触及。F174 仅作用于 CI build / 发布 / install 路径,**0 行 ccteam 业务代码改动**:

- `crates/*/src/**` 0 改
- `progress.jsonl` schema 0 改
- MCP tool surface 0 改(仍 27 工具,STUB 仍 0)
- workflow.yaml / `.mcp.json` / `.claude/agents/*.md` 0 改
- 红线 R0-R12 全部不触及

---

## 4. Baseline gate

- workspace `0.6.6` → `0.6.7`(`Cargo.toml::workspace.package.version`)
- test:V0.6.6 baseline `1639/1`,本版**无新增测试**(install path 不进 cargo test,CI build 本身是 gate),目标 ≥ `1639/1`
- clippy:`-D warnings` clean(本版 0 业务代码改,自动持平)
- fmt:`cargo fmt --all -- --check` clean
- CI build 必须四个 target 全绿(linux-x64 / linux-arm64 / macos-arm64 / macos-x64),any one fail = release 不可发

---

## 5. Ship gate(CLAUDE.md §五.7)

| Item | Done |
|---|---|
| `CLAUDE.md §一` baseline / workspace version 更新 | ✅(本 PR) |
| `Cargo.toml::workspace.package.version` 0.6.6 → 0.6.7 | ✅ |
| `Cargo.lock` 同步 | ✅(`cargo build` 自动更新) |
| `docs/versions/v0-6-7/README.md` 落地 | ✅(本文件) |
| `docs/dev-coupling-audit.md` 加 F174 索引 | 见 PR |
| 用户面 docs:`README.md` quickstart 段 / `docs/quickstart.md` install 段 | **无需改动** ── install.sh 命令本身不变,只是底层 binary 变 static;用户视角零差 |
| CI release.yml 四个 target 全绿 | tag-push 后验 |
| 两台用户机重跑 install.sh 验证 | tag-ship 后验 |

---

## 6. 不在范围(V0.7 候选,本版不做)

- macOS universal binary(目前 macos-arm64 / macos-x64 两份独立 tarball,够用)
- Windows native build(WSL2 走 linux-x64 musl binary,foundation 仍 unix-only,见 release.yml 头部注释)
- `cargo install --git` fallback 路径优化(install.sh `error: download failed` 时仍可手动 fallback,主路径 musl 通了之后用户基本不会触发)
- Linux 32-bit / armv7 / riscv64 prebuilt(用户基数极小,需要时再扩 matrix)

---

## 7. Acceptance(用户验)

V0.6.7 tag push → release.yml 跑完 → 两台原本报 `GLIBC_2.39 not found` 的机器重跑:

```sh
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
# 应输出: ccteam 0.6.7
```

成功 = patch 闭环。
