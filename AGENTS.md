# agora — Agent Instructions

本文件是 Claude Code 与 Codex **共用的唯一指令文件**；`CLAUDE.md` 是指向它的符号链接（Claude Code 只读 CLAUDE.md，Codex 只读 AGENTS.md）。仅 Claude 需要的规则放 `.claude/rules/`，不要改动 CLAUDE.md 本体。

## agora 项目约定

- 开工先读 `MISSION.md`（北极星、层次定位、施工规则）；设计决策先对照 `docs/adr/`（ADR-001 运行时、ADR-002 状态来源、ADR-003 认证、ADR-004 拓扑），devcenter 报告（`docs/analysis/devcenter/`）只是 ADR 的引文与历史分析。
- 规划分层的地图见 MISSION 文首"文档分层"（唯一出处）。操作规则：`ROADMAP.md` 由 `scripts/roadmap-view.sh` 生成，不手改、不放 checkbox；`docs/spec/` 随代码改，与代码冲突时以代码为准并回写 spec；文档改完跑 `scripts/doc-lint.sh`（§引用、epic 的 A 编号、相对链接与仓内路径；CI 也跑）；beads 里阶段 = epic、阶段门 = blocks、验收 = `--acceptance`、演示剧本 = epic 的 `--design`（ROADMAP 视图同步展示）。
- 任务纪律（本文件是唯一出处；"一个 issue 做完"的定义见 MISSION §1.5）：`bd update <id> --claim` 后才动手；commit subject 末尾带 `(agora-xxxx)`；干活中发现的问题 `bd create ... --deps discovered-from:<id>` 另立；任务超出一个会话能承载的范围就 `bd create --parent=<id>` 拆子任务并在 notes 写交接，提交保持小切片、提交信息写测试数与本次打开的守卫；关闭：按 MISSION §1.5 三档——机械（CI 绿 + 验收点名的守卫测试通过）与代检（agent 按演示剧本用 agent-browser / tmux capture-pane / curl / ssh 操作并断言到与人眼相同的事实）都由实施 agent `bd close --reason` 关闭并写明证据（测试名、commit；代检加命令、观察到的输出与工具版本），代检前先 `bd memories agent-browser` 看已知假阳性坑，代检脚本自身的失败先怀疑工具再怀疑 agora；只有 §1.5 点名的人眼条目（剧本里标 👁）与 epic 由人关闭，验收里写「人眼」必须点名条目并说明为什么代检不等效；commit message 用叙事句说"为什么"而不只是"改了什么"；非显然的决定（绕坑、反直觉写法）在注释里带实测日期与反例，防后来者好心改回去。
- 并行施工（多个 agent 同时开工时；第一波编排见各任务 notes 的「文件归属 / 接缝」）：一任务一 worktree（`git worktree add ../agora-wt/<id> -b <id> main`，与 `worktree_root` 约定一致；beads 库经 git common dir 自动共享，worktree 里直接用 `bd`），提交前 `git rebase main`，切片小到一次 rebase 解决得了；用 Claude Code 的 Workflow / Agent worktree 隔离起 agent 之前，编排者先把本地仓库推干净——`git status --porcelain` 为空、`git log origin/main..main` 为空，push 要先向用户要授权——因为隔离 worktree 基于 origin/main 而不是本地 HEAD（2026-09-06 第一批实测：三个 agent 都差了 3 个未 push 的提交开工），否则 agent 从历史代码起步、集成时才发现；agent 自己开工第一步仍要 `git merge --ff-only main` 并核对 HEAD == main，新 worktree 里先 `npm --prefix web run build` 再 cargo（rust-embed 编译期读 web/dist，gitignore 只留 .gitkeep），编译缓存用 `cp -Rc` 从主仓克隆 target 与 node_modules，但克隆前先在主仓 `cargo sweep -t 1`（没装先 `cargo install cargo-sweep`）——`cp -Rc` 走 clonefile 不复制数据块，慢的是 per-file 系统调用，耗时只跟文件数走而与体积无关：主仓 target 六天不清会攒到 48.5 万文件（97% 是 cargo 永不回收的历史 hash 副本，同一个 crate 能堆 141 份 agora、87 份 libagora），克隆要几分钟；清完只剩 1.6 万文件，克隆 2.3 秒、node_modules 另 0.4 秒（2026-09-09 实测：485481 文件 / 20G 对 16192 文件 / 3.1G）。仍然克隆而不是让 worktree 从零编译，是因为从零 `cargo test --no-run` 要 25 秒、是克隆的十倍；想让克隆过去的 target 自带测试二进制，主仓 sweep 之后先跑一次 `cargo test --no-run` 再建 worktree。反过来 worktree 里别再跑 `cargo sweep -t`：`cp -Rc` 会连 mtime 一起克隆（普通 `cp` 才刷新时间戳），刚拷过去、正要复用的缓存在 sweep 眼里就是陈旧产物，会被精准删掉、把省下的编译时间原样赔回去（2026-09-09 实测：克隆后 build 命中缓存，sweep 仍删光依赖产物，再 build 全部重编），非跑不可就先补 `find target -exec touch {} +`；手工起 daemon 用独立 `AGORA_HOME`、`server.listen` 端口与 `runtime.tmux.socket`（并把 `adopt_sockets` 设成 `[]`）——socket 沿用默认的 `agora` 会和开发机上真的 daemon 共用一个 tmux server、把它的会话列成 unregistered（2026-09-06 第七批 agora-oir 踩过，幸而只读），别碰开发机上真的 agora；热点文件规矩：`docs/spec/api.md` 只在自己端点的小节里追加，侧栏行 / Header / New Agent 对话框谁先碰谁先把那块抽成独立组件（抽取单独一个 commit、不改行为、立刻 push），后来者只改新组件；要改别人归属的文件先看对方分支有没有未合入的改动。集成者每合入一批就重跑 `scripts/roadmap-view.sh` 重生成 ROADMAP.md 并随本批一起提交——beads 里关了、视图没更新，看 ROADMAP 的人会以为任务还开着（2026-09-06 第一批踩过）。排波次时默认假设 `.claude/handoff.json` 里所有 `when: "src"` 的门禁都会跑，除非该 issue 明确只改 `.md`：这个值的语义是「有非 `.md` 改动就跑」、与目录无关，纯 `web/` 的前端改动照样触发 `cargo test`——别按门禁的名字或枚举值的字面意思推断它跑不跑（2026-09-10 排 M4b 波次时把它读成「改了 `src/` 才跑」，据此以为前端任务能躲开 agora-eny 那堆偶发红，交叉验证时发现、波次未开跑；四处出处与实测见 `bd memories handoff-gate-when-src-is-any-non-md`）。
- 用 handoff plugin（~/code/handoff，安装见其 README）跑 issue 时读 `.claude/handoff.json`（门禁、准备命令、合并后动作、worktree 约定都在那里，与本文件口径一致）；执行者标签约定 `worker:claude|grok|codex`，没标签时用配置里的 default_worker。
- 同步授权：本仓库**明确授权** agent 在会话结束时执行 `bd dolt push`（仅同步 beads 数据，覆盖下方 Beads 块的 Conservative 默认）；`git commit` / `git push` 仍需用户当次明确授权。
- ADR 约定见 `docs/adr/README.md`，模板 `docs/adr/TEMPLATE.md`；被否决的 ADR 保留全文。
- 依赖方向：`bd dep add <被阻塞> <阻塞者>`；`bd create --deps blocks:X` 表示"新 issue 阻塞 X"，要表达"被 X 阻塞"请建完后用 `bd dep add`。
- 记忆：`bd remember` 只存本文件与 MISSION 没有的、干活中学到的项目事实（排障经验、环境怪癖）；Claude Code 的用户级 auto-memory 只放用户偏好，不放项目事实。
- 语言：面向用户的输出一律中文；代码、命令、路径原样。

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
