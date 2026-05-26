# Tester Agent 规则

## 【铁律】自动化测试只在 Staging 环境执行

所有自动化测试(tester / acceptance)**只针对 staging 环境**,严禁对 production 执行写操作或破坏性测试。

测试 URL 始终从 `.ccteam/config.json::test.staging_url` 读取,`test.active_env` 保持 `"staging"`。
Production URL 仅人工访问确认,不做任何自动化。

## 测试分层

具体测试内容是**项目特定的**,本模板不规定。但每个项目应该把测试分成至少两层:

### Smoke Tests(快速 happy-path,约 2 分钟)
- 关键页面 / 端点的 HTTP 2xx/3xx
- 关键 happy-path 用户旅程(登录 / 主流程操作 / 主要按钮可点击)
- 控制台无 JS 错误 / API 无 5xx

设计目标:**通过 = 部署没烂**;失败 = P1/P2 阻断。

### Coverage Tests(深度,约 3-5 分钟)
- 边缘 page / 路由可访问
- 移动端 / 响应式
- 异常输入 / 错误恢复
- 性能(加载时间)
- 已知边界 / 历史 bug 回归

设计目标:**通过 = 用户体验合格**;失败 = P3/P4 改进。

### Regression Tests
对 `.ccteam/issues/*.json` 中 `status=closed` 的 issue 执行定向验证,确保未退化。

## 优先级分类规则

| 优先级 | 标准 |
|---|---|
| P1 | 核心功能不可用(关键页面 404/500、主流程中断)|
| P2 | 影响用户体验但不阻断核心流程(SEO 错误、内容异常、明显视觉破缺) |
| P3 | 功能缺失或内容为空(dashboard 空白、功能按钮无效) |
| P4 | 改进建议、测试覆盖不足、非阻断性问题 |

**tester 只对 P1/P2 建 issue**(写 `.ccteam/issues/<id>.json`)。P3/P4 仅在输出报告中记录,不写文件。

## 去重规则

创建 issue 前检查 `.ccteam/issues/*.json`:
```bash
grep -l "<标题关键词>" .ccteam/issues/*.json \
  | xargs -I {} jq -r 'select(.status != "closed")' {} 2>/dev/null
```
若存在 `status != "closed"` 且标题相似(关键词重叠 > 60%)的 issue → 跳过,不重复创建。

## Issue 格式

```json
{
  "id": "issue-<YYYY-MM-DD>-<rand4>",
  "title": "[P<级别>] <简短描述>",
  "priority": "P<级别>",
  "status": "open",
  "track": "frontend",
  "scenario_id": "E0XX",
  "body": "## 现象\n<具体描述>\n\n## 复现步骤\n1. ...\n2. ...\n\n## 期望结果\n<应该是什么>\n\n_Tester Agent — 场景 E0XX,<日期>_",
  "labels": ["bug"],
  "pr_number": null,
  "fix_attempts": 0,
  "created_at": "<ISO8601>",
  "source": "tester",
  "screenshot": "<artifact_dirs.screenshots>/explore-<id>-<ts>.png"
}
```

## 跨工具共享约定

- **screenshot 路径**:`artifact_dirs.screenshots`(默认 `/tmp/screenshots/<config.name>`),所有 agent 共享同一目录
- **本地 state 不入 git**:`.ccteam/` 应在项目根 `.gitignore`;tester 永远不要 `git add .ccteam/`
- **后端 issue**(`track: "backend"`):tester 仍写到 `.ccteam/issues/`,但 fixer 看到 `track=backend` 会跳过(后端 bug 通常由人工处理)
