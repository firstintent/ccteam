---
name: ship
required_inputs:
  - .ccteam/test-report.md
  - .ccteam/implement-report.md
required_outputs:
  - .ccteam/retro.md
soft_cost_warn_usd: 2.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
---

# 任务:收尾交付

按顺序:

1. 运行最终测试一次,确认全绿(否则走异常出口,reason 写不绿原因)
2. 创建 git commit(message 简洁说明本项目做了什么)
3. 写 retro,**三处落地**(基于 `teams/dev/team.yaml.retro_schema`):

   a. **本项目 retro 报告** → `retro.md`,30 行内回顾:
      - 实际花费 vs plan-eng 估算
      - 关键技术决策(后续项目能复用的)
      - 踩过的坑

   b. **本仓库 auto-memory** → 调 `/memory` 写入项目特定 lessons,topic 文件结构你自定;
      只记本仓库未来延续会用到的事(不重复 retro.md 内容)。

   c. **跨项目 lessons 库** → 用 `Edit` 修改 `~/.claude/rules/ccteam-lessons-dev.md`,
      **只改 `<!-- ccteam-managed:lessons begin/end -->` 之间内容**(不动标记,也不动
      marked 外的用户段)。在 marked section 内 append 一段以本项目 slug + 日期为
      H2 标题的新条目;字段顺序与 description 取自 `teams/dev/team.yaml.retro_schema`
      (每字段一个 H3):

      - `tech_stack` — Languages, frameworks, key libraries used
      - `pitfalls` — Mistakes / surprises to avoid next time
      - `successful_designs` — Design choices that paid off
      - `do_not_do_again` — Anti-patterns observed

      格式:

      ```
      ## <项目 slug> (YYYY-MM-DD)

      ### tech_stack
      <一段总结>

      ### pitfalls
      <一段总结>

      ### successful_designs
      <一段总结>

      ### do_not_do_again
      <一段总结>
      ```

      若 `~/.claude/rules/ccteam-lessons-dev.md` 不存在,说明用户没跑过
      `ccteam doctor --install-memory-bridge`;在 retro.md 里留一句
      "memory bridge missing — run ccteam doctor --install-memory-bridge",
      跨项目 lessons 这次跳过(本项目已 ship,不走异常出口)。

4. 若项目有 README 需求则生成
