# v0.8.19 handoff — web 前端可交付质量改版

> branch `v0819-impl`(off `origin/dev`),7 提交,逐波 gate-green。纯 SPA(`crates/ccteam-web/web`)+ docs,**0 Rust 改动**。等 review → merge dev(无 tag,owner 部署)。

## Decided

- **会话窗口先行、可独立部署**(W1):owner 点名的两个 bug(回车误发 / 渲染不及格)是纯前端、最高优先,先落、先验。
- **组件原语层是病根**(W2a):全站没有 `components/ui/`、class 串各页重复 = demo 感的结构性来源。立 `cn()`(twMerge∘clsx)+ CVA 原语,后续波次都用它。
- **主题用 var 覆盖、不引 next-themes**(W2b):确认 Tailwind v4 `@theme` 工具类编译成 `var(--color-*)`(`dist` 实测 `.bg-surface-900{background-color:var(--color-surface-900)}`),所以 `:root.light` 重声明同名 token 即可一翻全站。主题状态进 `useWebSettings`(复用既有持久层),`<html>.light` 由 App effect 同步 + `index.html` 预绘脚本防首屏闪。默认 **dark**(ccteam 身份),不跟随系统。
- **明暗切换单图标**(owner 追加):AvatarMenu 一个按钮,dark 显 Sun(点→亮)、light 显 Moon(点→暗)。
- **去 emoji 用 lucide**(owner 追加):`lucide-react`(已是依赖)统一导航图标;头像盘 emoji → 纯色圆点(`useWebSettings.avatar` hex,旧 emoji 值 `avatarColor()` 兜底)。
- **浮层手搓而非 @base-ui**(W3a):装了 `@base-ui/react@1.6.0` 实测其 `Dialog` 在 bare-node `renderToString`(测试环境)渲染**空字符串**(只走 client Portal),会挂 SSR 冒烟测试 → **卸掉**,手搓 SSR-safe `Dialog`(client `createPortal` / server inline,焦点陷阱-lite + Esc + click-away + scroll-lock)+ `Combobox`(可搜 + 翻转定位 + **隐藏原生 `<select>` 兜 SSR/no-JS**)。
- **HostsView 留卡片**(W4a):每主机至多 ~2 个 agent + 带注册 CTA,卡片比表格更合适;只统一到 `Card` 原语 + 骨架 + 空态。
- **测试连接按钮不加**(W3b):`configApi` 无 getMe-only 接口,Telegram 校验内嵌在 `saveTelegramToken`;按红线**不造后端**。

## Rejected

- **@base-ui/react 作 overlay 底座** —— SSR 空渲染挂测试(见上),不值得为它改测试范式。
- **next-themes** —— 多余依赖;`useWebSettings`+`@theme inline`-free 的 var 覆盖更轻。
- **sonner / 角色命名 token 大重构(surface-*→bg/fg)** —— 前者 toast 已有;后者 churn 大,浅色用现名覆盖(接受 `surface-900` 在浅色语义反转,当「基准表面」抽象)。
- **结构化 tool/thinking 上 web(W4b 后端)** —— 见 Risks/Remaining,超「小后端」,deferred。

## Risks

- **浅色对比度**:浅色盘是首版,正文/状态色按 4.5:1 设计但**未在真机逐面核对**;部署后 owner 切浅色过一遍各页(尤其 status 色 pill、市场卡、终端)。
- **`field-sizing-content` 兼容**:Composer/Textarea 自增高用它(Chromium 123+,Firefox 暂无)—— 不支持的浏览器回退成固定行 + 滚动,不致命。
- **W4b 流式指示**是 `busy`(submit→`done` 帧)двух态,非真三态(thinking/streaming/done);per-turn 元信息(model·延迟·token·成本)**没做** —— 这些数据未按 turn 外露到 web SSE。
- **部署门**:SPA 经 `build.rs`(`web-bundle` feature)→ rust-embed 进二进制;**daemon 不重部署 + SPA 不重 build,用户看不到改版**。

## Files(33 changed,+2511/-789)

**新增** `web/src/`:`lib/utils.ts`(cn)、`lib/markdown.ts`、`components/Markdown.tsx`、`components/Composer.tsx`(+ `.test.tsx`)、`components/ui/{button,card,badge,input,textarea,label,dialog,combobox,table,skeleton,empty-state,index,ui.test}.tsx`。
**改** `web/src/`:`pages/SessionView.tsx`(Composer+Markdown+busy+回到最新+流式)、`pages/SettingsPage.tsx`(重写,+ `.test.tsx`)、`pages/MarketplaceView.tsx`(Dialog+Combobox+骨架+空态)、`pages/StatusView.tsx`(tanstack 表+骨架+空态)、`pages/HostsView.tsx`(Card+骨架+空态)、`pages/ChatConsole.tsx`(nav lucide + NewSessionModal Combobox)、`pages/chatTranscript.ts`(无改 — assistant 渲染在 SessionView)、`components/AvatarMenu.tsx`(+`.test.tsx`,主题切换+色点头像)、`hooks/useWebSettings.ts`(theme 字段)、`App.tsx`(主题 class 同步)、`index.css`(`:root.light`+D1-D4+代码块复制样式)、`index.html`(预绘脚本)、`package.json`(+tailwind-merge/CVA/@tanstack/react-table)。
**docs**:`docs/versions/v0-8-19/{README.md,handoff.md,prototype/*}`、`CLAUDE.md`(§一 baseline + v0.8.19 条)、`Cargo.toml`(version 0.8.19)。

## Remaining(deferred / 后续)

1. ~~结构化 tool/thinking 上 web~~ **已落地**(`96401c2`,owner 追加「按更优雅/通用方式」):新 `GatewayEventKind::Activity{status_key, SessionActivity}` 中立事件 + `progress::activity_for` 共享 summarizer(IM/web 同源不漂移)+ IM no-op(零回归)+ web `kind:activity` → SPA 活动行。blast radius 恰 2 个 exhaustive match(web payload + IM 消费)。**仅剩 stream-json 的 tool-result 体**(`tool_result` 块缺 name + 需 item-id 重键 → 不冒险改 adapter;tool 名/入参/思考已全外露)。
2. **per-turn 元信息**:model·延迟·token·成本 按 turn 外露到 web SSE(后端)→ chat hover footer。
3. **浅色盘真机核对** + 可选「跟随系统」第三态。
4. **Playwright/e2e**:本环境 inotify-busy 未跑(env-gated,非本版回归);CI/真机复测。
