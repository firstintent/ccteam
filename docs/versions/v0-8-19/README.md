# v0.8.19+ — web 前端可交付质量(整个前端)

> **状态:已落地 dev(branch `v0819-impl`,等 review/部署;无 tag)**。owner:web 端「简陋的像个 demo」+「整个前端考虑,不止 chat」+「还有 Setting、浅色暗色主题、整体质感」+「去 ai 味 emoji 用开源 ico 库」+「明暗切换图标用一个」。
> 参考 **LAP** = `references/litellm-agent-platform/src/ui`(Next16 但**同栈** React19 + Tailwind4 + base-ui/shadcn + CVA;patterns 可直接抄)。owner 允许小范围后端改动配合。
> 原型(自包含、离线;即设计 spec):① 会话窗口 [`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ② 前端地基/原语库(单图标主题切换)[`prototype/v0819-ui-kit.html`](prototype/v0819-ui-kit.html) ③ Settings 重设计(单图标主题切换)[`prototype/v0819-settings.html`](prototype/v0819-settings.html)。

## 实现结果(已落地,见 [`handoff.md`](handoff.md))

7 波,逐波过 `tsc + eslint(0/0) + vitest + vite build`;**纯 SPA + 文档,0 Rust 改动** → cargo `2039/0` / clippy / fmt 不动。vitest **188→198**(+IME 守卫/原语/主题/combobox 单测)。

- **W1**(`40c0a90`)会话窗口:`Composer`(IME 守卫 + Shift/Cmd+Enter + Send⇄Stop + per-sid 草稿 + 自增高)+ 助手 markdown(`lib/markdown` marked+DOMPurify → `.cockpit-markdown`)+ 代码块复制。
- **W2a**(`72f8fd6`)原语库:`tailwind-merge`+CVA + `cn()` + `components/ui`(Button/Card/Badge/Input/Textarea/Label)。
- **W2b**(`8ba373a`)浅/暗主题(`:root.light` 覆盖 var token + 单图标 Sun/Moon + 预绘不闪)+ D1-D4 + 去 emoji(lucide 导航 + 色点头像)。
- **W3a**(`997058e`)浮层:手搓 SSR-safe `Dialog`+`Combobox`(@base-ui 因 SSR 空渲染卸掉),4 原生 `<select>` 全换 + 市场抽屉 → Dialog。
- **W3b**(`23cff89`)Settings 重设计:card-per-provider + 用户表格 + 状态读出,灭 `*_CLASS`,红线保(masked-token / 两步内联确认 / admin-gate);测试连接没加(无 getMe-only 接口,不造后端)。
- **W4a**(`404141f`)`@tanstack/react-table` Status 舰队可排序 + `Skeleton`/`EmptyState` 原语;Hosts 仍卡片 + 骨架 + 空态。
- **W4b**(`3de519b`)chat 打磨:回到最新按钮 + 流式三点指示。

- **W4b 后端结构化活动**(`96401c2`,owner 追加「按更优雅/通用方式去改」):新 `GatewayEventKind::Activity{status_key, SessionActivity}` 一等中立事件。设计:① 结构化数据本就到 pump(`ThreadEvent`/`ThreadItemDetails`),不需往上游 replumb;② `progress.rs::activity_for` **共享 summarizer** → IM 状态行 + web 摘要同源、构造上不漂移;③ IM 消费 **no-op** → Answer/Progress 投递 byte-identical、零回归;④ web SSE 出 `kind:activity` + 结构化体 → SPA 紧凑活动行(lucide 图标)。blast radius = 恰 2 个 exhaustive match。baseline cargo `2039→2045` · ccteam-web `257→258` · vitest `198→207`,clippy/fmt clean。

**仍 Deferred(小)**:stream-json 的 tool-result **体** —— `tool_result` 块缺 tool name + 需 item-id 重键,不冒险改 adapter;tool 名 / 入参 / 思考文本已全外露,只差结果体。

> 部署:dev 落地无 tag → 需 **SPA 重 build + daemon 重部署 + 用户 `/mcp` 重连**。

---

## 一、痛点(= 验收基准)

owner 点名 chat 三处(回车误发 / 渲染不及格 / 交互不及格),并逐步扩到:**整个前端 · Settings · 浅色/暗色主题 · 整体质感**。

**结构性根因:ccteam SPA 没有组件原语层,且只有暗色一套。** 没有 `components/ui/`,每页从裸 Tailwind 重拼,class 串以模块常量重复(`SettingsPage.tsx:46-57` 的 `FIELD_CLASS`/`PRIMARY_BTN_CLASS`/`GHOST_BTN_CLASS`,又在 `Marketplace`/`Status`/`Hosts`/`AvatarMenu` 内联重抄);`index.css @theme` 写死暗色,无主题轴。→ 焦点/禁用/按压/徽章拼写各页不一致 + 无明亮模式 = 「demo 感」与「质感缺失」的根。chat 是最响一处,不是唯一。

映射 `docs/requirements.md`:核心痛点「从手机/web 驱动云端 AI 团队」── 驱动入口不可用 = 价值漏在最后一公里。

---

## 二、现状诊断(代码 SoT)

SPA = `crates/ccteam-web/web/`(React19 + Vite + Tailwind4,token 在 `index.css @theme`,**dark-only**)。

### 2.1 chat 会话窗口 ── `pages/SessionView.tsx`
- **输入框 `:403-408`**:Enter handler **零 IME 守卫** → 中文选词回车直接发半句。草稿切 session 即丢;不自增高;无 Send/Stop。
- **渲染 `:382-395`**:`{row.content}` 裸字符串。依赖已装(`marked`/`dompurify`/`remark-gfm`),`.cockpit-markdown` 样式已存在(`index.css:128-230`),没接进会话。
- `chatTranscript.ts:eventToRow` 丢 tool/thinking/流式中间态。

### 2.2 整个前端
- **无 `components/ui/`**(主导问题);浮层全手搓(Marketplace 抽屉、AvatarMenu 弹层 `:148-153` 的 `<button fixed inset-0>` click-away);原生 `<select>` 破暗色;加载/空态用文字;列表手写 `.map`;无 `tabular-nums`。

### 2.3 Settings ── `pages/SettingsPage.tsx`(866 行)
- 布局 `p-4 max-w-md mx-auto`(**448px 窄单列**,挤);3 段手搓(Telegram/Lark/UserManagement);用 `FIELD_CLASS`/`PRIMARY_BTN_CLASS`/`GHOST_BTN_CLASS`(就是 §一 的重复源)。
- 4 态 + admin-gate(tenant 见纯文字指引);masked token「(set, …wxyz)」+ 内联两步覆盖确认(**已做对,别退**);chat_id 异步轮询(`:21-25`)。
- 用户管理是手写列表,非表格(不能排序/搜)。AvatarMenu(`components/AvatarMenu.tsx`)有显示名/头像/语言/登出,**无主题切换**。

### 2.4 主题 ── `index.css @theme`
- `@theme { --color-surface-900:#1c1c1f; --color-text-primary:#e4e4e7; … }` **写死暗色**,无 light 轴、无切换、无系统跟随。加浅色 = **token 层要改成主题感知**(见 §三 主题)。

### 2.5 ✅ 已经做得好/更好,**别动**(免 churn)
`lib/fetchInterceptor.ts`(全局 fetch + `X-Aoe-Token` 轮换 + 401 分类,强于 LAP)· `Toasts.tsx`+`toastBus`+`reportError`(**别引 sonner**)· Settings 内联两步确认(**强于 LAP 原生 `confirm()`**,LAP 自己注释 confirm 破暗色;只 DRY 成 `<ConfirmInline>`)· `useKeyboardShortcuts`(LAP 没有)· 4 态页 / `prefers-reduced-motion` / focus-visible / status 语义色(比 LAP 全)。

---

## 三、从 LAP 吸收什么

### A. 会话窗口(chat)── 原型 `v0819-chat-console.html`
`.sessions-md` markdown CSS + react-markdown/remark-gfm(`globals.css:213`)· 输入框 Send⇄Stop + 草稿保留(`composer.tsx:30,90`)· **IME 守卫(LAP 也缺→我们改进,`!isComposing&&keyCode!==229`)** · 贴底滚动锚定(`chat/page.tsx:881`)· 工具活动折叠(`message-block.tsx:331`)· 用户气泡/助手全宽不对称 · 三态流式 · hover 元信息。

### B. 整个前端(分层)── 原型 `v0819-ui-kit.html`
**地基 CSS(一行级,可整段 lift):** D1 全局 `tabular-nums`+`font-feature-settings`(`globals.css:166`,成本数字不抖)· D2 表头字阶 · D3 暗色点阵背景(`globals.css:123`)· D4 主题化 `::selection`(`globals.css:130`)。
**组件原语库(核心,先落它):** 立 `components/ui/`+`cn()`(`twMerge(clsx())`)。**决策:overlay 类(Dialog/Menu/Select/ScrollArea)采 `@base-ui/react`**(白送 portal/定位/焦点陷阱/键盘);Button/Card/Badge/Input 用**零运行时依赖 纯元素+CVA**。C2 Button(`active:translate-y-px`+`focus-visible:ring-3`)· C3 Card(`ring-1 ring-foreground/10` 发丝环)· C4 Badge · C5 Input/Textarea/Label(`field-sizing-content` 自增高)· C6-9 Dialog/DropdownMenu/Select/ScrollArea(base-ui)· C10 Table。
**壳/CRUD/跨切面:** S1 乐观删除+回滚 · CR1 骨架屏 · CR2 图标+CTA 富空态 · CR3 `@tanstack/react-table`(Status/Hosts 列→可排序/筛选表)· CR4 抽 `<ConfirmInline>`(DRY 自己的)· X1 可搜·portal·翻转感知 combobox(`model-select.tsx`,替原生 `<select>`)。

### C. 主题 · 浅色/暗色(提为正式项,不再 P2)── 原型带 ☀/🌙
抄 LAP 整套配方(`layout.tsx:49`+`theme-toggle.tsx`+`globals.css` 双 token 集):
1. **token 层改主题感知**:`index.css` 的 `@theme` → **`@theme inline`**(让 `--color-*` 变运行时可覆盖,LAP 正是这么做),保留暗色为默认;加 `:root.light { --color-surface-900:…; --color-text-primary:… }` 覆盖块**重声明同名变量**(`bg-surface-900` 等工具引用 `var()`,改 var 即翻主题)。
2. **设计一套浅色盘**:浅表面(zinc-50/100/white)+ 深文字(zinc-900/600)+ 品牌琥珀走 `brand-600/700`(浅底对比)+ teal/vendor 保留 + status 升到 `-600`(浅底对比)。
3. **切换入口**:AvatarMenu 在语言下加**一个**图标按钮(lucide Sun/Moon,**单图标**非两个 —— 暗态显 Sun=点亮、亮态显 Moon=点暗),`useWebSettings` 加 `theme` 字段写 `<html>` class;初始默认跟随系统 `prefers-color-scheme`。
4. **不闪**:`index.html` 内联 pre-paint 脚本按存储值先打 class(免 FOUC)+ 切换时加 `.no-transition` 一帧(LAP 的 `disableTransitionOnChange`)。
- **诚实**:① 值命名 token(`surface-900`)在浅色语义会反转(900 变浅)—— 当「基准表面」抽象用即可,或做更大的「角色命名 token(bg/fg/card/border)」重构(churn 大,记为 option)。② 工作量 **中-大**(每个面要在浅色下核对对比度)。这就是它之前压 P2 的原因;owner 既要,纳入。

### D. 整体质感(贯穿所有 wave,不是单独一波)
累积出「production 非 demo」的手感,做成质量条:tabular-nums(数字稳)· 点阵背景 + 主题选区 · 8px 间距节奏 + 统一 radius scale · 微交互(按钮 1px 下沉、卡片发丝环、focus 环、hover 才显的次级操作)· 有品味的浮层/toast 进出动画(全配 reduced-motion)· 排版(Geist 字形特性 `cv02..`、标题 `text-wrap:balance`)· 一致的空/加载态(骨架非文字)· 「配色只表语义」纪律 · **图标统一 lucide**(已是依赖 `^1.14.0`,仅 3 处用 → 推广全站 UI 图标)+ 新增 `brand-icons.tsx`(vendor/transport 品牌 SVG,lucide 无 logo);**清零 emoji** —— ChatConsole 底部导航 4 glyph(`:426-446`)、AvatarMenu 头像盘 🟧🟦🟩🟪⬛ → 纯色圆点(`:17` + `useWebSettings:31` 默认 + `AvatarMenu.test.tsx`)。**每条改动都按这把尺收口。**

### P2 / 视目标再做:S3 ProductSwitcher · S4 图标轨折叠(先核对 `useEdgeSwipe`)· X3 Inspector 事件面板 · CR5 provider 网格 · CR6 OAuth 回跳 toast · S2 iframe 态 · S5 轮询角标。

---

## 四、方案分波(re-sequenced)

- **W1 ── 会话窗口急救(纯前端,可立即部署)= owner 点名两 bug**:输入框(IME 守卫 + Shift/Cmd+Enter + Send/Stop + 草稿 per-sid + 自增高,抽 `Composer.tsx`)· markdown 渲染(已装依赖 + `.cockpit-markdown` + 代码块复制 + 用户/助手不对称)。**不依赖原语库、不碰后端。**
- **W2 ── 前端地基:原语库 + 主题轴 + 质感地基**:`cn()`+`tailwind-merge` · C2-C5(CVA Button/Card/Badge/Input)· D1-D4 CSS · **`@theme inline` 主题感知 + 浅色盘 + AvatarMenu ☀/🌙/系统 + FOUC 守卫**。迁移重复 class 常量。**纯重构 + 主题,baseline 不退。**
- **W3 ── 浮层/选择 + Settings 重设计**:`@base-ui` C6-9 + X1 combobox(替 Marketplace 抽屉 / AvatarMenu 弹层 / 原生 `<select>`)· **Settings 重设计**:窄单列→Card 化(每 provider 一卡:头 = 名+状态徽章,身 = 表单,LAP 式右侧 label→mono 状态读出)· 用户管理→Table(C10/CR3)· masked-token + `<ConfirmInline>`(CR4)· Telegram 加「测试连接」(getMe)· 全走新原语(灭 `FIELD_CLASS`/`*_BTN_CLASS`)。
- **W4 ── 列表/表格/空态 + 会话打磨 + 事件透传**:CR1/CR2/S1 · CR3+C10 把 Status/Hosts 列做成真表格 · chat W2 打磨(贴底滚动/hover 元信息/三态流式)· **小后端**:`eventToRow`+`useSessionEvents` SSE 补 `tool`/`thinking`(透传已有 `CanonicalEvent`,**不新解析终端**)→ 工具活动折叠。

> 切口:**W1(±2)= v0.8.19**;W3-4 滚 v0.8.20+,由 owner 定。

---

## 五、红线(全不破)

- **No prompt injection** / **不解析终端输出** / **progress.jsonl SoT** / **session=一等实体** ── 本轨是展示层 + 一类已有事件透传,不碰。
- **不碰用户 `settings.json`**。新依赖仅 `tailwind-merge`+`class-variance-authority`(必)+ `@base-ui/react`(W3 overlay)+ `@tanstack/react-table`(W4 表格);主流轻量、React19/Tailwind4 兼容;可删没用的 `shiki`。主题不引 `next-themes` 库(`@theme inline`+`useWebSettings` 自管,更轻 + 复用既有持久层)。
- W4 后端**仅 web SSE 透出已有 `CanonicalEvent`**,不新增终端解析。
- 主题切换**不碰 secrets/不动后端**;masked-token / 内联确认红线保留。

---

## 六、验收

- [ ] 中文输入法选词回车**不发送**;切 session 草稿还在;markdown(含代码块/表格)正确渲染、代码块可复制。
- [ ] 全站按钮/卡片/徽章/输入框走 `components/ui/`,无重复 `*_CLASS`;焦点/禁用/按压一致。
- [ ] 浮层走 base-ui(焦点陷阱/Esc/键盘);无原生 `<select>`。
- [ ] **明暗主题**:AvatarMenu **单个**图标按钮切明暗(非两个);切换不闪、不漏未染色面;浅色下对比度达标(正文 ≥ 4.5:1)。
- [ ] **0 emoji**:全站图标走 lucide + `brand-icons.tsx`;ChatConsole nav / AvatarMenu 头像盘无 emoji(`grep -rE "🧩|📊|🖥|⚙|🟧" src` = 0)。
- [ ] **Settings**:Card 化 + 用户管理表格化 + 测试连接;无 `FIELD_CLASS`/`*_BTN_CLASS`;masked-token / 内联确认保留。
- [ ] 质感:成本/计时列 `tabular-nums` 不抖;加载=骨架、空态=图标+CTA;按压/焦点微交互到位。
- [ ] **每 wave baseline 不退**:`cargo test --workspace --exclude ccteam-web` ≥ 现值;`ccteam-web`+vitest+Playwright 不退;clippy 0 warning;`cargo fmt --all` 干净。

---

## 七、不做(out of scope)

- 不重画 shell 布局(ChatConsole 侧栏/底部导航/顶栏结构不动);新观感靠原语库 + 主题轴 + SessionView/Settings 主面。
- 不引状态库(沿用 useState/useRef + 现有 hook + `lib/fetchInterceptor`,后者**更强,别换**);不引 `sonner`/`next-themes`。
- 不抄 LAP 原生 `confirm()`(ccteam 内联确认更优,只 DRY)。
- 角色命名 token 大重构(`surface-*`→`bg/fg/card`)= option,本轨不做(浅色用现名覆盖)。
- 不做 attachments / 新 slash UI;不做版本号外 rename(CLEAT 归 v0.9)。

---

## 八、原型(自包含、离线;即设计 spec)

1. [`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ── 会话窗口:「看现在的渲染 ▸」前后对比;**输入框是真的**(输中文按回车不误发)+ 三点流式 + Send⇄Stop。
2. [`prototype/v0819-ui-kit.html`](prototype/v0819-ui-kit.html) ── 前端地基:「现状 ▸ 改版」对比 按钮/卡片/徽章/表单/tabular-nums/骨架/空态/base-ui 浮层/可搜 combobox;**顶部 ☀/🌙 主题切换**(看原语库在明暗两套都对)。页脚标「已经更好别动」+ @base-ui 决策。
3. [`prototype/v0819-settings.html`](prototype/v0819-settings.html) ── Settings 重设计:窄单列 → Card 化 + 用户管理表格 + 测试连接 + masked-token;**带 ☀/🌙 主题切换**,在真实 Settings 画布上看明暗 + 整体质感。
