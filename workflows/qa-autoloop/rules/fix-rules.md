# Fixer Agent 规则

## 工作目录
所有路径和分支从 `.ccteam/config.json` 读取,不要硬编码。

关键字段:
- `local_path` → 本地代码路径(就是 cwd)
- `github.{owner,repo,fix_base_branch,fix_branch_prefix}`
- `review.reviewer` → 决定请求哪个 reviewer

## Issue 优先级处理顺序
P1 > P2 > P3 > P4

每次运行只处理**一个** issue,优先级最高的 `status="open"`。

## Fix 流程

```
1. 归档 .ccteam/triggers/fixer/*.json → .ccteam/triggers.archived/fixer/
2. 检查 fix_disabled — true 则退出
3. 遍历 .ccteam/issues/*.json,选优先级最高的 open issue
   (jq filter: .status == "open" and (.fix_attempts // 0) < 3 and (.track // "frontend") != "backend")
4. 检查是否已存在同名分支(避免重复工作)
5. cd <local_path>
6. git fetch origin && git checkout <fix_base_branch> && git pull origin <fix_base_branch>
7. git checkout -b <fix_branch_prefix><issue_id>
8. 读取 .ccteam/issues/{issue_id}.json 的 body 字段
9. 分析问题,定位相关代码文件,实施修复
10. git add <changed files> && git commit -m "fix: <描述> (issue <issue_id>)"
11. git push origin <fix_branch_prefix><issue_id>
12. 创建 PR(curl GitHub API)
    - title: fix: <issue title> (<issue_id>)
    - body: "Fixes ccteam issue `<issue_id>`"
    - base: <fix_base_branch>, head: <fix_branch_prefix><issue_id>
13. 按 review.reviewer 请求 reviewer
14. 更新 .ccteam/issues/<issue_id>.json: status → "fixing", pr_number → <new_pr_number>, fix_attempts += 1
15. 创建 .ccteam/prs/<pr_number>.json
16. drop marker 到 .ccteam/triggers/releaser/<pr_number>.json 唤醒 releaser
17. 完事 — 不要 git push .ccteam/(应在 .gitignore)
```

## 代码规范(项目特定)
本模板不规定具体语言 / linter / formatter。建议在项目根放一份 `CODE_STYLE.md`(或类似)说明:
- 语言 / 主框架版本
- 注释语言(中文 / 英文)
- 修改后是否需要跑 lint / build / test(本地或 CI)
- 哪些依赖可以新增,哪些不能

Fixer 修复前先 `cat CODE_STYLE.md`(若存在)读取项目规范。

## 安全约束
- 只修改 `<local_path>` 下的文件
- **禁止修改 `.github/workflows/`**
- **禁止 `git push --force`**
- **禁止 push 到 `github.fix_base_branch` / `github.production_branch`** — 只 push 到 fix 分支
- `fix_attempts >= 3` → status → `"needs-human"`,停止尝试

## GitHub API(使用 `GH_TOKEN` 环境变量)

```bash
OWNER=$(jq -r '.github.owner' .ccteam/config.json)
REPO=$(jq -r '.github.repo' .ccteam/config.json)
BASE=$(jq -r '.github.fix_base_branch' .ccteam/config.json)

# 创建 PR
curl -s -X POST "https://api.github.com/repos/${OWNER}/${REPO}/pulls" \
  -H "Authorization: token $GH_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"title\":\"...\",\"body\":\"...\",\"head\":\"<branch>\",\"base\":\"${BASE}\"}"

# 请求 Copilot review(若 review.reviewer = copilot)
curl -s -X POST "https://api.github.com/repos/${OWNER}/${REPO}/pulls/${PR}/requested_reviewers" \
  -H "Authorization: token $GH_TOKEN" \
  -d '{"reviewers":["copilot-pull-request-reviewer[bot]"]}'
```
