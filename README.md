# agora

多 Agent 管理工具（设计阶段）。一句话定义与层次定位见 `MISSION.md`（定稿中）。

## 从哪里读起

分层规则（什么内容归哪个文件、谁是真相源）见 `MISSION.md` 文首"文档分层"；下表只是导航。

| 文件 | 作用 |
|---|---|
| `MISSION.md` | 北极星、层次定位（L1/L2/L3）、目标场景、不变量、施工规则、验收清单 |
| `docs/adr/` | 架构决策记录（"为什么"）；约定与索引见 `docs/adr/README.md` |
| `docs/spec/` | 实现形状：端点、配置、schema、线框、实测清单；随代码改 |
| `.beads/` | 路线图（epic）与任务；`bd ready` 看现在能做什么 |
| `ROADMAP.md` | 由 `scripts/roadmap-view.sh` 从 beads 生成的阶段视图，不手改 |
| `AGENTS.md` | Claude Code / Codex 共用的 agent 指令与任务纪律；`CLAUDE.md` 是它的符号链接 |
| `docs/analysis/devcenter/` | 同类项目 devcenter 的深度评审与借鉴范围判断（参考） |
| `docs/analysis/beads/` | 采用 beads 作为开发方法的评估：可行性、替代品横评、落地形态（参考） |

## 开发方法

任务跟踪用 [beads](https://github.com/gastownhall/beads)（`brew install beads`）。新克隆：`bd bootstrap`；日常：`bd ready` → `bd update <id> --claim` → 干活 → `bd close <id> --reason="..."` → `bd dolt push`。规则见 `AGENTS.md`。
