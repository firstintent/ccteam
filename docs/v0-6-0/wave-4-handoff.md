# V0.6.0 Wave 4 — Handoff

> **Status**: Shipping(integration branch `wave4-integration`,2-way merge done,baseline 1283/1,clippy 0 warnings,17 红线全绿)。
> **Wall time**: 2 teammate 并行 worktree ~45 min(doc-syncer 含 11 min Anthropic API transient backoff,nudge 后恢复);主 session 整合 + nas-box005 deploy + probe + handoff doc ~30 min。

## Decided

- **Tier-1 docs sync 完成**:CLAUDE.md(§一 version 0.6.0 + baseline 1282/1 + V0.6.0 finding list + V0.6.x deferred + V0.7 roadmap;§三 红线表"模式 × vendor"双轴 scope F106 + vendor 补充;§四 24 工具 5 group;§五"Minor 版本 4-wave 范式"新段)、`docs/tech-design.md`(§0 红线表 + §2.1 三层架构 update + §3.3.1 mode:/vendor: + 新章节 §6.5a chat-mode design)、`docs/interfaces.md` §12 子前缀 sweep + 7 chat/advise tool 新增 + 全 V0.5 unprefixed `mcp__ccteam__*` 名 sed 替换(across 10 files)、`docs/dev-coupling-audit.md` F106-F118 13 行索引 + F110 ~~取消~~。
- **clippy 18 → 0 warnings**(history first — `-D warnings` 真 clean)。fix:`artifact_watcher.rs:122-123` doc lazy continuation / `harness.rs:103-107` 缩进 / `migration.rs:185-196` 缩进 / `orchestrator.rs:82` `>` quote / `preferences.rs:179` 缩进 / `workflow.rs:24` 缩进 / `progress.rs:430` `LivenessProbe<'a>` type alias(显式 lifetime 避免改 public API)。
- **workspace.package.version bump 0.5.1 → 0.6.0**(Cargo.toml + Cargo.lock auto-regenerate)。
- **SpawnCtx.model_id plumb(Wave 3 D14 fixup)**:`AgentSpec.model: Option<String>` 新 YAML 字段 + `SpawnCtx.model_id: Option<String>` + `try_spawn_with_prompt` 塞 `agent.model.clone()` → SpawnCtx + `translate_thread_event(usage, vendor, model: Option<&str>)` 前向到 `ccteam_cost::estimate_cost`(empty string 仍 fallback,V0.5 compat)。13 call-sites updated。新 test `per_vendor_model_specific_pricing` pin Claude opus≠haiku + Codex o3≠gpt-4o-mini pricing — silent fallback regression 触发 fail。
- **`scripts/host-probe/`**:`deploy-to-nas.sh` + `run-probes.sh` + `README.md` — ssh-able。Env override 支持(`CCTEAM_NAS_HOST` 默认 `nas-box005`;DNS 无解析时用 `CCTEAM_NAS_HOST=192.168.1.19` IP override;`CCTEAM_NAS_WIPE_HOME=0` 默认保留 credentials;`CCTEAM_PROBE_REAL_TG=1` 启 mode 3 real round-trip)。
- **`docs/v0-6-0/host-probe.md`** 含真实 probe 结果(`run 20260519T013822Z`):8/8 场景 rc=0;3 codex happy / 2 TG real-partial / 2 mode-1 manual / 1 mode-2 mock。Real TG bidirectional 独立验证(curl getMe + sendMessage + getUpdates 收 user reply,message_ids 363/364/366/367)。
- **`docs/v0-6-0/demos/`** + README:V0.6.0 不录 demo GIF,defer V0.6.1 用 asciinema → agg 录(handoff 含 recipe)。
- **TG bot `@web3op_bot`(chat_id 339498819)credentials.json** 0600 落本地 + scp 到 nas-box005:0600(`~/.ccteam/im/credentials.json`)。bot 已 `/start`'d,双向 reachability 验证 ok。
- **nas-box005 stale processes 清理**:May 16-18 遗留 `ccteam start` daemon + agent shipper/fixer + bg-spare 多个 helper 进程全 killed(用户两轮提醒 + 主 session 自查 autostart vector — systemd/cron/bashrc 无命中,可能 historical SSH/VSCode-remote spawn 残留;清完 10s re-check 0 respawn)。

## Rejected

- ~~5 个 demo GIF 录真~~ — 时间投入 ≥4h(每 preset 30s 录屏 + edit + agg),defer V0.6.1。
- ~~probe script 起 ccteam-imd daemon~~ — V0.6.1 finding。当前 probe rc=0 但 daemon 没起的 mode 3 场景走不到真 round-trip;mitigation:手动 curl + getUpdates 已 prove TG channel surface OK,daemon→Channel 路径已被 mock_test e2e_mock_test.rs(Wave 3 ship)单元测试 cover。
- ~~probe script 真起 overnight-builder workflow~~ — 当前只 `--help` smoke。V0.6.1 加 fake workflow + mock artifact + assert agent_done 进 progress.jsonl。
- ~~CHANGELOG.md~~ — 仓内无传统,继续按 wave handoff + tag commit message 风格。

## Risks

- **probe script daemon-start gap**(V0.6.1 finding #1)— 现 probe script 不主动起 ccteam-imd 也不 health-wait。V0.6.0 ship 后,user 若手动 `ccteam daemon start` + 跑 `/ccteam-im-setup`,真 TG round-trip 即激活。code 路径已完整(Wave 2/3 落 + e2e_mock_test pass)。
- **mode 1/2 仅 manual / mock probe**(V0.6.1 finding #2)— mode 1 需 user 在 Claude session 跑 `/ccteam <NL>`,自动化 probe 困难;mode 2 overnight-builder probe 仅 `--help`,真 long-running workflow probe 需 V0.6.1 fake workflow fixture。当前 mitigation:cargo test 全链路 unit + integration cover。
- **OpenAI pricing 数据源**(Wave 1 cost-crater retained risk)— `pricing/openai.toml` verify @ 2026-05-19 openai.com;3-6 月后须 re-verify。V0.6.x 可加 `ccteam doctor --check-pricing-version` 警告(schema_version 已 ready)。
- **CodexAppServerAdapter notifications → progress.jsonl bridge 未通**(Wave 3 D9 retained risk)— mode 3 codex bot 未 V0.6 启用(per Wave 1 决策),所以暂无 user-facing impact。V0.7 启 codex bot 时同步加 bridge。

## Files

新文件:
- `scripts/host-probe/{deploy-to-nas.sh, run-probes.sh, README.md}`(3 files,host probe driver)
- `docs/v0-6-0/{host-probe, demos/README, wave-4-handoff}.md` + `docs/v0-6-0/demos/.gitkeep`

修改(doc-syncer):
- `CLAUDE.md`(§一/§三/§四/§五 全面 sync)
- `docs/{tech-design, interfaces, dev-coupling-audit, claude-code-best-practices, claude-code-tool-surface, ccteam-as-domain-agnostic-orchestrator}.md`
- `docs/v0-1/user-quickstart.md` + `docs/v0-4-6/user-manual.md` + `docs/research/v047-pattern-rust-vs-skill-split.md` + `docs/v0-6-0/wave-1-handoff.md`(子前缀 sed sweep)
- `skills/ccteam-control/SKILL.md`(子前缀 sweep)
- `crates/ccteam-core/src/{artifact_watcher, harness, migration, orchestrator, preferences, workflow, progress}.rs`(clippy fix)
- `Cargo.toml`(version 0.6.0)+ `Cargo.lock`(regenerate)

修改(host-probe):
- `crates/ccteam-core/src/{harness.rs(SpawnCtx.model_id 字段), orchestrator.rs(plumb), workflow.rs(AgentSpec.model 字段), queries.rs(translate_thread_event signature)}`
- `crates/ccteam-imd/src/supervisor.rs`(model_id forward)
- `crates/ccteam-cli/src/commands.rs`(plumb call-sites)
- `crates/ccteam-web/tests/flex_e2e_test.rs`(adapt to new sig)
- `.gitignore`(`.probe-results/`)
- 9 test files updated + 1 new(`per_vendor_model_specific_pricing` in `crates/ccteam-core/tests/per_vendor_budget_test.rs`)

## Remaining(V0.6.1 / V0.7)

V0.6.1 candidate findings(from this wave + retained):
- F119 probe script daemon-start + health-wait + mode-3 real round-trip(本 wave finding #1)
- F120 overnight-builder probe full workflow(本 wave finding #2)
- F121 ccteam doctor --check-pricing-version 警告(Wave 1 retained)
- F122 CodexAppServerAdapter notifications → progress.jsonl bridge(Wave 3 D9 retained)
- F123 5 demo GIF 录制(V0.6.0 deferred)
- F98 plan-approval ↔ outbox 联动(V0.5 deferred,V0.6 PRD §八)
- F124 6 号编排模式 HITL / Approval Gating(V0.6 PRD §八)

V0.7 主线:Epic C 国内 IM 启用(`openhuman` 已 vendored Option C,V0.7 加 lark/dingtalk/qq/wechat in-crate providers + 对应 onboarding skill)+ chat memory 跨设备同步(目前 turns.jsonl 落 local `<project>/.ccteam/chat/<bot>/`,V0.7 加 rsync/git/cloud sync option)。
