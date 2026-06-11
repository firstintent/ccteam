# v0.8.11 Wave 6 handoff — E5 文档 + ship gate

> 范围:ship gate 文档同步 + version bump。**绝不打 tag**(tag 由 owner 决定)。

## Decided

1. **workspace version** `0.8.10` → `0.8.11`(`Cargo.toml`)。
2. **CLAUDE.md**:§〇 header(v0.8.10→v0.8.11)+ 协议轴红线;`harness × provider facet` → `harness × provider × protocol facet`(两条 Claude 通道 + 工厂三路由 + `host` 预留);§一表(version / baseline 1994·279·145 / 当前在做)。≤200 行(实 156)。
3. **tech-design.md**:§2.3 两轴 → Claude 两条 spawn 路径(stream-json 默认 | terminal)+ 工厂三路由 + 四缝段落;§10 协议→代码指针表加两行(stream-json adapter 四缝 + protocol 轴/创建面/工厂)。
4. **usage.md**:`/new … terminal` 命令行 + 「协议通道」段(stream-json 默认 / terminal 高级 / stream-json 无 pane→`/screen` 人话拒 / 寻址两通道一致)。
5. **README.md**(英文,无版本进展):harness 段描述两条 Claude 通道(stream-json 默认 + terminal)。
6. **版本归档**:`docs/versions/v0-8-11/README.md`(冻结里程碑:一句话 / E1–E5 / §七 准备度 / follow-up / 迁移 / baseline)+ 六份 wave handoff。

## Ship gate(全绿)

| 门 | 值 |
|---|---|
| `cargo test --workspace --exclude ccteam-web` | **1994 / 0** |
| `ccteam-web` | **279 / 0** |
| vitest(SPA) | **145 / 0** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0** |
| `cargo fmt --all` | 干净 |

baseline 全程不退:1942(起跑,修 stale `/model` 测试)→ 1975(W1)→ 1985(W2)→ 1989(W3)→ 1993(W4)→ 1994(W5/W6)。

## Remaining(owner 决定 / follow-up)

- **打 tag**:绝不由 dev session 打;owner review 后决定。
- **HITL 生产 resolver 接线**(W3 Decided 1)、**真机/真 vendor smoke**(W4)、**MCP/web screenshot 对 stream-json 的 protocol 专属文案**(W3)—— 见 `README.md`(归档)follow-up 清单。
