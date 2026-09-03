# agora

多 Agent 管理工具（M0 设计阶段完成：MISSION 与四篇 ADR 定稿、M1a / M1b 已拆成任务；正在进入 M1a 终端底座施工）。一句话定义与层次定位见 `MISSION.md`。

## 从哪里读起

分层规则（什么内容归哪个文件、谁是真相源）见 `MISSION.md` 文首"文档分层"；下表只是导航。

| 文件 | 作用 |
|---|---|
| `MISSION.md` | 北极星、层次定位（L1/L2/L3）、目标场景、不变量、施工规则、验收清单 |
| `docs/adr/` | 架构决策记录（"为什么"）；约定与索引见 `docs/adr/README.md` |
| `docs/spec/` | 实现形状：端点、配置、schema、线框、实测清单；随代码改 |
| `.beads/` | 路线图（epic）与任务；`bd ready` 看现在能做什么 |
| `ROADMAP.md` | 由 `scripts/roadmap-view.sh` 从 beads 生成的阶段视图（阶段门、验收要点、各 epic 的演示剧本），不手改 |
| `AGENTS.md` | Claude Code / Codex 共用的 agent 指令与任务纪律；`CLAUDE.md` 是它的符号链接 |
| `docs/analysis/devcenter/` | 同类项目 devcenter 的深度评审与借鉴范围判断（参考） |
| `docs/analysis/beads/` | 采用 beads 作为开发方法的评估：可行性、替代品横评、落地形态（参考） |

## 构建与运行

单 binary `agora`（Rust）内嵌 Vite + React 前端，所以前端要先于 cargo 构建（rust-embed 在编译期读 `web/dist`）：

```bash
npm --prefix web ci && npm --prefix web run build   # 前端
cargo build                                          # 内嵌并编译
./target/debug/agora                                 # 读 ~/.agora/config.yaml（可缺省）；监听 127.0.0.1:7680；curl /api/health → {"status":"ok"}
./target/debug/agora open                            # 另开终端：铸造一次性配对链接并打开浏览器（30 天免登录；`agora url` 只打印）
./target/debug/agora auth devices                    # 已配对设备；`agora auth revoke <id>|--all` 即时吊销
```

配对后用 cookie 调 API（写端点要带同源 `Origin`）：`GET /api/sessions` 列会话（含运行时里未登记的），`POST /api/sessions` 创建，`POST /api/sessions/<id>/kill` 会杀时要 `{"confirmed":true}`，`WS /api/events` 订阅增量；形态见 `docs/spec/api.md`，配置见 `docs/spec/config.md`。

守卫：`cargo fmt --check`、`cargo clippy --all-targets -D warnings`、`cargo test`（含 `tests/arch_boundary.rs` 源码边界扫描）、`npm --prefix web run typecheck` / `test`。CI 在 macOS 与 Ubuntu 22.04 / 24.04 上跑同一套（`.github/workflows/ci.yml`）。日志级别 `AGORA_LOG=debug`，JSON 输出 `AGORA_LOG_FORMAT=json`。

## 开发方法

任务跟踪用 [beads](https://github.com/gastownhall/beads)（`brew install beads`）。新克隆：`bd bootstrap`；日常：`bd ready` → `bd update <id> --claim` → 干活 → 验收完全机械的任务 `bd close <id> --reason="..."`（含人眼条款的由人按演示剧本关）→ `bd dolt push`。规则见 `AGENTS.md`，"做完"的定义见 `MISSION.md` §1.5。
