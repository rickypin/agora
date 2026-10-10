# agora

多 Agent 管理工具：跨节点的 CLI coding agent 控制台（Rust daemon + 内嵌 React 前端）。桌面是全功能工作台；iPhone 上是交互收件箱（主屏 PWA + Web Push）。一句话定义与层次定位见 `MISSION.md`。

## 从哪里读起

- 了解产品目标与边界：[MISSION](MISSION.md)。
- 查看当前阶段：[ROADMAP](ROADMAP.md)；任务实况用 `bd ready` / `bd show <id>`。
- 查设计与实现：[ADR 索引](docs/adr/README.md)、[规格索引](docs/spec/README.md)。
- 开始贡献：[AGENTS](AGENTS.md)，包含任务纪律、按需阅读入口与隔离规则。

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

上述默认路径用于运行个人节点。开发代检必须先按下文「测试实例隔离」规则配置独立 home、端口与 tmux socket；不要用默认命令启动测试实例。构建不等于部署，当前节点形态见 `docs/spec/instance.md`。

日志级别 `AGORA_LOG=debug`，JSON 输出 `AGORA_LOG_FORMAT=json`。

## 验收命令

CI 的执行状态与 DoD 见 MISSION §1.5。合入验收按下表取适用项的并集，另加 issue 点名的守卫；出现失败不能用缩小范围代替解决。

| 适用范围 | 检查 |
|---|---|
| 所有改动 | `scripts/doc-lint.sh`、`git diff --check`；文档内容还须与相关决策 / 实现核对 |
| 程序、依赖、构建配置或测试改动 | `cargo test`、`cargo clippy --all-targets -- -D warnings`、`npm --prefix web test` |
| Rust 源码 / 测试改动 | `cargo fmt --all -- --check` |
| 前端或前端构建配置改动 | `npm --prefix web run typecheck`、`npm --prefix web run build`，构建先于 cargo |
| 纯文档改动 | 只需第一行，不重跑应用测试；若文档改变了行为契约，还须确认实现满足它，否则不能按“纯文档已完成”关闭 |

`.github/workflows/ci.yml` 保留跨平台矩阵；`.claude/handoff.json` 的 paths 只控制 lane 自动检查范围。脚本成功不代表合入验收已齐，集成者仍补足上表。UI 额外代检按 [设计系统](docs/spec/design-system.md) 选择；其按风险缩小的只是代检范围，不免除本表门禁。

## 开发方法

任务跟踪用 [beads](https://github.com/gastownhall/beads)（`brew install beads`），库在 zuan 的 Dolt 服务器上：新机器口令进 `~/.config/beads/credentials`、`.beads/.env` 写服务器连接，不跑 `bd bootstrap`。贡献流程统一见 [AGENTS](AGENTS.md)，完成定义见 MISSION §1.5。

## 开发验证

以下是启动测试实例或并行工作区时才需读取的操作方法；任务、验收与授权仍从 [AGENTS](AGENTS.md) 进入。

### 测试实例隔离

- daemon 使用独立 `AGORA_HOME`、监听端口、`runtime.tmux.socket`，`adopt_sockets: []`；不操作开发机真实会话。临时文件带 issue id，清理按自己记录的 PID / socket，不用宽泛 `pkill`。
- 集成测试从 `tests/common/isolate.rs` 取 home / socket 名，端口 `:0`，Drop 用 `isolate::kill_tmux`，外部进程等待用 `isolate::PROC`；守卫 `tests/test_isolation.rs`。只带 PID 会撞名，裸 kill-server 会遗留 socket（2026-09-10 实测）。
- 起 pi lane agent 加 `-ne` 或 `-e` 只加载所需扩展：扩展固定 URL 会绕过 AGORA_HOME 隔离，污染真 daemon（2026-10-08 实测）。
- 浏览器代检前查 `bd memories agent-browser`；按键后核对 URL / 页面身份，about:blank 的 errors 空不是通过。先排工具假象，不跳过产品断言；设备与 fixture 的证据边界见设计系统。

### 并行工作区

- 一任务一 worktree：`git worktree add ../agora-wt/<id> -b <branch> main`，分支按宿主约定；beads 库在服务器上，不复制库；worktree 用自己检出的 `.beads` 连库，基于 2026-10-10 切换前提交的找不到库，先 merge main。新建且尚未施工的 worker 先 `git merge --ff-only main` 并核对 HEAD == main；已有工作的 worktree 先保存进度，不重置到 main。获授权保存本任务提交后、集成前 `git rebase main`，有未提交改动时先处理，不为 rebase 丢弃改动。
- Claude Workflow / Agent 隔离曾从 origin/main 起步（2026-09-06 实测）；启用前 `git fetch origin` 核实基线，确保工作区干净且 `git rev-list --left-right --count main...origin/main` 为 `0 0`；单看未推送提交为空不能排除本地落后。push 仍按授权。文件归属写 issue notes，改共享文件先看其它分支未合入改动；复用已有组件，必要的共享抽取单独提交并按授权推送。
- node_modules 与 lockfile 一致时从主仓复制，否则按 lockfile 安装；先 `npm --prefix web run build` 再 cargo（rust-embed 编译期读取 `web/dist`）。Linux 用 `cp -rf` 拷 node_modules，不克隆 target，先查磁盘再定并行度；macOS 用 `cp -Rfc` 克隆 node_modules / target。target 不硬链接。
- worktree 内不跑 `cargo sweep -t`（clonefile 保留 mtime，会误删复用缓存）；主仓按膨胀实况清理，清后需克隆测试产物则先 `cargo test --no-run`。排障依据查 `bd memories cargo-sweep`，不把历史性能数字当永久门槛。
