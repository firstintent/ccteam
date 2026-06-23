# v0.8.19 — session 会话窗口改版(web 端可交付质量)

> **状态:设计 + 原型(等 review)**。本版只把 **SessionView 会话窗口**从「demo 感」提到可交付质量;不改架构、不动红线。
> 原型(可点、离线打开):[`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ── 顶栏「看现在的渲染 ▸」切前后对比;底部输入框是**真的**(可输中文测回车不误发)。
> 参考:`references/litellm-agent-platform/src/ui`(同栈 React 19 + Tailwind v4,patterns 可直接抄)。

---

## 一、痛点(= 验收基准)

owner 原话:「web 端简陋的像个 demo,完全不具备面向用户交付的质量」。点名三处:

1. **session 输入框简陋** ── 一不小心**回车把没写完的消息发出去**(尤其中文输入法选词时按回车直接误发半句)。
2. **消息内容渲染不及格** ── agent 吐的 markdown / 代码块 / 表格全是裸文本,`**`、`` ``` ``、`#` 原样显示。
3. **交互不及格** ── 工具调用看不见、没有贴底滚动、没有发送中/可中断态、没有元信息。

映射 `docs/requirements.md`:核心痛点「从手机/web 驱动云端 AI 团队」── 驱动入口本身不可用 = 价值漏在最后一公里。

---

## 二、现状诊断(代码 SoT,精确到行)

整个 SPA 在 `crates/ccteam-web/web/`(React 19 + Vite 8 + Tailwind v4,token 在 `web/src/index.css` 的 `@theme`)。会话窗口全在 **`web/src/pages/SessionView.tsx`**。

### 2.1 输入框 ── `SessionView.tsx:400-420`
```tsx
onKeyDown={(event) => {
  if (event.key === "Enter" && !event.shiftKey) {   // ← 没有 isComposing 守卫
    event.preventDefault();
    submit();
  }
}}
```
- **根因:零 IME 守卫**。全 SPA grep 不到 `isComposing` / `onCompositionStart`。中文/日文输入法按回车**确认候选词**时,这个 handler 直接把半句 Pinyin/残句发出去 ── 对 zh-default 用户是高频踩坑。
- 草稿 `useState("")` 在 keyed `SessionView` 内,**切 session 即丢未发草稿**(`ChatConsole` 用 `key={sid}` remount)。
- textarea 靠 `resize-y` 手动拖,**不随内容自增高**。
- 无 Cmd/Ctrl+Enter 备选、无发送中禁用、无可中断(Stop)态。

### 2.2 渲染 ── `SessionView.tsx:382-395`
```tsx
<div className="… whitespace-pre-wrap break-words …">{row.content}</div>
```
- `{row.content}` 是**裸字符串**,唯一格式化是 CSS `whitespace-pre-wrap`。无 markdown、无代码块、无表格、无语法高亮。
- **依赖其实已装**:`marked` + `dompurify`(已在 `MarketplaceView` 用)、`remark-gfm`、`shiki`(装了没用);样式 **`.cockpit-markdown`**(`index.css:128-230`,完整的 p/h/ul/pre/table/code)**已存在但只接在市场 README,没接进会话**。
- 事件模型有损:`chatTranscript.ts:eventToRow` 只留 `answer` + 收尾 `progress`,**把 tool / thinking / 流式中间态全丢了** ── 所以工具调用在会话里基本不可见。

### 2.3 交互
- 无贴底滚动锚定(读历史时新 token 会把你拽到底)。
- 无工具活动展示、无发送中/可中断指示、无 per-turn 元信息(model / 耗时 / token / 成本)。

---

## 三、参考 litellm 抄什么(带指针)

litellm 与 ccteam **同栈**(React 19 / Tailwind v4),`references/litellm-agent-platform/src/ui`:

| 抄什么 | 出处 | 为什么提质 |
|---|---|---|
| `.sessions-md` markdown CSS 层 + `react-markdown`+`remark-gfm` | `globals.css:213` / `message-block.tsx:385` | **零 JS、不要高亮器**就有 premium markdown(代码框/表格/inline code chip),全用 token → 暗色自动对。ccteam 的 `.cockpit-markdown` 已是同款,接上即可 |
| 输入框 Send⇄Stop 形变 + 草稿保留 + context-aware placeholder | `composer.tsx:30,90` | 一个按钮:能发=↑ 发送,agent 忙且空草稿=红■ 中断;`current.trim()===t` 才清空(发送途中继续打字不丢) |
| **IME 守卫(litellm 缺,我们要补)** | `composer.tsx:48` | litellm 也没 `isComposing` ── 对 CJK 是 bug,我们**改进**而非照抄 |
| 贴底滚动锚定 | `chat/page.tsx:881` | `wasNearBottom`(120px 阈值)+ 变更时按需 snap,~12 行无依赖 |
| 工具活动折叠 `groupRenderItems`+`ToolBlock` | `message-block.tsx:331,521` | 连续工具调用并成一个「活动」块:图标 + 人类化名 + 一行摘要 + 状态 pill + 点开看 JSON |
| 用户气泡 / 助手文档 不对称 + hover 元信息 | `message-block.tsx:136,204,309` | 右对齐限宽用户气泡 + 全宽无头像助手正文 = 现代 chat 观感;hover 才显 model·耗时·token·成本 |
| 三态流式指示 | `message-block.tsx:275-301` | thinking 转圈 / 流式三点(错峰)/ 排队 / 失败红,均配 `motion-reduce` |
| 内联可编辑 HITL 批准卡 | `tool-approval-panel.tsx` | 比现在 `[同意][拒绝]` 富:批准前可改 args(本版先做富展示,可编辑 args 选做) |

---

## 四、改版方案(分波)

### Wave 1 ── 纯前端,直接兑现 owner 点名的两个 bug(零后端)
1. **输入框**(`SessionView.tsx:403-408` + 抽成 `components/Composer.tsx`):
   - 加 IME 守卫 `!event.nativeEvent.isComposing && event.keyCode !== 229`;
   - Shift+Enter 换行(已有)+ Cmd/Ctrl+Enter 发送;
   - textarea 加 `field-sizing:content` + `max-h` 自增高;
   - Send⇄Stop 形变(忙且空草稿→中断,走现有 `…/stop`);
   - 草稿提升到 per-sid 持久(`localStorage` 复用 `chatTranscript.ts` 的 per-sid 套路),切 session 不丢;
   - 底部 hint:`Enter 发送 · Shift+Enter 换行 · 输入法候选回车不误发`。
2. **渲染**(`SessionView.tsx:382-395`):裸 `{row.content}` → markdown。复用**已装**的 `marked`+`dompurify`(与 `MarketplaceView` 同路径)或 `react-markdown`+`remark-gfm`,挂 **已存在的 `.cockpit-markdown`** class;代码块加 chrome + 复制按钮;用户气泡 / 助手全宽文档不对称。

> Wave 1 完即可部署:owner 点名的「回车误发 + 渲染不及格」当场消失,**不碰后端**。

### Wave 2 ── 交互打磨(前端)
- 贴底滚动锚定(litellm `wasNearBottom`)+「↓ 回到最新」pill;
- per-turn hover 元信息(model · 耗时 · token · 成本,数据已在 `chat_turn_completed` usage);
- 三态流式指示(thinking / 流式三点 / 失败)。

### Wave 3 ── 小后端(owner 允许的「必要小范围重构」)
- **事件模型补 tool/thinking part**:`chatTranscript.ts:eventToRow` + `useSessionEvents.ts` 的 SSE 形 `{kind:"answer"|"progress"}` → 增 `tool`/`thinking` kind;daemon `spawn_event_pump` 本就 emit `CanonicalEvent`(含工具),只是 web SSE 桥没透出 → 透出后前端做工具活动折叠 + 思考块。
- **红线守住**:仍是「读 transcript jsonl + 官方 hook fast event」,**不 scrape pane**;只是把已有的 `CanonicalEvent` 多搬一类到 web SSE,不新解析终端。

### 设置(轻量,放本版尾或顺手)
- Settings 卡片密度 + label→mono-value 状态列(litellm settings 风格);个人设置(头像/语言)已在 v0.8.18 落地,本版只统一卡片观感,不重做。

---

## 五、红线(全部不破)

- **No prompt injection** / **不解析终端输出** / **progress.jsonl 是 SoT** / **session=一等实体** ── 本版是**纯展示层 + 一类已有事件的透传**,不碰这些。
- **不碰用户 `settings.json`**;markdown 走已装依赖,无新增重依赖(可去掉没用的 `shiki`)。
- Wave 3 的后端改动**仅 web SSE 透出已有 `CanonicalEvent`**,不新增对终端的解析。

---

## 六、验收

- [ ] 中文输入法选词按回车**不发送**(只确认候选);写完按 Enter 才发;Shift+Enter 换行。
- [ ] 切 session 再切回,未发草稿还在。
- [ ] agent 的 markdown(标题/列表/**粗体**/`inline code`/```代码块```/表格)正确渲染;代码块可一键复制。
- [ ] 工具调用以「活动」折叠块可见(Wave 3 后)。
- [ ] 读历史时新消息不把你拽到底;有「回到最新」。
- [ ] baseline 不退:`cargo test --workspace --exclude ccteam-web` ≥ 现值;`ccteam-web` 测试 + vitest + Playwright 不退;clippy 0 warning;`cargo fmt --all` 干净。

---

## 七、不做(out of scope)

- 不重画 shell(ChatConsole 侧栏 + 底部导航 + 顶栏不动);新观感只在 SessionView 主面。
- 不引入状态库(沿用 useState/useRef + 现有 hook)。
- 不做 attachments / 新 slash UI(ccteam 已有真实 slash + 附件,litellm 反而是占位)。
- 可编辑-args 的 HITL 批准卡:本版先做富展示,**可编辑**选做(不阻塞)。
- 不做版本号以外的 rename(CLEAT rename 归 v0.9 线)。

---

## 八、原型

[`prototype/v0819-chat-console.html`](prototype/v0819-chat-console.html) ── 自包含、离线打开。已接的交互:
- 顶栏 **「看现在的渲染 ▸」**:前后对比(裸文本 ↔ 渲染后),一眼看见 delta;
- 底部 **输入框是真的**:输中文按回车不误发(compositionstart/end 守卫)、Shift+Enter 换行、发送后三点指示 + Send⇄Stop 形变;
- 工具活动折叠、代码块复制、HITL 批准、贴底滚动 pill 均可点。

原型即设计 spec:页脚标了**谁拥有什么 / 本版明确不做什么**。
