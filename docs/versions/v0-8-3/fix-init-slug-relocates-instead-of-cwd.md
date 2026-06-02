# v0.8.3 bugfix-plan —— `ccteam init --slug <name>` 不该把项目搬到 `~/projects/`

> **状态**:**方案 A 已被 owner 拍板**(2026-06-02)——「可指定项目 path dir;slug 只是标记名称;不指定时 path 默认 = pwd」。进入实现。
> **设计裁决点**:见 §3「方案」——A 选定;B/C 留档备查。caps 子决策(§6)采 (a)**严格 + 更好报错**,(b) 自动小写留作 owner 可 veto 的备选。

## 0. 一句话

用户在一个**已有代码仓**(`~/nasworkspace/AgentServe`)里跑 `ccteam init --slug agentserver`,
期望「就地把当前目录初始化成 ccteam 项目、顺便起名 agentserver」;实际 ccteam **静默另建**了一个空的
`~/projects/agentserver/` 骨架,用户真正的代码一行没进去。`--slug` 这个参数被重载成了
「**重命名** + **搬家到 `projects_root`**」两件事,而用户只想要前者。

## 1. 复现(用户原样)

```
rob@nas-box005:~/nasworkspace/AgentServe$ pwd
/home/rob/nasworkspace/AgentServe          # ← 用户的真实代码仓在这

rob@nas-box005:~/nasworkspace/AgentServe$ ccteam init --slug agentserver
  target dir       /home/rob/projects/agentserver     # ← 却装到了这,空目录
  slug             agentserver
  ...
```

期望:`target dir` = `/home/rob/nasworkspace/AgentServe`(当前目录就地初始化)。
实际:`/home/rob/projects/agentserver`(`projects_root` 下另起的空骨架)。

## 2. 根因(代码级)

### 2a. 主缺陷 —— `--slug` 触发搬家

`crates/ccteam-cli/src/commands.rs:313` `resolve_install_target` 的优先级:

```rust
fn resolve_install_target(paths, opts) -> Result<PathBuf> {
    if let Some(p) = &opts.install_in { ... return abs; }      // 1. --in <path>
    if let Some(slug) = &opts.slug {                            // 2. --slug <name>  ← 元凶
        let target = paths.projects_root.join(slug);           //    = ~/projects/<slug>
        std::fs::create_dir_all(&target)?;
        return Ok(target);
    }
    std::env::current_dir()                                     // 3. cwd
}
```

只要给了 `--slug`(且没给 `--in`),就走分支 2 搬到 `projects_root/<slug>`,**绕过 cwd**。

### 2b. 这是 drift,不是设计本意 —— 关键证据

- **顶层 `Init` 文档**(`main.rs:52-54`)写的是:`--slug <name>` 用来「**override the derived slug**」
  —— 即**改名、留在 cwd**。这才是文档化的本意。
- **搬家行为只活在** F72 的字段级注释(`commands.rs:55-58` / `main.rs:68-70`)+ `resolve_install_target` 分支 2。
- 所以**方案 A 不是新设计,是把跑偏的 F72 分支 2 拉回顶层文档早就写明的本意**。
  这把「改 `--slug` 语义」从「breaking change」重新定性为「**修 drift**」——对 owner review 更站得住。

### 2c. 次缺陷(被 A 顺带消灭)—— `init --slug` 与 `new` 语义不一致

- `ccteam new <slug>`(`main.rs:2640-2672` `run_new`):**会补 team 前缀** → `~/projects/dev-agentserver`,
  符合红线「新建项目走 `<projects_root>/<team>-<slug>/`」。
- `ccteam init --slug agentserver`:分支 2 **不补前缀** → `~/projects/agentserver`。
- 两条路被 `main.rs:217-224` 注释宣称「**Identical semantics**」,实际产出**不同目录**——注释是错的。

> **A 自动消灭 2c**:一旦 `init --slug` 不再搬家,`init` 就永远不会产出 `projects_root/<slug>`,
> team-前缀逻辑只剩 `run_new` 一处,不一致从根上消失。**这是一处改动,不是两个互相打架的补丁。**

## 3. 方案

### 方案 A(推荐)—— `init` 永远就地(cwd),`--slug` 只改名,搬家归 `new`

- `resolve_install_target` **删掉分支 2**。新优先级:`--in <path>` → cwd。
- `--slug` 退化为**纯名字 override**:只改注册到 `config.yaml::projects[]` / `state.json` 的 slug,**绝不改安装位置**。
- 「在中心目录新建项目」的心智 → 用**已存在**的 `ccteam new <slug>`(保留 team 前缀、内部走 `--in`)
  或显式 `ccteam init --in <path> --slug <name>`。功能零损失,只是砍掉 `init --slug` 这条**多余且反直觉**的第三条路。
- **配套改文档**(都已变成假话):
  - `main.rs:68-70`(`InitOptions.slug` 字段 doc)— 删「installs at `<projects_root>/<slug>/`」。
  - `commands.rs:55-58` + `commands.rs:309-312`(`resolve_install_target` 优先级注释)。
  - `main.rs:217-224`(`New` 注释「Identical semantics to `init --slug`」)— 改成「`init --in <projects_root>/<team>-<slug>`」。

**优点**:对齐 `git init`/`npm init` 心智;对齐 CLAUDE.md §〇「`ccteam init` 当前布局:**项目内写** `.ccteam/...`」;一处改动同时灭主缺陷 + 次缺陷;pre-v1.0「breaking rename 不留 alias」,无需 compat shim。
**代价**:`init --slug X` 行为变了(原来搬家)。但 ① 它本就违背顶层文档;② `ccteam new X` 完整覆盖原意图;③ 见 §5 测试审计——**没有任何现存测试**编码了这条搬家路径,改它零回归。

### 方案 B(保守替代)—— 保留搬家,但 cwd 像真仓时**拒绝并提示**

- `--slug` 仍搬家;但当 `--slug` 给了**且** cwd 非空 / 是 git 仓时,**报错**:
  「你在一个已有仓里;就地初始化请去掉 `--slug` 跑 `ccteam init`,或新建中心项目用 `ccteam new <slug>`」。
- **优点**:改动更小,不动 `--slug` 语义。**缺点**:保留了重载心智 + 2c 不一致;heuristic(「像不像仓」)本身易误判;且 §6 的「caps 仓名」问题没解(见下)。

### 方案 C(否决)—— 只给 `init --slug` 补 team 前缀

- 只修 2c(让 `init --slug` 也产 `dev-agentserver`),**不解用户的实际诉求**(仍然搬家、仍然不在 cwd)。**不足,否决。**

## 4. 用户当前残留清理(无论选 A/B,都要先做)

`~/projects/agentserver/` 是个**只含 ccteam 骨架、不含 AgentServe 代码**的空项目,已注册进 `config.yaml`。清理:

```bash
ccteam remove agentserver --purge      # 去注册 + 删 .ccteam/.claude/agents/workflow.yaml(不碰业务码/.env)
rmdir ~/projects/agentserver           # 骨架清掉后,空壳目录手动删
```

**修复后正确的重跑**(注意 §6:`AgentServe` 含大写,bare `init` 会因 slug 校验失败):

```bash
cd ~/nasworkspace/AgentServe
ccteam init --slug agentserver         # A 落地后:就地初始化 cwd,注册名 = agentserver
```

## 5. 落地清单 + 验收(方案 A)

**改代码**(已落地)
- [x] `commands.rs` `resolve_install_target` 删分支 2(`--slug → projects_root` 搬家),并去掉不再用的 `paths` 参数 + 更新唯一 caller(`run_init`)。
- [x] `run_init` slug 派生:当 slug 是**派生自 basename**(无 `--slug`)且校验失败时,报错改为提示 `ccteam init --slug <lowercase-name>`(caps 目录如 `AgentServe` 不再 opaque fail)。
- [x] 同步文档/注释:`commands.rs` `InitOptions.{install_in,slug}` 字段 doc + `resolve_install_target` 优先级注释;`main.rs` `Init.--slug` arg doc + `New` 命令 doc(「Identical semantics」→「`init --in <projects_root>/<team>-<slug>`」)。

**测试审计(关键发现)**
- 测试模块里**每一个** `InitOptions` 都设了 `install_in: Some(...)`(`commands.rs` 全量 grep 已确认)
  —— **没有任何测试**走 `slug: Some + install_in: None` 这条搬家分支。
  → 结论 1:**删分支 2 不破坏任何现存测试**(也正因没测,bug 才漏出去)。
  → 结论 2:**必须新增回归测试**——`run_init` with `slug: Some("x"), install_in: None`,在受控 cwd 下断言
  `target == cwd` 且 `!projects_root.join("x").exists()`。
  (env-mutating / chdir 类放 `crates/ccteam-cli/tests/*.rs` integration,别放 lib `#[cfg(test)]`——CLAUDE.md §六。)

**验收 gate**
- [x] 新回归测试绿:`crates/ccteam-cli/tests/init_cwd_test.rs::init_with_slug_targets_cwd_not_projects_root` ——
  `init --slug agentserver`(无 `--in`,child cwd = 任意路径仓)→ `.ccteam/state.json` 落在 cwd,且 `<projects_root>/agentserver` **不存在**。
- [x] 现存 13 个 `run_init` 单测全绿(含 `run_init_rejects_invalid_slug_grammar` —— 报错措辞改动兼容)。
- [x] `cargo clippy --workspace --all-targets -- -D warnings` = 0;`cargo fmt --all -- --check` 干净。
- [x] `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1755 pass / 0 fail**(`WS_EXIT=0`;含新增 init_cwd 测试;本轮 `t09_memoize` env-flake 未触发)—— ≥ baseline,零失败。
- [ ] (留待手测)`ccteam new agentserver` 仍 → `~/projects/dev-agentserver`(team 前缀路径不回归;run_new 未改,逻辑上保持)。

## 6. 待 owner 拍板的次级决策

**`AgentServe` 含大写 → bare `ccteam init` 会报错**:`validate_slug_format`(`crates/ccteam-core/src/projects.rs:158-178`)
只收 `[a-z0-9-]+`,basename `AgentServe` 直接 fail。两个选择:

- **(a) 保持严格 + 好报错(推荐默认)**:bare `init` 在 caps 目录里报错并提示「目录名含大写,请用 `ccteam init --slug <小写名>`」。
  最少魔法、注册名显式可控。`--slug agentserver` 正是为此存在(也正好印证 A:`--slug` = 就地改名)。
- **(b) 自动 sanitize 派生 slug**:把 basename 自动转小写/替非法字符(`AgentServe` → `agentserve`)。更省事,但**静默改名**,
  用户可能不知道注册成了什么。

> 倾向 (a)。这条不阻塞 A 的主体,但 fix-plan 要把「正确重跑命令」写准,所以需 owner 选一个。

## 6.5 任意路径项目本来就支持(blast radius 仅限 `resolve_install_target`)

用户的仓在 `/home/rob/nasworkspace/AgentServe`(**不在** `~/projects/` 下)。这**完全没问题**:

- `paths.project_dir(slug)`(`crates/ccteam-core/src/paths.rs:117-124`)**先查 `config.yaml::projects[]`**,
  命中就返回登记的**绝对路径**;只有 slug **未注册**时才 fallback 到 `projects_root.join(slug)`。
- 已有测试 `f77_project_dir_consults_config_registry`(`paths.rs:496`)证明 `/vol4/.../dex-ui` 这类任意路径解析正确。
- 即 `~/projects/` 只是 `ccteam new` **新建**项目的默认落点,**绝非**已有项目必须待的地方;
  `start` / `ls` / `show` / session 解析全部按 slug → 注册绝对路径走。

→ 本 bug 的爆炸半径**仅限 `init` 的 `resolve_install_target` 一处**:只要 `init` 把正确的
`{slug, path=cwd}` 写进注册表,系统其余部分对任意路径早已工作。这也佐证「方案 A 功能零损失」。

**今天就能用的 workaround(无需等修复)**:`--in` 优先级高于 `--slug`,直接钉真实路径:

```bash
ccteam remove agentserver --purge && rmdir ~/projects/agentserver   # 先清掉之前的空骨架
ccteam init --in /home/rob/nasworkspace/AgentServe --slug agentserver
```

(修复方案 A 的意义,就是让用户**不必记 `--in`** 也能 `ccteam init --slug agentserver` 就地初始化。)

## 6.8 Shipped non-Rust surface 扫描(行为变更的真正爆炸半径)

改 CLI flag 语义,Rust gate 只证明 Rust 面;**真正风险在 shipped 非-Rust 自动化**——有没有脚本/skill/doc
跑 `ccteam init --slug X`(**不带** `--in`)却期望落到 `~/projects/X`。已逐一扫(`grep -rn ... skills/ install.sh docs/ scripts/`),结论 **clean**:

- **meta-agent bootstrap**:`render_install_meta_agent_report`(`commands.rs:4227`)→ `bootstrap_meta_project(paths)`(ccteam-core,显式路径),**不** shell `ccteam init --slug meta`。安全。
- **`docs/usage.md`(tier-1 用户手册)**:§2 一律「`cd <dir>` 后再 `ccteam init --slug <name>`」(72/77/278 行),本就 **cwd-based**;旧搬家行为反而与手册矛盾。新语义下产出路径不变,**无需改**。
- **`scripts/host-probe/run-probes.sh:374`**:`init --in $PROJ --slug ...` 显式 `--in`,不受影响。
- **`skills/`**:`ccteam-team` 用 `--mode agent-team`、`ccteam-scan` 用裸 `ccteam init`,均不依赖 `--slug` 搬家。
- **`install.sh`**:不跑 `ccteam init`。
- **版本归档 docs**(v0-4-2 / v0-4-3 / v0-5-0):冻结历史,描述旧行为,按 §五.3 留档不改。

→ 无 shipped 自动化依赖 `init --slug` 搬家;workspace 全绿已覆盖所有调 `run_init` 的 Rust 路径(含 meta/install 集成测试)。**ship-safe**。

## 7. 红线对齐

- ✅ 「新建项目走 `<projects_root>/<team>-<slug>/`」—— A 后**只剩 `run_new` 一条**产中心项目,前缀逻辑唯一化(2c 消失)。
- ✅ slug 冲突护栏(`commands.rs:168`)在 A 下原样工作,无需改动。
- ✅ `refuse_sensitive_install_target`(HOME/根目录拒装)对 cwd-default 早有兜底。
- ✅ pre-v1.0 不留 compat shim:直接改 `--slug` 语义,不留 alias。
