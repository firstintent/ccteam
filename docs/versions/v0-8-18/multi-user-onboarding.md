# v0.8.18 · 新用户加入多用户 ccteam —— 操作步骤

> **给谁看**:owner 体验 + 验收用。共用一个 daemon、同一个 OS 账号、按 project 分区(软隔离·非安全,见 [`../../research/multi-user-soft-partition.md`](../../research/multi-user-soft-partition.md))。
> **可点走查原型**:[`prototype/multi-user-walkthrough.html`](prototype/multi-user-walkthrough.html) —— 切「新用户首次 / @bob / 切到 @alice」体验「各看各的私有世界」。

---

## 场景

管理员(= owner / alice)已在一台云机上 `ccteam start`。要让同事 **bob** 也用上,但两人**各看各的**(项目/会话/成本互不可见)。

---

## A. 管理员侧(一次性,给新用户开通)

1. bob 把自己的 Telegram 告诉管理员(或直接 DM bot)。
2. 管理员**批准**把 bob 加进名单 —— 二选一:
   - CLI:`ccteam user add bob`(铸 bob 的身份 + 专属 web 链接,打印出来)。
   - IM:既有 `/telegram:access` 配对批准流程。
   - **注册天生管理员门控**(绝不因频道消息自动批准 —— 红线)。
3. 把 bob 的**专属 web 链接**发给他:`https://<host>/?token=ccteam:<bob-hex>`(或让 bob 在 IM 里 `/web` 自取)。

> 管理员的 `ccteam config`(bot 设置)**不用动** —— 一个 bot 服务所有人,Telegram 按 chat_id 区分。

---

## B. 新用户 bob 侧

1. **登录**(二选一,都读同一张身份表):
   - **web**:打开专属链接 → 自动设 cookie → 进去就是 **@bob**(`TokenEntryPage` 现成 URL-shim)。每人一个链接,不共用。
   - **IM**:DM bot(已批准)→ 直接用,身份 = 你的 chat_id。
2. **首次**:看到空状态「你还没有项目」。
3. **建项目**:`cd ~/bob/api-server && ccteam init`(或 web/IM 点「新建」)→ 项目归 **@bob**。
4. **用起来**:你的控制台只列**你的**项目 / session / 成本;alice 的你看不见(顶部「软隔离·非安全」横幅诚实标注)。
5. **协作(可选)**:谁想共享某项目 → `/share <project> @对方`。

---

## C. 体验「隔离」(走查原型里点这个)

- @bob 登录 → 只见 `~/bob/api-server` + 他的 s40 + 他今日 $0.41。
- 切到 @alice → 整个 app 变成她的世界:`excore`/`lap` + s31/s28/s33 + 今日 $4.21。**bob 的一概不在**。
- 同一台机、同一个 daemon —— 各看各的。

---

## 诚实边界(必须让 owner 知道)

- **软隔离·非安全**:同一个 OS 账号(同 uid),bob 和 alice 在磁盘上**仍能互读**彼此项目目录 / secret(单 uid 红线)。本方案只挡 **ccteam UI/操作面**里的误串,不挡同机窥探。
- 要**真隔离** = 一人一 OS 账号 / 一人一实例(沙箱),那是另一条路(control-plane 方向)。
- **CLI 是管理员面**:有 shell 就是全权(同账号下没法按人分),所以 `ccteam user` 这类是管理员用的;按人分只在 web + IM 生效。

---

## 验收(owner 这关)

1. 走查原型点「新用户首次 → @bob → 切 @alice」,三个世界清楚切换、互不串。
2. 操作步骤 A/B 读下来,新用户从「被批准」到「只看见自己的」全程闭环、无歧义。
3. 诚实边界一眼可见(横幅 + 本文 C/诚实段)。
4. owner 认可后,才进 PRD 实现(档 0 必做;登录注册/CLI user 视 owner 对 web 多用户的需求定档 1 是否进本版)。
