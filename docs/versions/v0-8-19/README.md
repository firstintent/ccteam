# v0.8.19+ — web 前端可交付质量(整个前端,不止 chat)

> **状态:设计 + 原型(等 review)**。owner:web 端「简陋的像个 demo,完全不具备面向用户交付的质量」+「整个前端考虑,不止 chat」。
> 参考 **LAP** = `references/litellm-agent-platform/src/ui`(Next16 但**同栈** React19 + Tailwind4 + base-ui/shadcn + CVA;patterns 可直接抄)。owner 允许小范围后端改动配合。
> 原型(自包含、离线):① 会话窗口 [`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ② 前端地基/原语库 [`prototype/v0819-ui-kit.html`](prototype/v0819-ui-kit.html) ── 都带「现状 ▸ 改版」前后对比。

---

## 一、痛点(= 验收基准)

owner 点名 chat 三处(回车误发 / 渲染不及格 / 交互不及格),并要求**扩到整个前端**。深扫后,根因不止 chat ——

**结构性根因:ccteam SPA 没有组件原语层。** 没有 `components/ui/`,每个页面都从裸 Tailwind 重新拼按钮/卡片/徽章/输入框,class 串以模块常量重复(`SettingsPage.tsx:46-57` 的 `FIELD_CLASS`/`PRIMARY_BTN_CLASS`/`GHOST_BTN_CLASS`,又在 `MarketplaceView`/`StatusView`/`HostsView`/`AvatarMenu` 内联重抄)。已装 `clsx`+`lucide-react` 但**无 `cn()`、无 `tailwind-merge`、无 CVA**。→ 焦点态/禁用态/按压反馈/徽章拼写各页不一致 = 「demo 感」的真正来源。chat 是最响的一处,不是唯一一处。

映射 `docs/requirements.md`:核心痛点「从手机/web 驱动云端 AI 团队」── 驱动入口不可用 = 价值漏在最后一公里。

---

## 二、现状诊断(代码 SoT)

SPA = `crates/ccteam-web/web/`(React19 + Vite + Tailwind4,token 在 `index.css @theme`,**dark-only**)。

### 2.1 chat 会话窗口 ── `pages/SessionView.tsx`
- **输入框 `:403-408`**:Enter handler **零 IME 守卫**(全 SPA grep 不到 `isComposing`)→ 中文选词回车直接发半句。草稿 `useState` 切 session 即丢;textarea 不自增高;无 Send/Stop。
- **渲染 `:382-395`**:`{row.content}` 裸字符串,无 markdown/代码块/表格。**依赖已装**(`marked`+`dompurify` 已在 `MarketplaceView` 用;`remark-gfm`/`shiki` 装了没用),**`.cockpit-markdown` 样式已存在**(`index.css:128-230`)只是没接进会话。
- **事件有损**:`chatTranscript.ts:eventToRow` 丢掉 tool/thinking/流式中间态。

### 2.2 整个前端
- **无 `components/ui/`**(见 §一)── 主导问题。
- 浮层全手搓:Marketplace 抽屉(`fixed inset-0 bg-black/50` + 手写 Escape/click-away)、AvatarMenu 弹层(手写 `<button fixed inset-0>` click-away)→ 焦点陷阱/键盘/定位易错。
- 原生 `<select>`(Marketplace 项目/源筛选、NewSessionModal)无法染色、不能搜 → 破暗色主题。
- 加载态用文字(「加载市场目录中…」),空态用纯文字(「还没有用户。添加一个吧。」)→ 不如骨架屏 + 图标+CTA 空态。
- 列表全是手写 `.map` div(StatusView fleet、Hosts agent 列),不能排序/筛选/搜。
- 排版无 `tabular-nums` → 成本/计时列数字抖动变宽。

### 2.3 ✅ 已经做得好/更好,**别动**(避免无谓 churn)
- **fetch/auth 基建** `lib/fetchInterceptor.ts`:全局 fetch 拦截 + Bearer 注入 + `X-Aoe-Token` 轮换 + 401 分类 + 去抖 + 5xx→toast。**比 LAP `lib/api.ts` 厚、更强。**
- **Toast** `Toasts.tsx` + `toastBus` + `reportError`(slide-up + `role=alert/status` + 自动消失)── 已覆盖,**别引 `sonner`**。
- **内联确认删除** `SettingsPage.tsx:354-380`(删除?→确认/取消)── **比 LAP 的原生 `confirm()` 好**(LAP 自己注释说 confirm 破暗色)。只需抽成可复用 `<ConfirmInline>`,不抄 LAP。
- **4 态页面**(loading/error/empty/success)、**token-only 配色纪律**、**`prefers-reduced-motion`**、**focus-visible 环**、**键盘快捷键**(`useKeyboardShortcuts`,LAP 没有)、**status 语义色**(比 LAP 全)── 均已具备或更强。

---

## 三、从 LAP 吸收什么

### A. 会话窗口(chat)── 见原型 `v0819-chat-console.html`
| 抄什么 | LAP 出处 | 提质 |
|---|---|---|
| `.sessions-md` markdown CSS + react-markdown/remark-gfm | `globals.css:213`/`message-block.tsx:385` | 零 JS、不要高亮器的 premium markdown(ccteam `.cockpit-markdown` 已是同款,接上即可)|
| 输入框 Send⇄Stop 形变 + 草稿保留 + context placeholder | `composer.tsx:30,90` | 一按钮:能发=↑,忙且空草稿=红■中断;`current.trim()===t` 才清空 |
| **IME 守卫(LAP 也缺 → 我们改进)** | `composer.tsx:48` | `!isComposing && keyCode!==229`,CJK 不误发 |
| 贴底滚动锚定 / 工具活动折叠 / 用户气泡·助手全宽不对称 / 三态流式 / hover 元信息 | `chat/page.tsx:881`、`message-block.tsx:331,136,275,309` | 见原型 |

### B. 整个前端(分层)── 见原型 `v0819-ui-kit.html`

**地基(CSS 一行级,可整段 lift):**
| # | 抄什么 + LAP 出处 | ccteam 落点 | 提质 | 量 |
|---|---|---|---|---|
| D1 | 全局 `tabular-nums` + `font-feature-settings`(`globals.css:166`)| CostPill/Status 成本·预算/Hosts 版本/侧栏 timeAgo | 数字等宽不抖 —— 成本台最明显的「工程感」| 小 |
| D2 | 表头 uppercase-tracking + 全局标题字阶(`globals.css:170`)| 所有非 chat 页头 + 未来表格 | 一条基规替掉 ~8 处重复页头串 | 小 |
| D3 | 暗色点阵背景(`globals.css:123`)| `index.css body` | 空 Status/Hosts/市场 画布显质感,零成本 | 小 |
| D4 | 主题化 `::selection`(`globals.css:130`)| 全局选区 | 默认蓝选区与暗琥珀冲突 | 小 |

**组件原语库(核心吸收,先落它):** 立 `components/ui/` + `cn()`(`lib/utils.ts`=`twMerge(clsx())`)。**决策点:overlay 类(Dialog/Menu/Select/ScrollArea)采 `@base-ui/react`**(headless,Radix 维护继任,白送 portal/定位/焦点陷阱/键盘);Button/Card/Badge/Input 用**零运行时依赖的 纯元素 + CVA**。
| # | 原语 + LAP 出处 | 落点 | 提质 | 量 |
|---|---|---|---|---|
| C1 | `cn()`+`tailwind-merge`(`lib/utils.ts`)| C2-C10 地基 | 后置 class 安全覆盖,变体组件前提 | 小 |
| C2 | `Button` CVA(`ui/button.tsx`):default/outline/ghost/destructive/link × xs/sm/lg/icon + `active:translate-y-px` + `focus-visible:ring-3` + `[&_svg]:size-4` | 全站按钮(替 `*_BTN_CLASS`)| 按压反馈/焦点/禁用/图标尺寸一处定 | 中 |
| C3 | `Card` 族(`ui/card.tsx`):`ring-1 ring-foreground/10` 发丝环 + Header/Action/Footer 槽 | StatusView/HostsView 卡(现手搓)| 比实线边更细;标准化带操作的卡 | 小中 |
| C4 | `Badge` CVA(`ui/badge.tsx`)| 市场源/已装 pill、Status 活动 pill、Hosts 就绪 pill(现 6 种拼写)| 收敛成一个 variant 属性 | 小 |
| C5 | `Input`/`Textarea`/`Label`(共享焦点/invalid/disabled;Textarea `field-sizing-content` 自增高)| 全站表单(替 `FIELD_CLASS`)| 一致态 + Lark open_ids 自增高 | 小 |
| C6 | `Dialog`(base-ui,portal+backdrop-blur+焦点陷阱+动画)| Marketplace 抽屉(手搓 modal)| 替掉手搓的焦点/Escape/scroll-lock | 中 |
| C7 | `DropdownMenu`(base-ui,键盘 nav+`destructive` item)| AvatarMenu 弹层 | 真键盘/定位/click-away | 中 |
| C8 | `Select`(base-ui,portal+动画+勾选)| 替所有原生 `<select>` | 可染色/键盘/动画 | 中 |
| C9 | `ScrollArea`(base-ui,overlay 滚动条)| 长列表 | 跨浏览器细滚动条(补 FF)| 小中 |
| C10 | `Table` 族(`ui/table.tsx`)| 喂 Layer4 表格 | 语义表格原语 | 小 |

**壳/导航 + CRUD 页 + 跨切面:**
| # | 抄什么 + LAP 出处 | 落点 | 提质 | 量 |
|---|---|---|---|---|
| S1 | 乐观删除+回滚(`sidebar.tsx:112`)| ChatConsole 会话删除等 | 即时反馈+正确回滚(替整列表重拉)| 小 |
| CR1 | 骨架屏(`agents/edit:441` `animate-pulse` 形状占位)| 市场/Status/Hosts/Settings 加载 | 形状骨架比文字「加载中」快感强 | 小 |
| CR2 | 富空态=图标+文案+CTA(`vault:97`)| 市场/用户 空态 | 死胡同变下一步动作 | 小 |
| CR3 | `@tanstack/react-table` + 去范式 `searchText` + 三态排序头 + 计数筛选 pill(`agents/agents-table.tsx`)| StatusView fleet + Hosts 列 → 真表格 | 可排序/筛选/搜的 admin 表 | 中/张 |
| CR4 | **抽 ccteam 自己的内联确认**成 `<ConfirmInline>`(非抄 LAP)| Settings/市场/会话删除 | DRY 既有更优实现 | 小 |
| X1 | 可搜索·portal·翻转感知 combobox(`model-select.tsx`:下方空间<180 向上开 + scroll/resize 重排 + 非受控搜索框)| 替 model/role/project 选择器 | many-option 选择器该有的搜+翻转;LAP 最可复用的独立组件 | 中 |

**P2 / 视目标再做(别盲上):** X2 `next-themes` 明亮模式(ccteam 故意 dark-only,要 light 才做)· S3 ProductSwitcher / S4 图标轨折叠(先核对既有 `useEdgeSwipe` 移动端)· X3 Inspector 原始事件面板 · CR5 provider 按钮网格 · CR6 OAuth 回跳 toast+清 URL · S2 iframe 嵌入态 · S5 轮询角标。

---

## 四、方案分波(re-sequenced;chat 急救先走,地基随后承载其余)

- **Wave 1 ── 会话窗口急救(纯前端,可立即部署)= owner 点名的两个 bug**
  输入框(IME 守卫 + Shift/Cmd+Enter + Send/Stop + 草稿 per-sid 保留 + 自增高,抽 `components/Composer.tsx`)· markdown 渲染(已装依赖 + `.cockpit-markdown` + 代码块复制 + 用户/助手不对称)。**做完即部署,不依赖原语库、不碰后端。**
- **Wave 2 ── 前端地基:组件原语库** `cn()`+`tailwind-merge` · D1-D4 CSS · C2-C5(Button/Card/Badge/Input/Textarea/Label,零依赖 CVA)· 迁移重复 class 常量。**纯重构、零行为变化,baseline 必须不退。**
- **Wave 3 ── 浮层与选择(`@base-ui/react`)** C6-C9 + X1 combobox;替 Marketplace 抽屉 + AvatarMenu 弹层 + 原生 `<select>`。**最大正确性收益**(手搓浮层最易错)。
- **Wave 4 ── 列表/空态/表格 + 会话打磨 + 事件透传** CR1/CR2/S1/CR4;CR3+C10 把 StatusView/Hosts 列做成真表格;chat Wave2 打磨(贴底滚动/hover 元信息/三态流式);**小后端**:`eventToRow`+`useSessionEvents` SSE 补 `tool`/`thinking`(透传已有 `CanonicalEvent`,**不新解析终端**)→ 工具活动折叠。

> 版本切口:**Wave 1(±2)= v0.8.19**;Wave 3-4 滚入 v0.8.20+(由 owner 定切口)。

---

## 五、红线(全不破)

- **No prompt injection** / **不解析终端输出** / **progress.jsonl 是 SoT** / **session=一等实体** ── 本轨是展示层 + 一类已有事件透传,不碰。
- **不碰用户 `settings.json`**。markdown 走已装依赖(可删没用的 `shiki`)。新增依赖仅 `tailwind-merge`(必)+ `class-variance-authority`(必)+ `@base-ui/react`(Wave 3,overlay 专用)+ `@tanstack/react-table`(Wave 4,表格);均主流轻量、Tailwind4/React19 兼容。
- Wave 4 后端**仅 web SSE 透出已有 `CanonicalEvent`**,不新增终端解析。

---

## 六、验收

- [ ] 中文输入法选词回车**不发送**;写完 Enter 才发;Shift+Enter 换行;切 session 草稿还在。
- [ ] agent markdown(标题/列表/**粗体**/`code`/```块```/表格)正确渲染;代码块可复制。
- [ ] 全站按钮/卡片/徽章/输入框走 `components/ui/`,无重复 `*_CLASS` 常量;焦点/禁用/按压一致。
- [ ] 浮层(市场抽屉/头像菜单)走 base-ui,有焦点陷阱/Escape/键盘 nav;无原生 `<select>`。
- [ ] 加载=骨架屏、空态=图标+CTA;成本/计时列 `tabular-nums` 不抖。
- [ ] StatusView/Hosts 列可排序/筛选/搜(Wave 4)。
- [ ] **每 wave baseline 不退**:`cargo test --workspace --exclude ccteam-web` ≥ 现值;`ccteam-web` 测试 + vitest + Playwright 不退;clippy 0 warning;`cargo fmt --all` 干净。

---

## 七、不做(out of scope)

- 不重画 shell 布局(ChatConsole 侧栏/底部导航/顶栏结构不动);新观感靠原语库 + SessionView 主面。
- 不引状态库(沿用 useState/useRef + 现有 hook + `lib/fetchInterceptor`,后者**更强,别换**)。
- 不引 `sonner`(toast 已有)、不抄 LAP 的 `confirm()`(ccteam 内联确认更优,只 DRY)。
- 不抄 LAP 明亮模式(dark-only 是刻意身份;light 模式 P2,视目标)。
- 不做 attachments / 新 slash UI(ccteam 已有真实 slash + 附件)。
- 不做版本号外 rename(CLEAT rename 归 v0.9 线)。

---

## 八、原型

1. [`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ── 会话窗口:顶栏「看现在的渲染 ▸」前后对比;**输入框是真的**(输中文按回车不误发)+ 三点流式 + Send⇄Stop。
2. [`prototype/v0819-ui-kit.html`](prototype/v0819-ui-kit.html) ── 前端地基/原语库:「现状 ▸ 改版」对比 按钮变体 / 卡片发丝环 / 徽章 / 表单 / tabular-nums / 点阵质感 / 骨架屏 vs「加载中」/ 富空态 / base-ui 浮层 vs 手搓 modal / 可搜 combobox vs 原生 select。页脚标了**已经更好别动的清单** + `@base-ui` 决策建议。

原型即设计 spec。
