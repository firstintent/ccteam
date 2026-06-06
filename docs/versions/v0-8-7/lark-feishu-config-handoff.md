# v0.8.7 Add-on — Lark/Feishu 接入 `ccteam config` Handoff

> 用户中途追加需求(非 PRD wave):`ccteam config` 只能配 Telegram,补 Lark/Feishu。
> **CONFIG-ONLY** —— Lark/Feishu **transport 早已在 dev 在树**(`transport/providers/lark.rs` 959 行 WS 长连接 + 725 行测试,daemon `CHANNEL_BUILDERS` 已有 lark 行,`LarkCreds` 落 `~/.ccteam/im/credentials.json`,`lark` 在默认 features)。`origin/feat/lark-feishu` 分支已陈旧(dev 超前 34 commit 且已含其码)→ **不合并、仅参考**。本次只补 `ccteam config` 配置面 + 解禁 3 处校验器。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1886/0**(W1 后基线 1877 +9)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `doctor --verify-mcp` 17/17、0 drift(只加 enum 值、未加工具)。

## Decided
- **菜单加独立第 3 项**「set Lark/Feishu app credentials」(不嵌在 Telegram 项下)→ 每平台 stdin 读取线性、提示文案各自专属;show-prefs 3→4,提示 `1-3`→`1-4`。`run_config_lark_menu()` 依次问:app_id(`cli_...`)、app_secret、region(`[F]eishu` CN 默认 / `[L]ark` intl → `use_feishu`,回车=Feishu/CN)、allowed open_ids(逗号/空格/Tab 分隔,可空)。
- **`run_config_set_lark_creds(app_id, app_secret, allowed_user_ids, use_feishu)`**:先 **live 校验** creds(取 `tenant_access_token`),再 `creds.lark = Some(LarkCreds{...})` merge 进 credentials.json(`load→save`,完整保留既有 telegram/slack/discord —— 同 Telegram 路径)。
- **onboarding 校验复用** `lark.rs:609-648` 的 `auth/v3/tenant_access_token/internal` POST 形状:新增 `ccteam_im::onboarding::lark_setup` / `lark_setup_with_base(... api_base)`;因 channel 的 fetch 是私有,onboarding.rs 自带一份最小拷贝 + `TenantTokenResponse{code,msg,tenant_access_token}` 子集(**不碰 transport** 红线);非零 `code`/缺 token → `OnboardingError::{ApiNotOk,BadResponse}`(坏 creds 响亮失败、不落死 token)。
- **FAIL-CLOSED 语义双重提示**:Lark allowlist 空 = **答复无人**(与 Telegram 空=开放相反),IDs 是 open_id(`ou_...`),`*`=所有人 —— 交互提示 + 落盘后摘要都明示,防操作者自锁。
- **3 校验器解禁** + main.rs `session register --platform` help 列 lark。
- **测试用 `_with_base` seam**:`run_config_set_lark_creds_with_base(api_base, creds_path_override)` + onboarding `*_with_base`,用一次性 `std::net::TcpListener` HTTP mock + tempdir creds 路径 → 无网络、无 env 变更,故 commands.rs 测试可留 `#[cfg(test)] mod`;onboarding 测试落 `ccteam-im/tests/`(集成,raw-TCP mock,因 ccteam-im 无 axum/wiremock dev-dep)。

## Rejected
- **不**把第二平台嵌进 Telegram 菜单项(线性 stdin + 专属文案更清晰)。
- **不**碰 `lark.rs` transport(红线;channel fetch 私有 → onboarding 自带最小拷贝,~40 行,可接受的轻度重复)。
- **不**合并 `origin/feat/lark-feishu`(dev 已是其超集,合并会回退/冲突)。
- **不**加无头 `config lark ...` 子命令(`Config` 的 clap arg 是 `args: Vec<String>` 定位 `num_args=0..=2`,region/allow flag 塞不进 → 推迟,见 Remaining)。

## Risks
- **交互菜单 `run_config_lark_menu` 无自动测试**(需 TTY,与既有 Telegram 菜单项同缺口);仅 `run_config_set_lark_creds(_with_base)` + onboarding helper 有单测 → **ship 前建议手动 TTY smoke**。
- 生产路径 setup 时做 **live `tenant_access_token` 取**(open.feishu.cn / open.larksuite.com)→ 断网/代理会令 setup 失败(与 Telegram `getMe` 一致、有意为之 = 诚实校验)。
- **无无头路径**:脚本/CI provision Lark creds 仍只能手写 credentials.json(Telegram 也无)。
- open_id 仅做非空切分、**无格式校验** → 操作者可能误贴 user_id/union_id。

## Files
- ccteam-im:`src/onboarding.rs`(`lark_setup`/`lark_setup_with_base`)、`tests/lark_onboarding_test.rs`(新,3 测试)。
- ccteam-cli:`src/commands.rs`(菜单第 3 项 + `run_config_set_lark_creds(_with_base)` + `run_admin_register_bot` 接 lark + 4 测试)、`src/mcp_chat_tools.rs`(`validate_im_platform` + register_bot schema enum 接 lark + 2 测试)、`src/main.rs`(help 文案列 lark)。

## Remaining
- **可选无头形**:`ccteam config lark <app_id> <app_secret> [--feishu|--lark] [--allow <ids>]` —— 需把 `Config` 升成带 flag 的正经 clap 子命令(或加 `LarkConfig` variant)+ main.rs `run_config`(~1142)接线;`run_config_set_lark_creds` 已是现成 handler。
- **docs**:`docs/usage.md` + README 的 `ccteam config` 段未提 Lark → 并入 **W6 docs-sync**(本任务 config-only;ship-gate 文档同步是独立 wave)。
