# agora

多 Agent 管理工具（设计阶段）。一句话定义与层次定位见 `MISSION.md`（定稿中）。

## 从哪里读起

| 文件 | 作用 | 谁是真相源 |
|---|---|---|
| `MISSION.md` | 北极星、层次定位（L1/L2/L3）、目标场景、不变量、施工规则、验收清单 | 是（markdown） |
| `docs/adr/` | 架构决策记录；约定与索引见 `docs/adr/README.md` | 是（markdown） |
| `.beads/` | 路线图（epic）与任务；`bd ready` 看现在能做什么 | 是（Dolt，经 `refs/dolt/data` 同步） |
| `ROADMAP.md` | 由 `scripts/roadmap-view.sh` 从 beads 生成的阶段视图 | 否（视图） |
| `AGENTS.md` | Claude Code / Codex 共用的 agent 指令；`CLAUDE.md` 是它的符号链接 | 是 |
| `docs/analysis/devcenter/` | 同类项目 devcenter 的深度评审与借鉴范围判断（2026-09-01） | 参考 |
| `docs/analysis/beads/` | 采用 beads 作为开发方法的可行性、必要性、替代品横评与落地形态 | 参考 |

## 开发方法

任务跟踪用 [beads](https://github.com/gastownhall/beads)（`brew install beads`）。新克隆：`bd bootstrap`；日常：`bd ready` → `bd update <id> --claim` → 干活 → `bd close <id> --reason="..."` → `bd dolt push`。规则见 `AGENTS.md` 与 `MISSION.md` §6。
