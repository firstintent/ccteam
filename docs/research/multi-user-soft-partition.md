# 多用户软租户分区 —— 共用一个 daemon、同一个 OS 账号、最小改动

> **类型**:设计稿(讨论,代码未动)。
> **日期**:2026-06-21 · **owner 选定**:多用户挤同一个 ubuntu 账号 + **共用一个 ccteam daemon**,要**最简化**;按 **project** 分区(每用户项目目录本就独立);并追问「IM 配置会不会失效 / 有没有更小改动」。
> **原型**:[`../versions/v0-8-18/prototype/multi-user-soft-partition.html`](../versions/v0-8-18/prototype/multi-user-soft-partition.html)。

---

## 0. 先把诚实话说前面(红线)

同一个 OS 账号(同 uid)+ 共用一个 daemon → **UX 级软隔离(防误串),不是安全隔离。** 同 uid 谁都能读彼此文件 / `/proc/<pid>/environ` / ptrace(单-uid 全信任红线);项目目录虽各自独立,磁盘上**仍互读**。本方案只保证:**ccteam 视图/操作面里用户不误看/误碰别人的项目和 session。** 给互信同租户用刚好;要真隔离 → 一人一实例 / 沙箱。owner 已在此前提下明确选「最简、共用 daemon」。

---

## 1. 两个「白送」+ 一个病根

多用户塌缩成一个信任域,但其实大半已分好:

- **白送 1 —— IM 已 per-chat**:每人私信同一个 bot,Telegram 天然给不同 `chat_id` → 这就是租户身份,gateway 本就按它分。
- **白送 2 —— 存储已 per-sid**:`pane`=`ccteam-chat-<slug>-<sid>`、`turns`=`.ccteam/chat/<sid>/turns.jsonl`、marker/cursor 全按 sid,全局唯一,物理不撞。
- **病根**:不是存储、是 **ACL/视图**。`chat_can_access`(`gateway.rs:1219`)有「同 project 互看」+「web-operator 通看」两条漏;web 单一共享 token(`validate_bearer(presented, expected_hex)`,`auth.rs`)= 谁拿链接谁是同一个超级操作员。

→ **串只是「谁能看见哪个 sid」这层的问题。** 所以改动能很小。

---

## 2. `ccteam config`(IM)不会失效,也不用改

telegram bot 是 **daemon 级**的:一个 bot、多个 chat。多人私信同一个 bot,各自 `chat_id` 区分;`ccteam config` 由 daemon 主人**跑一次**设好 bot,用户各自 pair 进来即可 —— **不需要每人一个 bot、不改 config**。身份这块 IM 是白送的(只有 web 那个共享 token 要处理)。

> 例外:某用户出于隐私想要**自己的 bot** → 那是「一人一实例」路线(各自 `CCTEAM_HOME` + 各自 bot),不在共享-daemon 这条。

---

## 3. 分档上(核心很小,web/project 是渐进增强)

| 档 | 做什么 | 改动量 | 必需? |
|---|---|---|---|
| **档 0** | **ACL 收成 own-only** —— 删 `chat_can_access`(`gateway.rs:1219`)的「同 project 互看」+「web-operator 通看」两条。IM 用户(本就 per-chat)立刻互不可见。 | **几行**,零新字段/token,不碰 config | ✅ **最小可用就这一项** |
| **档 1** | **web 个人视图** —— 不发明新 token 体系,复用 web 已有的「一次绑一个 chat」概念(`state.rs:96`):每人一个预绑自己 chat 的 web 链接,own-only 一套上视图只剩自己的。更省:v0 把 web 当 admin/owner 面,其他人只用 IM。 | 中(复用既有 bind) | 选(web 多用户才需要) |
| **档 2** | **按 project 整理** —— `ProjectState` 加 `owner`(`state.rs:86`),项目列表/`/cd`/web 按 owner 过滤;session 归属从所属 project 继承;`/share <project>` 显式共享。 | 一个字段 + 几处过滤 | 选(更整齐的粗粒度分区,非治串必需) |

**最小可用 = 档 0。** 档 1/2 是渐进增强,不是前置。原型画的是档 1+2 的最终视图(用户只看见自己的项目树),但后端只上档 0 也已不串(IM 侧)。

---

## 4. 不做 / 不碰

- ❌ 不拆进程、不开沙箱、不动 OS。
- ❌ 不新建账号/密码体系 —— 复用 IM pairing(chat_id)当身份。
- ❌ 不碰 session 存储 / pane / turns / sid。
- ❌ 不动 `ccteam config` IM(bot 是 daemon 级共享)。
- ❌ 不声称安全隔离 —— 同 uid 软隔离,UI 横幅诚实标注。

---

## 5. 验收

1. (档 0)A、B 各自 IM pair → 各自 `/use`/`/cd` 只能碰自己的 session;旧「同 project 互看」漏堵上。
2. (档 1)A、B 各拿不同 web 链接 → web 视图各只剩自己的。
3. (档 2)项目列表/`/cd` 按 owner 过滤;`/share <project> @B` 后 B 可见该项目。
4. 横幅/文案明确「软隔离,非安全」。baseline 不退。

---

## 6. 升级路径(若哪天要真隔离)

软分区与「真隔离」同源:今天 **身份(chat_id)→ 过滤视图**(软);将来 **身份 → 路由到该用户自己的实例/沙箱**(硬,control-plane)。档 0 的 own-only ACL + (档 2 的)`owner` 字段既解决今天误串,也是将来真多租户的地基,不浪费。

> 与 [[loop-engineering-is-ccteams-init_chin]] 的 control-plane 同源:身份 → 路由;软是 scope、硬是 sandbox。
