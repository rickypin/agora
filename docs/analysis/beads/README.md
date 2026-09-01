# 在 agora 引入 beads 作为开发方法：可行性评估与基本模式

> 评估日期：2026-09-01。对象：beads（`bd`）v1.2.2，https://github.com/gastownhall/beads ，MIT。
> 参照实例：`~/code/devcenter`（同类型项目，已用 beads 施工 16 天 / 154 issues）。

## 0. 结论

**可行，建议采用；但只用"最小配置"。**

- 采用理由：agora 的施工方式与 devcenter 相同（单人 + Claude Code / Codex 施工），beads 正是为这种"agent 跨会话失忆"场景设计的：任务图存在仓库旁边，`bd prime` 在每个会话开头把规则 + 就绪任务 + 项目记忆塞回上下文（约 1–2k tokens，不走 MCP）。
- 最小配置 = embedded Dolt（默认）+ `agora` 前缀 + Claude/Codex hooks + 在 MISSION 施工规则里写死使用纪律。**不碰** server 模式、formulas/molecules/gates/wisps、Notion/GitHub/Jira 同步、federation。
- 最大的风险不是技术，而是"装了不用"：devcenter 的实证显示 agent 会把 beads 退化成"计划分解 + 完工登记"，claim/commit 引用/关闭的闭环基本没跑起来（见 §2.4）。纪律要靠 MISSION 与 hook 注入的规则约束，而不是靠工具本身。
- 必要性（§6）：与并行 agent 会话数强相关。单会话串行时非必要（markdown 足够）；agora 习惯多会话并行、量级 150+ 任务、且产品本身是多 agent 管理工具 → 落在"必要"侧，触发点在实现阶段而非 M0。
- 替代品与规划层（§7–§8）：按 agora 权重无更优替代品，次选仅 Backlog.md；规划层拆三层——约束层（MISSION）与 ADR 留 markdown，路线图进 beads（阶段 = epic、阶段门 = blocks、验收 = --acceptance），ROADMAP.md 退化为薄视图。

## 1. beads 是什么（三个角色 + 一条纪律）

| 组件 | 角色 | 说明 |
|---|---|---|
| `.beads/embeddeddolt/` | **唯一真相** | 嵌入式 Dolt（版本化 SQL），所有 `bd` 读写直接落这里，embedded 模式每次写自动 commit；被 `.gitignore` 排除，不进源码分支 |
| `refs/dolt/data` | **跨机同步线路** | `bd dolt push / pull` 把 Dolt 历史推到 git remote 的独立 ref 上，与 `refs/heads/main` 互不干扰；新克隆用 `bd bootstrap` 取回 |
| `.beads/issues.jsonl` | **被动导出视图** | 给人看、给 diff、做备份/迁移用；**不是**同步通道 |

纪律：`bd` 是 CLI，agent 通过 hooks + CLI 使用；不用 markdown TODO 做真相源；不用 `bd import issues.jsonl` 代替 `bd dolt pull`；不用 `bd edit`（交互式）；一律加 `--json`。

## 2. 可行性检查表

### 2.1 环境与安装

| 项 | 现状 | 判断 |
|---|---|---|
| 安装 | 评估时本机未装 `bd` / `dolt` / `bv`（当日已装，见 §5）；`brew install beads` 会连带 `dolt` + `icu4c@78` | 可行。`bd` 自带嵌入 Dolt 引擎，独立 `dolt` CLI 仅 server 模式或直接查 SQL 时需要 |
| 仓库 | `origin = git@github.com:rickypin/agora.git` | `bd init` 自动把 git origin 配成 Dolt remote；`bd dolt push` 推 `refs/dolt/data`。devcenter 在 GitLab 上已验证该 ref 可落地（`git ls-remote` 可见 `refs/dolt/data` 与 `refs/heads/__dolt_remote_info__`） |
| 与现有文件冲突 | 仓库中没有 CLAUDE.md / AGENTS.md / `.claude` / `.codex` / `.beads` | 无冲突，`bd init` 全部新建 |
| Claude Code | `bd setup claude` 只写 `.claude/settings.json` 的 `SessionStart` hook（`bd prime --hook-json`）+ CLAUDE.md 一段托管块（实测约 55 行，非"一小段"）；compaction 后 SessionStart 会再次触发，无需 PreCompact | 与 devcenter 的 `.claude/settings.json` 完全一致 |
| Codex | `bd setup codex` 写 `.agents/skills/beads/`、AGENTS.md 段落、`.codex/hooks.json`（SessionStart / PreCompact / PostCompact / UserPromptSubmit → `bd codex-hook <Event>`）+ `.codex/config.toml` `[features] hooks = true` | 两套集成可同时装（devcenter 就是同时装的） |
| git worktree | 同仓库多个 worktree 默认共享同一个 `.beads`（可用 `BEADS_DIR` 覆盖） | 多个 agent 会话并行在不同 worktree 上工作时共用一张任务图 —— 恰好是 agora 想要的 |

### 2.2 规模与并发

- embedded 模式 = **单写者 + 文件锁**，零运维。单人 + 若干个 agent 会话足够：`bd` 的写操作都是毫秒级短事务，串行化代价可忽略。
- 只有当多个 agent 需要**同时长时间持有写连接**（orchestrator 部署）才需要 `bd init --server` + `dolt sql-server`。agora 现阶段不需要；真遇到锁冲突再迁移，迁移路径官方支持。
- 待实测：两个 Claude 会话在两个 worktree 里同时 `bd update --claim` 时，embedded 文件锁是排队还是报错。

### 2.3 风险与成本

| 风险 | 严重度 | 应对 |
|---|---|---|
| 项目迭代快（v1.2.2；hook 脚本带 `BEGIN BEADS INTEGRATION v1.1.2` 版本标记）→ 升级后需 `bd hooks install` 刷新、可能有 schema 迁移 | 中 | 锁 brew 版本，升级当作一条 chore issue 处理 |
| Dolt 版本回归：官方 pin 在 2.2.0，2.3.0+ 的 `DOLT_RESET('--hard')` 有 ~5% 间歇失败，影响 `bd flatten` / `bd admin compact` | 低 | embedded 模式用的是 `bd` 内嵌的 Go 库版本，不受 brew `dolt` CLI 版本影响；不主动跑 flatten/compact |
| 概念面大（formula / molecule / gate / wisp / federation / notion / gc / flatten…） | 中 | 只用 §3 那 10 条命令，其余在 MISSION 里标为 Non-Goal |
| `.beads/` 下 config.yaml / metadata.json / hooks 进 git；`embeddeddolt/` 不进；忘记 `bd dolt push` = 任务数据只在本机 | 中 | 会话收尾协议强制 `bd dolt push`（`bd prime` 注入的规则里有） |
| 退出成本 | 低 | `bd export -o issues.jsonl` → 删 `.beads/`、hooks（`bd hooks list`）、`bd setup claude --remove` / codex 同理。MIT，无锁定 |

### 2.4 devcenter 的实证（同类项目真实用法）

- 154 个 issue **全部 closed**；状态流转里只有 **12 次** `in_progress`；146 个 commit 里只有 **3 个** subject 引用了 issue id。
- 分层 epic 用得很好（`devcenter-126.1 … .14`），说明"把 ROADMAP 阶段拆成 epic → 子任务"是自然的。
- 结论：他们实际把 beads 当作**计划分解器 + 完工台账**，而不是 claim → commit(id) → close 的细粒度跟踪。这不是工具问题，是纪律没写进施工规则。agora 若想要可追溯性（commit ↔ issue ↔ ADR），必须在 MISSION 的 Coding-Agent 规则里明确要求，并在 code review 时检查。

## 3. 基本模式（一个 agent 的一天）

### 3.1 一次性初始化（已于 2026-09-01 执行，结果见 §5）

```bash
brew install beads                 # 连带 dolt、icu4c@78
cd ~/code/agora
bd init --prefix agora             # 建 .beads/、嵌入式 Dolt、git hooks；默认顺带 bd setup claude/codex
bd setup claude --check            # 验证 .claude/settings.json 的 SessionStart hook
bd setup codex                     # 如需 Codex
git add .beads .claude .codex .agents AGENTS.md CLAUDE.md && git commit -m "Adopt beads for issue tracking"
bd dolt push                       # 首次发布 refs/dolt/data
```

### 3.2 规划：epic → 子任务 → 依赖

```bash
bd create "M0: MISSION 与首批 ADR" -t epic -p 1 --description="..."     # → agora-xxxx
bd create "写 MISSION.md" --parent agora-xxxx -t task -p 1 --description="..."   # → agora-xxxx.1
bd create "ADR-001 运行时选型" --parent agora-xxxx -t task -p 1 --description="..."  # → agora-xxxx.2
bd dep add agora-xxxx.2 agora-xxxx.1      # ADR-001 依赖 MISSION 先完成（blocks）
bd dep tree agora-xxxx
bd ready                                  # 只列"无阻塞、未领取"的任务；bd list --status=open 列全部
```

依赖类型：`blocks`（硬顺序，影响 ready）、`parent-child`（epic 结构）、`discovered-from`（干活时发现的新问题，记录出处，不阻塞）、`related`（软关联）。

### 3.3 会话开始（自动）

Claude Code 的 SessionStart hook 跑 `bd prime --hook-json`：注入工作流规则、`bd ready` 列表、`bd remember` 存的项目记忆、进行中的任务。compaction 之后再触发一次，所以 agent 不会"忘了 bd 怎么用"。可用 `.beads/PRIME.md` 覆盖默认输出。

### 3.4 干活循环

```bash
bd ready --json                          # 1. 找可做的
bd show agora-xxxx.1 --json              # 2. 看清楚
bd update agora-xxxx.1 --claim --json    # 3. 领取（open → in_progress）
# ... 写代码 ...
git commit -m "Write MISSION.md (agora-xxxx.1)"      # 4. commit 引用 issue id
bd create "发现：xxx 需要单独处理" -t task -p 2 --description="..." \
   --deps discovered-from:agora-xxxx.1               # 5. 干活中发现的新问题登记出处
bd close agora-xxxx.1 --reason="Completed: ..." --json   # 6. 关闭（agent 不得替人自动关闭未验证的任务）
```

### 3.5 项目记忆

```bash
bd remember "本机 tmux 3.5 的 set-clipboard 在 iTerm 下需要手动开" --key tmux-clipboard   # 只存指令文件里没有的事实
bd memories / bd recall <key> / bd forget <key>
```
适合存：干活中学到的排障经验、环境怪癖。不适合：已写在 AGENTS.md / MISSION 里的规则（那些本来就每会话注入，再存一遍是双份）、一次性信息、凭证。存进去的内容会被每次 `bd prime` 注入。

### 3.6 会话收尾（"落地"协议）

```bash
bd list --status=in_progress --json      # 没做完的写进 description 或 comment，不留在脑子里
bd dolt push                             # 必做：否则任务数据只在本机
git push
```

### 3.7 多机 / 新克隆

```bash
git clone ... && cd agora && bd bootstrap    # 取回 refs/dolt/data
bd dolt pull                                 # 日常拉取
```

## 4. 对 agora 的采用建议

1. **现在就用最小配置**：embedded + `agora` 前缀 + Claude/Codex hooks。第一批 issue 直接来自 devcenter 报告 §6.6 的"下一步"：MISSION.md、ADR-001 运行时选型、ADR-002 状态源分层、ADR-003 节点认证、测试骨架 + CI。
2. **映射关系**：MISSION 的 phase / ROADMAP 阶段门 → `epic`；每个 ADR → `task`；bug → `bug`；升级 beads / 刷新 hooks → `chore`。
3. **把纪律写进 MISSION 的 Coding-Agent 规则**（吸取 devcenter 教训）：
   - 开工必 `--claim`；commit subject 引用 `(agora-xxxx)`；
   - 干活中发现的问题用 `--deps discovered-from` 登记，不塞进当前 issue；
   - agent 不自动关闭需要人验证的任务；
   - 会话结束必 `bd dolt push`。
4. **明确不用**（写进 Non-Goals）：server 模式、formulas/molecules/gates/wisps、外部 tracker 同步、federation。
5. **一个额外收益**：agora 自身要做的 L2"任务语义层"（任务 / 依赖 / 就绪 / 领取），beads 的数据模型（hash id、`ready` 定义、四种依赖、`discovered-from` 溯源）是一个现成的、经过 26.8k star 项目验证的参考实现；后续设计 agora 的任务模型时可直接对照，甚至把 `bd` 作为 agora 的任务来源集成点之一。

## 5. 装后实测结果（2026-09-01，bd 1.2.2 / brew dolt 2.3.1）

- [x] **并发领取**：两个后台进程同时 `bd update --claim` 不同 issue → 都成功；两个 actor 同时抢同一 issue → 后者报 `issue already claimed by sessionA`、exit 1。embedded 文件锁 + 原子 claim 成立。
- [x] **`bd remember` 存储位置**：存在 Dolt 库里（`bd memories` 可列；`.beads/embeddeddolt/` 2.2 MB），随 `bd dolt push` 同步；写入后 `.beads/backup/` 自动生成一份本地备份（gitignored）。
- [x] **CLAUDE.md / AGENTS.md**：最终做法是 **`AGENTS.md` 唯一 + `CLAUDE.md -> AGENTS.md` 符号链接**。依据：Claude Code 只读 CLAUDE.md（支持 `@import` 与 symlink），Codex 只读 AGENTS.md（`project_doc_fallback_filenames` 仅在其缺失时生效，且不支持导入），bd 对符号链接的 CLAUDE.md 不写入，因此 bd 升级不会再造出第二份。bd 托管块（`<!-- BEGIN/END BEADS ... -->`）原样保留，agora 段放在最前。生成块默认 **Conservative profile**（不主动 commit / push / dolt sync）与"会话末 `bd dolt push`"矛盾，已在 agora 段显式授权 dolt push、保留 git 授权要求。
- [x] **`bd remember` 与指令文件的重复**：AGENTS.md 与 memories 都是每会话注入，初期存的 4 条规则与 AGENTS.md 完全重复，已删除；策略改为"只存指令文件里没有的事实"。
- [x] **pre-commit hook 耗时**：0.07 s（auto-export 默认关闭）。
- [ ] `bd create --file` 的 markdown 格式仍未实测（M1 拆分时再验）。

踩坑记录：
- `brew install beads` 会先跑 `brew update --auto-update`，本机卡在一个无关 tap 的 git fetch 上 12 分钟；用 `HOMEBREW_NO_AUTO_UPDATE=1` 重装 1 分钟完成。
- brew 装的 dolt 是 **2.3.1**（官方 pin 2.2.0）；embedded 模式用 bd 内嵌引擎，不受影响，但不要主动跑 `bd flatten` / `bd admin compact`。
- **依赖方向**：`bd create --deps blocks:X` 表示"新 issue 阻塞 X"；要表达"被 X 阻塞"，建完后 `bd dep add <被阻塞> <阻塞者>`。第一次建反了，用 `bd dep remove` 修正（zsh 下不要用 `set -- $pair` 拆参数）。
- `bd init` 在非交互模式下会**自动 git commit** 它生成的文件（`.beads/`、`.claude/`、`.codex/`、`AGENTS.md`、`CLAUDE.md`、`.gitignore`），提交信息 `bd init: initialize beads issue tracking`。

## 6. 必要性评估（2026-09-01 补充）

§0–§5 回答的是"能不能用"；本节回答"是否非用不可"。

### 6.1 它解决 agora 的哪些问题，这些问题是否真实存在

| # | 问题 | 对 agora 是否真实 | markdown 能否解决 | beads 独有？ |
|---|---|---|---|---|
| P1 | 新会话 / compaction 后失忆："下一步做什么、做到哪了" | 真实，Claude Code 每天发生 | 能解决一半：ROADMAP.md + SessionStart hook `cat` 它；但文件越长注入成本越高，beads 的 `bd prime` 恒定 1–2k tokens | 否 |
| P2 | 多个并行 agent 会话 / worktree 抢同一任务、改同一清单文件冲突 | 真实：用户日常多会话并行（全局 CLAUDE.md 里 agent-browser 的 `--session` 隔离规则就是这类问题的产物） | 不能：markdown 没有原子 claim，多 worktree 改同一文件必冲突 | **是**（hash id + Dolt cell 级 merge + `--claim`） |
| P3 | 依赖关系与"现在能做什么"查询 | 前期弱（M0 只有 5–10 个任务），实现期跨模块时变强 | 勉强：靠人脑；> 50 个任务后失效 | 是（`bd ready`、四种依赖） |
| P4 | commit ↔ issue ↔ ADR 可追溯 | 弱需求；devcenter 实际也没做到（3/146） | 同样靠纪律 | 否（是纪律问题） |
| P5 | agora 自己要做 L2 任务语义层，需要一个"给 agent 用的任务图"的参考实现 | 战略性、agora 独有 | 否 | 是（dogfooding） |

### 6.2 三个方案对比

| 维度 | A. markdown + git（ROADMAP/TASKS.md + hook） | B. GitHub Issues via `gh` | C. beads 最小配置 |
|---|---|---|---|
| 引入成本 | 0 | 0（已有 GitHub origin） | 30 分钟 + brew 依赖 |
| 并行会话领取 | 无 | 有（assign / label），非原子但够用 | 原子 `--claim` |
| 依赖 / ready | 无 | sub-issues + blocked-by（近一年补上），无 ready 查询 | 完整 |
| 会话恢复成本 | 随文件增长 | 每次 `gh issue list` JSON，几 k tokens，需网络 | 恒定 1–2k，离线 |
| 人类可见性 | 仓库内 | 最好（网页、PR 自动关闭） | 弱（`bd list` / JSONL） |
| 多 worktree 冲突 | 会 | 不会 | 不会 |
| 退出成本 | — | 低 | 低（`bd export` + 删目录） |
| 与 agora L2 的关系 | 无 | 无 | 参考实现 / 未来集成点 |

### 6.3 devcenter 的证据：双轨与浅用

- MISSION.md 20 个验收 checkbox、ROADMAP.md 33 个任务 checkbox，**0 处引用 bd id**；beads 里另有 154 个 issue。→ beads 没有替代规划层，而是与 markdown 双轨维护。
- `git worktree list` 只有 main；即单会话串行施工 → P2 对 devcenter 不存在，beads 的独有价值没被用到；剩下的 P1/P3 价值 markdown 也能给一大半。这解释了 12 次 in_progress、3 个 commit 引用的浅用现象。
- 推论：**beads 的必要性与并行会话数强相关**。单会话串行 → 非必要；多会话并行 → 必要（否则要自造等价物）。

### 6.4 判定

- **不必要**的情形：始终单会话串行；总任务 < 30；或打算用 GitHub Issues 做对外可见的 backlog（此时方案 B + 一个 SessionStart hook 即可）。
- **必要**的情形：≥ 2 个并行 agent 会话 / worktree；任务 > 50 且跨模块有依赖；希望 agent 自主选任务而不是人喂任务。
- agora：用户习惯多会话并行、项目量级与 devcenter 同级（150+ 任务）、且产品本身就是多 agent 管理工具 → 落在"必要"侧，但**触发点在实现阶段，不在 M0**。

### 6.5 建议

1. 引入，定位为**任务执行层**；规划层仍是 MISSION/ROADMAP（markdown）。为避免 devcenter 的双轨，ROADMAP 里不再放任务 checkbox，只放阶段目标 + 验收标准 + 对应 epic id。
2. 时机：MISSION 定稿后、第一个实现 epic 开工前；M0 的 ADR 任务用来练手。切换成本此刻最低，且 MISSION 的施工规则要引用 bd。
3. 4 周后复盘两个指标：commit 引用 issue id 的比例 ≥ 70%；in_progress 转换数 ≈ closed 数。达不到说明纪律没落地——修纪律或退出，退出成本很低。
4. 若暂不引入：方案 A（ROADMAP.md + `docs/TASKS.md` + SessionStart hook），并把"第一次出现并行会话"定为迁移触发点，迁移只是批量 `bd create`。

## 7. 替代品横评（2026-09-01 互联网调研）

评判维度按 agora 的需求排序：a) 仓库内驻留 + git 同步；b) 并行会话 / worktree 的原子领取；c) 依赖图 + ready 查询；d) 会话开头恒定成本注入；e) Claude Code **与** Codex 都能用；f) 人类可读、可 PR 评审；g) 成熟度；h) 退出成本。

| 工具 | 形态 | a | b | c | d | e | f | g | 结论 |
|---|---|---|---|---|---|---|---|---|---|
| **beads** (bd) | Dolt 图数据库 + CLI + hooks | ✓ | ✓ hash id + `--claim` | ✓ 四种依赖 + `bd ready` | ✓ `bd prime` 1–2k | ✓ 两者官方 setup | ✗ 只有 JSONL 导出 | 26.8k★，v1.2.2，迭代快 | **基线** |
| **Claude Code 原生 Tasks** | `TaskCreate/…` 工具，`~/.claude/tasks/<id>/` | ✗ 不进仓库（#20487 开放请求） | ✗ 无 claim 语义 | 部分：`blocks/blockedBy`，无层级 / 优先级 | ✗ 不自动注入；`/clear` 清空（#41667） | ✗ 仅 Claude Code；**Fable 5 / Sonnet 5 / Opus 4.8 上默认不暴露**（需 `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`，#80015） | ✗ | 官方，但多处未文档化 | 会话内草稿可用，**不是项目级替代品** |
| **Backlog.md** (MrLesk) | `backlog/tasks/*.md` + CLI + MCP + Web/TUI 看板 | ✓ | ✗ 顺序 id、无原子领取、并发编辑靠 git merge | 部分：dependencies / parent，无 ready 语义 | ✗ 靠 `backlog instructions` 或 MCP（MCP 成本高） | ✓ | **✓ 最强**：frontmatter + acceptance criteria + plan 全在 md | 6.6k★，MIT，活跃 | **唯一可信的次选**：若更看重人类可读与 PR 评审 |
| **Task Master** (eyaltoledano) | PRD → `tasks.json`，MCP 为主（7–36 个工具） | ✓ 单文件 | ✗ 单 JSON 文件并行必冲突 | ✓ 依赖 + 复杂度 + next | ✗ MCP 重 | 偏 Cursor / Claude Code | 部分 | 25k★ | 强项是 PRD 拆任务，不适合多会话 |
| **Flux** (sirsjg) | Kanban 服务 + MCP + CLI（`flux ready`） | ✗ 需常驻服务 | 部分 | ✓ | ✗ | ✓ | ✓ Web | 小众 | 需要仪表盘的团队场景 |
| **git-bug** | issue 存为 git 对象，CLI/TUI/Web，桥接 GitHub/GitLab | ✓ | ✗ | ✗ 无依赖 / epic / 优先级 | ✗ | ✗ 无 agent 集成 | 部分 | 10k★，GPLv3 | 通用分布式 tracker，无 agent 语义 |
| **GitHub Issues** via `gh` | 远端服务 | ✗ 不在仓库、需网络 | 部分（assign） | 部分：sub-issues + blocked-by，无 ready | ✗ 每次 `gh issue list` JSON | ✓ | **✓** 网页 + PR 自动关闭 | 最成熟 | 对外可见 backlog 的正确选择；agent 工效差 |
| Spec Kit / OpenSpec / BMAD / GSD | 规划框架：spec → plan → `tasks.md` 清单 | ✓ | ✗ | ✗ 清单无依赖图 | ✗ 需整份 spec 入上下文 | 多数支持 | ✓ | 6–12 万★ | **不是 tracker**，是规划层工具（见 §8） |
| Vibe Kanban / Conductor / Claude Squad / Nimbalyst / Superset / Mux | agent 编排工作台（worktree 隔离 + 看板） | — | — | — | — | — | — | Vibe Kanban 母公司 2026-04 关闭转社区 | **是 agora 的竞品，不是 beads 的替代品**（另见 devcenter 报告） |

判断：**按 agora 的权重（b、e 是硬需求）没有比 beads 更优的替代品。** 能与之竞争的只有 Backlog.md，而它输在的恰好是 P2（并行领取、多 worktree 合并）。Claude Code 原生 Tasks 在当前模型上连默认开启都没有，且 Codex 不可用；其余要么是规划框架、要么是编排工作台，层次不同。社区实践（paddo.dev、Better Stack）也倾向组合：会话内草稿用原生 Tasks，项目级记忆用 beads，规划用 spec 文档。

## 8. 规划层能否放进 beads

### 8.1 beads 承载"规划"的能力

- issue 字段：`--description`、`--design` / `--design-file`、`--acceptance`（验收标准）、`--notes`、`--context`、`--body-file`（从文件 / stdin 读正文）；类型含 `epic` 与 `decision`；`--validate` / `bd lint` 按类型检查必要段落；`--file` 从 markdown 批量建 issue、`--graph` 从 JSON 计划建带依赖的图；`--defer` / `--due` / `--estimate`；`bd epic` 子命令；`bd remember` 存长期规则；`.beads/PRIME.md` 覆盖会话注入内容。
- Yegge 的立场：beads "replaces markdown files in your plans/ directory"，理由是 agent "TERRIBLE at managing Markdown plans"（不可查询、易腐烂、解析成本高）；但在 Gas Town 文档里又说可以先用 Spec Kit / BMAD 做计划，"once your plan is ready, ask an agent to convert it into Beads epics"；对个人用户的建议是 "Make the work durable, not the agent"。
- 第三方共识（Better Stack）："用规范做详细计划，用 beads 执行"。

### 8.2 把"规划层"拆成三层再判断

| 子层 | 内容 | 放 beads？ | 理由 |
|---|---|---|---|
| **约束层** | MISSION：北极星、Non-Goals、不变量、DoD、施工规则 | **否**（源仍是 `MISSION.md`） | 每个会话要整份读、需要 PR diff 评审、要在 GitHub 上可见；issue 的字段形态和 CLI 编辑方式都不适合长篇约束。可把其中的施工规则镜像进 `bd remember` / `PRIME.md`，但源头是 markdown |
| **决策层** | ADR | **记录不放，任务放** | ADR 是文档：要永久可链接、被代码注释引用、PR 评审。beads 里只建一个 `-t decision` 的工作项（"做出 ADR-003 决策"），用 `--external-ref` 指向 `docs/adr/ADR-003.md` |
| **路线图层** | ROADMAP：阶段、工作包、阶段门（验收）、里程碑 | **是** | 这层本质是"带依赖的工作"：阶段 = epic，工作包 = 子 epic / task，阶段门 = 阶段 epic 之间的 `blocks` 或 `--waits-for-gate all-children`，验收标准写进 epic 的 `--acceptance`。`bd ready` 只有拿到这张图才能算"现在能做什么" |

### 8.3 必要性与合理性判断

- 路线图进 beads 的必要性是**条件性的**：不用 beads 时 ROADMAP.md 完全够；一旦用 beads 做执行层，路线图留在 markdown 就会出现 devcenter 的双轨（33 个 checkbox + 154 个 issue、0 处互引）。二选一：要么路线图进 beads（单一真相），要么 beads 只记"干活时发现的问题"和 bug。前者合理，因为 epic / 依赖 / 验收字段就是为它设计的。
- 约束层与 ADR 进 beads 是**不必要且不合理**的：收益（可查询）用不上，代价（不可 PR 评审、GitHub 不可见、CLI 编辑长文）真实。
- 代价与缓解：
  - 路线图在 GitHub 上不可见 → 保留一份薄的 `ROADMAP.md`：只写阶段目标一句话 + 对应 epic id + 验收要点，或者用脚本从 `bd list -t epic --json` 生成；
  - 长篇验收 / 设计文字用 CLI 写很别扭 → 先写 markdown 草稿，用 `--body-file` / `--design-file` 灌入；`bd create --file` 的 markdown 格式文档未写明，装后实测；
  - agent 容易造出臃肿 epic → `bd lint` + `--validate`，并在 MISSION 施工规则里限定 epic 只由人建或人审。
- 不建议现在引入 Spec Kit / OpenSpec 之类规划框架：MISSION + ADR + epic 三件套已覆盖 proposal → design → tasks 的链路；OpenSpec 的 `changes/<name>/{proposal,design,tasks}.md → archive` 模式可在后期大特性时借鉴。

### 8.4 落地形态

```
MISSION.md            ← 约束层（markdown，PR 评审）
docs/adr/ADR-NNN.md   ← 决策记录（markdown）
.beads/               ← 路线图 + 任务（epic / task / bug / chore / decision-任务）
ROADMAP.md            ← 视图：scripts/roadmap-view.sh 从 epic 生成
AGENTS.md             ← agent 指令唯一文件；CLAUDE.md 是符号链接
bd remember           ← 只存指令文件里没有的、干活中学到的事实
```
