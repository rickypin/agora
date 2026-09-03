# ADR-001: 持久化运行时选择

- 状态：**Accepted**（2026-09-02；用户验收 D1–D9，其中语言、Kill 语义、双 socket 三点经单独确认）
- 日期：2026-09-02
- beads：`agora-90t.2`（输入清单与实测记录都在该 issue 注记里）
- 依赖：MISSION §0.1（Windows 不得被排除）、§0.2（合盖暂停不算失败）、§1.3（L1 完整覆盖）、§2.1（Thin）、§2.2 不变量 1–7、§3.1–§3.4、§4（会话模型、命名、生命周期）、§5.7、§7（技术栈一并决定）、§10.1；ADR-004（一跳转发对运行时的要求）。ADR-002 消费本文的 `LaunchSpec.env` 与 `Exit` 形态。

## Context

谁拥有 agent 进程的生命周期，决定了"浏览器关了、daemon 重启了、agent 还在不在"（不变量 1–4、7）。MISSION 把这一层定为 L1 的核心并要求**完整覆盖**（§1.3），而且要求运行时是 agent persistence 的唯一真相、SQLite 只存 metadata（不变量 7）。解空间被四条约束限定：

1. **agent 必须活过**客户端关闭、WebSocket 断开、daemon 重启；Mac 合盖是暂停不是失败（§0.2）。
2. **薄**：单 binary + 内嵌前端 + SQLite；Docker 不能是部署前提（§2.1）。
3. **运行时抽象不得排除 Windows**（§0.1、§11）；Windows 节点实质上是第二个运行时实现。
4. **fake-agent + 单进程多节点的测试骨架**必须在真实运行时上跑得起来（规则 4、9；`agora-3la`）。

devcenter 把生命周期交给 tmux，用最少代码换来不变量 1–3、scrollback、多客户端与外部 session 采纳（报告 §3.4 第 1 行）；代价是硬依赖 tmux、macOS / Linux only、tmux 泄漏进 session 模型、gateway 改用户全局 tmux 选项。它的 470 个测试 2026-09-02 在本机 tmux 3.7c 上全绿，不变量 1–3 的 tmux 层机制已亲手验证（`agora-90t.6`）；本文写作时又补测了 `-f` 专用配置、`-c` / `-e`、`respawn-pane`、进程组信号、PATH 探测（见附录 A）。

## 决策问题

agent 进程由谁托管；托管层如何被抽象、边界在哪；语言与技术栈用什么。

## 备选项

| 备选 | 优点 | 缺点 | devcenter 的经验 |
|---|---|---|---|
| **tmux 承载**（daemon 永不直接 spawn agent） | 不变量 1–3 结构性成立，零代码；scrollback、多客户端 attach、外部会话采纳（A22）、退出码（`remain-on-exit`）全是现成的；用户随时可用 `tmux attach` 逃生；安装一行 | 硬依赖外部进程：tmux server 崩或 `kill-server` 全部 agent 一起死；无 Windows 实现；client / server 版本不匹配时整个运行时不可用；pane 进程继承 tmux server 的环境（PATH 陷阱） | ✅ 做对的一件事（报告 §3.4 第 1 行、§6.2 第 1 行）；泄漏与全局选项是实现问题不是路线问题 |
| **自管 PTY + supervisor**（daemon 内 portable-pty，独立 supervisor 进程持有 PTY） | 无外部依赖；天然跨平台（ConPTY）；环境与 scrollback 完全由 agora 控制 | 要自己实现 detach / 多客户端 / scrollback ring / 崩溃隔离 / 采纳；supervisor 本身又是一个需要"活过 daemon 重启"的进程，等于重写 tmux 的子集；V1 时间全花在这里 | 未验证；报告 §6.4 第 5 条留白 |
| **Agent SDK 内嵌**（不跑 TUI，以 SDK 驱动） | 状态源天然结构化；无需状态检测 | 每个 agent 一套 SDK、无 generic shell、无法采纳用户在别处起的会话（A16、A22）；与"终端是传输层"（§1.2）和 Adapter 边界（规则 5）冲突；等于替 agent 厂商重写 TUI | 留白；devcenter 用 hook 拿到了 SDK 才有的结构化信号，说明不必内嵌 |
| **容器 / 云沙箱**（每 agent 一个容器） | 隔离强；远程 / Windows 友好 | Docker 是 §2.1 明确避免的部署前提；容器只解决隔离，**不解决持久化**（容器里仍要一个 tmux 或 supervisor）；仓库挂载、凭据、延迟都变复杂 | 留白；§11 把它与 Windows 一起归入第二运行时 |

## Decision

### D1 运行时：tmux（V1，macOS / Linux）

选 tmux。理由不是"devcenter 这么做"，而是四条约束里前两条它免费满足、第四条已实测（附录 A）、第三条由 D9 的抽象兜住。

否决理由：

- **自管 PTY + supervisor**：它解决的问题（跨平台、无依赖）V1 不需要，付出的是重写 detach / 多客户端 / scrollback / 采纳；而且它正是 Windows 第二运行时要做的事——留到那时做，V1 不做两遍（D9）。
- **Agent SDK 内嵌**：与 A16 / A22（采纳用户在任何地方起的会话）直接冲突；结构化信号走 hook 就能拿到（ADR-002），不必放弃 TUI。
- **容器**：违反 §2.1；且不回答"谁托管进程"，只是把问题搬进容器。

**明说的赌注**：不变量只有"agora crash ≠ agent crash"，**没有"tmux crash ≠ agent crash"**。tmux server 崩溃或被 `kill-server`，该 socket 上全部 agent 一起死。接受这个赌注的依据：tmux server 是二十年的稳定件，故障率远低于 agora 自己；残余风险用 D3（专用 socket，用户的 `kill-server` 杀不到）与守卫 2（agora 自己永远不调 `kill-server`）压到最小。

### D2 Runtime 抽象边界

`runtime/` 是唯一知道 tmux 的地方；session 模型、observer、API、前端只看到下面的接口（伪签名，语言无关；ADR-002 的 Adapter 接口同风格）：

```
Runtime {
    kind() -> "tmux" | "native"
    create(spec: LaunchSpec) -> Ref        # 一次调用完成创建 + 会话级选项
    list() -> [RuntimeSession]             # 全部 socket 的全部会话，含非 agora 创建的
    inspect(ref) -> RuntimeSession
    attach(ref, size) -> PtyStream         # 多客户端各自独立一条流；resize 走流上的接口
    capture_tail(ref, lines) -> bytes      # 只供观测与预览（§5.7）
    terminate(ref, grace) -> ()            # TERM 进程组 → grace → KILL；不销毁会话
    respawn(ref, spec) -> ()               # 同一会话内重建（Restart）；保留 scrollback
    remove(ref) -> ()                      # 销毁会话；进程仍活着时拒绝（StillAlive）
}

LaunchSpec     { name, command, cwd, env: {k: v}, size: {cols, rows} }
RuntimeSession { ref, name, pid, alive, exit: None | Code(n) | Signal(name),
                 exited_at, title, cwd, attached, size, managed }
Ref            = "tmux:<socket>:<session>" | "native:<id>"     # 只在 runtime 内解析
RuntimeError   = NotFound | StillAlive | ServerUnavailable | VersionMismatch
               | Timeout | Failed { stderr_tail }
```

规则：

- **`Ref` 是不透明字符串**，形态 `tmux:agora:ag-a81c28`、`tmux:default:mywork`；`session.runtime_ref` 原样落库，解析只发生在 `runtime/tmux/`。`managed` = 是否在 agora 自己的 socket 上。
- **子进程只经一个入口**：argv 直传（不经 shell，zsh 会把 `=name:` 当等号展开，实测 2026-09-02）；每次调用有超时（默认 5 s；attach 是长进程不计）；在 blocking 线程上跑；stdout / stderr 都排空、stderr 只保留尾部 4 KB 进错误（devcenter 两次管道死锁换来的规则）；错误按 `RuntimeError` 分类，不做字符串匹配（规则 10）。
- **`exit` 是数据不是判断**：`Code(0)` / `Code(n)` / `Signal(name)` 原样交给 observer；FINISHED / FAILED 的映射、以及"agora 自己 terminate 的会话按信号退出算什么"归 ADR-002（已写入 `agora-90t.3` 注记）。
- **`attach` 在实现里交出的是 `AttachSpec { argv, env }` 而不是 `PtyStream`**（2026-09-03，`agora-xqa.7`）：PTY 由 Terminal Gateway 一处持有，运行时只回答"起什么进程"，这样 PTY 读写的 blocking 线程只在 gateway 一处，`runtime/` 也不用依赖 portable-pty。签名里的 `PtyStream` 读作"gateway 用 AttachSpec 起出来的流"。
- **`LaunchSpec.env` 是 ADR-002 把每会话身份交给 agent 的通道**（定稿后是 `AGORA_SESSION_ID` / `AGORA_EPOCH`；hook 投递走投递箱 + unix socket，没有一次性 token，ADR-002 D3）；tmux 路线下走 `new-session -e`，不写 shell、不改用户配置。

tmux 映射（具体命令行是 spec 的事，这里只钉住形状与理由）：

| 操作 | tmux | 为什么这样 |
|---|---|---|
| create | `tmux -L agora -f <conf> new-session -d -s <name> -c <cwd> -e K=V… -x 160 -y 48 <command> \; set-option -t =<name>: remain-on-exit on` | 一次调用消除秒退竞态（实测保住 `exit 7`）；`-c` / `-e` 替代 `cd &&` 与 shell 拼接；`-x/-y` 给 detached 会话一个像样的初始尺寸而不是 80×24；`-f` 每次都带，server 不在时自动按 agora 配置起 |
| list | 每 tick 每 socket **一次** `list-panes -a -F …`（session_name、pane_pid、pane_dead、pane_dead_status、pane_dead_signal、pane_dead_time、pane_title、pane_current_path、session_attached、尺寸） | 子进程数 O(socket) 不 O(会话)；devcenter 每 tick 每会话两个进程在 50 会话时会显形 |
| attach | daemon 自己的 PTY 内 `tmux -u -L <socket> attach-session -t =<name>`，**不带 `-d`** | MVP 允许多客户端同看（§6.8）；`-u` 强制 UTF-8（launchd 起的 daemon 没有 locale）；`=` 精确匹配防前缀误伤 |
| capture_tail | `capture-pane -p -J -S -<n> -t =<name>:` | `-J` 拼回折行；只在需要文本兜底或预览时调用 |
| terminate | `kill(-pane_pid, SIGTERM)` → 5 s → `SIGKILL`，不经 tmux | tmux 给每个 pane 起新进程组，pgid == pane_pid（实测），整组杀不留孤儿；`kill-session` 只发 SIGHUP，忽略 SIGHUP 的子孙会残留 |
| respawn | 活着先 `terminate`（进程组 SIGTERM → SIGKILL）；`display-message -p '#{window_height}'` → `resize-window -y 1`（可见屏推进 history）→ `respawn-pane -k -t =<name>: -c <cwd> -e … <command>` → `resize-window -y <原高度>` + `set-option -w -u window-size`（放大时 tmux 把那些行拉回可见屏） | 同一会话、同一名字；**保留前一轮 scrollback 与最后一屏**（光标以下的行除外）。`respawn-pane` 的 screen_reinit 只留 history；tmux 打印 "Pane is dead" 前的那次换行只救顶行，3.2a 信号退出连这一行都不打（agora-6bo；3.2a / 3.4 / 3.7c 实测 2026-09-03） |
| remove | 先 `inspect` 确认 `pane_dead=1`，再 `kill-session -t =<name>:` | 清理永远只碰已退出的会话；活着的一律拒绝 |

### D3 socket：专用 socket 承载、默认 socket 只读

agora 创建的会话全部在**专用 socket** `-L agora` 上；采纳（A22）时**只读扫描**默认 socket（配置 `runtime.tmux.adopt_sockets`，默认 `["default"]`），对其上的会话只做 list / attach / capture，**绝不 set-option**。两个 socket 的张力（`agora-90t.2` 注记 ⑨）这样解：

- 用户在自己终端里的 `tmux kill-server` 杀不到 agora 的会话；agora 也永远不动用户 server 的任何选项（devcenter 改全局 `mouse` / `set-clipboard` 的错误不重犯）。
- 采纳的会话保持用户原有配置（§4.1），因为 agora 根本不写它。
- 代价：用户手动看 agora 的会话要 `tmux -L agora ls`；每 tick 多一次 list 调用。
- 测试骨架直接受益：每个 fake 节点一个 socket（`agora-test-<pid>-<n>`），单进程多节点互不干扰——devcenter 的 `Tmux::with_socket` 做法。

专用 server 由 daemon 启动时 `tmux -L agora -f <conf> start-server` 拉起，`<conf>` 由 agora 生成到 `AGORA_HOME`（默认 `~/.agora`，ADR-003 D6）：

```
set -g history-limit 10000        # D6；必须在 server 级设：3.7 以前对已存在的 pane 不生效（实测，反例见附录 A）
set -g remain-on-exit on          # 退出后保留 dead pane 供读退出码（create 时再 set 一次，双保险）
set -g status off                 # UI 已显示名字与状态；只影响本 socket
set -g window-size latest         # 最近 resize 的客户端赢，避免"最小客户端"把 agent 挤成 80×24
set -g destroy-unattached off
set -g exit-unattached off        # 两条都是默认值，写出来是为了钉死：没人 attach 也不许销毁
```

### D4 生命周期映射（MISSION §4.6 的运行时含义）

| 操作 | 运行时动作 | 备注 |
|---|---|---|
| 创建 | `create` → 写 metadata；写库失败 → `remove` 回滚 | 先外部资源后 metadata，不留孤儿记录（不变量 7 同构） |
| Detach / Close Tab | 关掉那条 `attach` 流 | 运行时不知道浏览器存在 |
| **Kill** | `terminate`（TERM → KILL）；**会话保留为 dead pane**；写 `killed_at` | 与 MISSION §4.6 "杀掉运行时会话"字面不同，见下。`killed_at` 是"用户做过这件事"的事件时刻，与 `ended_at` 同类而非活性（不变量 7 不禁）：按信号退出 + `killed_at` 非空 → FINISHED（killed by user），否则 FAILED；daemon 重启后照旧（2026-09-03 之前只在内存里记，重启后被 Kill 的会话变 FAILED，agora-xqa.16） |
| **Restart** | `respawn`；命令由 Adapter 按 `agent_session_id` 重算 resume 参数（§5.6）；`epoch + 1`、写 `spawned_at`、清 `killed_at` | 同一会话、同名、同 cwd，scrollback 与最后一屏保留（D2 respawn 行的缩放窗口手法，agora-6bo）；活着才需确认（§8）。`spawned_at` 是本代进程的起始时刻，进程状态层的 2 s STARTING 窗口只看它——不看 `updated_at`，那个被 rename / kill / 清理刷新（agora-xqa.15） |
| 清理 | `remove`（只对 dead pane） | 触发：用户在 Dashboard 确认 / Delete Metadata / Restart 复用。**V1 不做定时 GC**：看没看过只有人知道 |
| Delete Metadata | 删行；会话活着 → 留在 socket 上变成"未注册"（可再采纳）；已死 → 顺手 `remove` | 两个端点、两个语义（§7.3） |
| daemon 重启 | `list()` ⨝ SQLite | 已知 ref 活着 → 重建；已知 ref 已死 → 按 `exit` 报 FINISHED / FAILED；已知 ref 不在 → `ended_at` 补今天、状态 UNKNOWN（reason: runtime session missing），可 Delete 或 Restart（此时 Restart 退化为同名 `create`，无 scrollback）；未知 ref 在 agora socket → 未注册（多半是库丢了）；未知 ref 在默认 socket → 可采纳的未注册会话（采纳后 `origin = adopted`，§5.5） |

**Kill 为什么不销毁会话**：MISSION 已经规定 FINISHED / FAILED 的会话"用户看过之后"才清理（§4.6）。Kill 只是让 agent 退出的另一种方式，让它走同一条清理路径有三个好处：scrollback 还在（杀掉失控 agent 后正好要看它干了什么）、Restart 仍是同一会话、Kill / Delete / 清理三个动词的边界不再互相踩。代价是 Kill 之后行不会自己消失，要人确认一次。MISSION §4.6 已据此回写（v0.11，2026-09-02）：Kill = "杀掉 agent 进程（显式确认）；运行时会话连同其输出保留，按清理策略回收"。

### D5 attach 与一跳转发

Terminal Gateway 的一端是 daemon 自己 open 的 PTY，里面跑 `tmux attach`；每个浏览器客户端各一条，尺寸由 `window-size latest` 仲裁。WS 断开 → 给 attach 进程 SIGHUP → **确认它退出后**才释放 PTY master（devcenter 实锤：portable-pty 释放时写 `^D`，attach 若还活着会把 `^D` 送进 pane 杀掉 agent；宁可泄漏 fd）。keepalive 20 s / 65 s 沿用。

一跳转发（ADR-004）对运行时**没有新要求**：peer 节点经本节点的终端 WS attach，在运行时眼里只是又一个客户端。延迟增量 = 一次 WS 中继，LAN / tailnet 下满足 §10.1 的 < 10 ms。

### D6 观测默认值（§3.3、§5.7、§10.1 交给本 ADR 的数字）

| 项 | 默认 | 理由 |
|---|---|---|
| list tick | 2 s，全部会话一次调用 | 进程存活与退出码的唯一来源；hook 路径不等它 |
| capture 间隔 | RUNNING 2 s；WAITING / IDLE 5 s；已退出不 capture | 只对需要文本兜底或预览的会话做 |
| capture 行数 | 200 行，`-J` | 兜底模式只看末尾窗口，不全 tail `contains` |
| tail buffer | ≤ 64 KB / 会话，每次 capture 整体替换，不累积 | §5.7：不得把无限输出留在内存 |
| `history-limit` | 10000 行（`runtime.tmux.history_limit`） | 100000 被否：每行约 1 KB 网格，50 会话最坏 5 GB 在 tmux server 里；10000 够浏览器回看一天，要更多改配置 |
| xterm.js scrollback | 10000 | 与运行时对齐 |
| 子进程超时 | 5 s | 一次 hang 住的调用不得拖住所有会话（不变量 5） |

### D7 运行环境：PATH、locale、tmux 版本

- **pane 进程继承 tmux server 的环境，不是客户端的**（devcenter `which.rs` 结论，本机复现：launchd 式最小环境下 pane 的 PATH 是 `/usr/bin:/bin`，`claude` 不可见；`$SHELL -l -c` 也不够，本机 `~/.local/bin` 是 `.zshrc` 加的）。因此 daemon 启动时**探测一次用户 shell 的 PATH**：`$SHELL -l -i -c 'printf "\n__AGORA_PATH__%s\n" "$PATH"'`，stdin `/dev/null`、`TERM=xterm-256color`（`TERM=dumb` 会让 `.zshrc` 提前退出，实测）、5 s 超时、取哨兵行；成功则用它作为 daemon 自身与专用 tmux server 的 PATH，失败则退回 daemon 自己的 PATH 并在 health 里标明 `path_source`。**存储的命令保持可移植**（`claude`），不改写成绝对路径；机器相关的东西（PATH、resume 参数、hook 环境）每次启动现算（`agora-90t.2` 注记 ⑪）。
- **locale**：daemon 环境缺 UTF-8 的 `LANG` / `LC_ALL` 时，create 与 respawn 注入 `-e LANG=C.UTF-8`（devcenter 的 CJK 乱码教训）；安装脚本同时在 launchd / systemd 单元里写 `LANG`（A26）。
- **tmux ≥ 3.2**：`new-session -e`（3.2）、`window-size latest`（3.1）、`respawn-pane -e`（3.0）。`pane_dead_signal` / `pane_dead_time` 是 3.3 才有的（ubuntu-22.04 CI 的 3.2a 实测两者皆空，2026-09-03）：3.2a 上被信号杀死的 pane 报 `Exit::Signal("unknown")`，`exited_at` 取运行时首次观测到死亡的时刻；pane 死后 `pane_dead_status` 为空既可能是退出码尚未收集（pty EOF 先于 waitpid）也可能是信号退出，运行时按首次观测到死亡后 2 s 的宽限区分：宽限内 `exit = None`（UNKNOWN，退出码尚未收集，与 3.3+ 同一形态），之后才报 `Signal("unknown")`，否则正常 `exit 7` 会先闪一次 FAILED（agora-5nu；ubuntu-22.04 容器紧密轮询 30 次撞上 2 次，2026-09-03）；另有一条不分版本的限制：进程**经 PTY 交互输入之后**退出时，tmux 有时根本不收集退出状态——pane 死了但 `pane_dead_status` 与 `pane_dead_signal` 都为空，而且不会再补上（不是"尚未收集"的短暂窗口）。实测 2026-09-03（`read` 后 `exit 3`，send-keys 喂一行）：3.2a 密集轮询 5/6 丢、200 ms 轮询 1/6 丢，3.4 1/6 丢，3.7c 0/6。后果是这类会话在 3.2a 上退化为 `Signal("unknown")` → FAILED，在 3.3+ 上 `exit` 一直是 `None` → **UNKNOWN**，A12 的「退出码可读」对交互过的会话不成立。`tests/invariants.rs` 因此只断言「已死且结论自洽」；不经交互的秒退退出码仍是硬要求（`quick_exit_keeps_exit_code`）。定性与去留见 agora-tc4；暂不因此抬高下限。覆盖 Ubuntu 22.04（3.2a）、24.04（3.4，zuan 实机）、brew（3.7c）。daemon 启动时读 `tmux -V`（文档化接口）；低于下限或 server 协议不匹配（`brew upgrade tmux` 之后新 client 连不上旧 server）→ health 报 `runtime: degraded` 并给出原因，已有会话显示 UNKNOWN，**不杀 server、不退出 daemon**。安装脚本负责装 tmux；agora 的升级命令（A39）不碰 tmux。

### D8 语言与技术栈

**Rust**：tokio、axum（HTTP + WS）、portable-pty、rusqlite（bundled）、serde、rustls（+ rcgen；ACME 库归 ADR-003）、rust-embed（内嵌前端）、tracing。前端 React + TypeScript + Vite + xterm.js（+ fit addon），PWA 在手机阶段加。单 binary，三平台各自 CI 原生构建。

选 Rust 的理由：参考实现同栈——devcenter 的 tmux 封装、gateway 的 VEOF 与 `Utf8Stream`、隔离 socket 测试都是实测过的坑，可以逐条对照（借鉴设计不复制代码，报告 §6.5）；本机与用户仓库已有 Rust 工具链（cargo 1.90，5 个 Rust 仓库；本机无 Go）；portable-pty 同时覆盖 unix PTY 与 Windows ConPTY，是 D9 第二运行时现成的地基；devcenter 实测 idle 9 MB。

否决 Go 的理由**不是能力**（stdlib 的 TLS / autocert / embed / `exec` 超时更顺手，交叉编译更省事），而是：参考实现要整体重译、本机零工具链、Windows PTY 生态弱于 portable-pty。判据：如果 M1a 里出现第二种并发模型（例如为了子进程或 PTY 绕开 tokio），说明 async 成本失控，**在 M1a 结束前**重议；之后是重写，不再讨论。评估点钉在 Terminal Gateway 打通时（`agora-xqa.10` 的验收条款：是否出现第二种并发模型、PTY blocking 线程与 tokio 的边界是否干净，结论写该 issue 注记；2026-09-03）——那是 M1a 里 PTY 与 async 第一次正面相遇的地方。

为避开 devcenter 在 Rust 上踩的坑，施工约束四条：只有一种并发模型（tokio）；子进程只经 `runtime::exec` 一个入口（超时 + blocking + stderr 有界）；PTY 读写在 blocking 线程 + 有界 channel；模块边界用错误枚举，不用 `anyhow`，锁不 `expect("poisoned")`。

判据评估结果（2026-09-03，`agora-xqa.10` 网关打通时）：没有出现第二种并发模型，不重议。一个实测边界：PTY 的阻塞 read 不能放 `spawn_blocking`——runtime 关闭时会等它，而它只在 attach 退出后返回，整个进程挂死；PTY 与子进程的阻塞 I/O 一律用 `std::thread` + 有界 channel（`src/gateway/mod.rs`、`runtime::exec`）。

### D9 Windows 与第二运行时的留位

`Runtime` 接口 + `Ref` 的 scheme 前缀就是留位：第二个实现是 **native supervisor**（每会话一个 supervisor 进程持有 ConPTY / PTY、自带 scrollback ring、经本地 socket 多客户端 attach），随 Windows 进入范围时另立 ADR 并实现（`agora-fs8`）。V1 不写它一行代码，但 V1 的代码**不得**在 `runtime/tmux/` 之外假设 tmux 存在（守卫 1）。容器 / 云沙箱 / SDK 运行时同归此处。

## Non-Goals

- 不做终端复用器：分屏、多窗口、tmux 键位、用户 tmux 配置都不管（MISSION §1.4"不是终端产品"）。
- 不管理用户自己的 tmux：默认 socket 只读；不装插件、不改选项、不替用户起 server。
- 不做进程隔离 / 沙箱（容器归第二运行时）；不做资源限制。
- 不做 dead 会话的定时回收（V1 由人确认）；不做 scrollback 落盘（§5.7）。
- 不做跨节点搬迁（§1.4）；`respawn` 只在同一会话内。
- 不在本 ADR 定 hook 投递形式、文本兜底模式、状态机驻留时间（ADR-002）。

## 什么会让它变危险

- **tmux 泄漏到 `runtime/` 之外**（session 模型或 API 里出现 socket 名、`tmux` 字面量、pane 概念）→ 第二运行时无处安放。守卫：源码边界检查，`tmux` 标识符只允许出现在 `runtime/tmux/`、`scripts/`、测试 → `tests/arch_boundary.rs`。
- **有人调 `kill-server`，或对默认 socket 写选项** → 用户全部会话陪葬 / 用户 tmux 行为被改。守卫：`TmuxRuntime` 没有这两个方法；`managed=false` 的 ref 上 `create` / `respawn` / `remove` / 任何 `set-option` 都返回错误；测试用命令录制器断言默认 socket 只收到 list / attach / capture → `tests/runtime_tmux.rs::foreign_socket_is_read_only`；源码扫描 `kill-server` 为零 → `tests/arch_boundary.rs`。
- **子进程无超时或 stderr 未排空** → 一个 hang 住的 tmux 拖住所有会话（不变量 5）、daemon 死锁。守卫：唯一入口 `runtime::exec`；测试用会挂起的假 `tmux` 与狂写 stderr 的假 `tmux` → `tests/runtime_exec.rs::hung_child_times_out_in_5s`、`::stderr_flood_does_not_deadlock`。
- **list 退化为每会话一次子进程** → 50 会话时每 tick 上百个 tmux 进程（§10.1；devcenter 每 tick 每会话两个进程的反例）。守卫：`list()` 每 tick 每 socket 恰好一次 `list-panes -a`；测试用命令录制器断言调用数不随会话数增长 → `tests/runtime_tmux.rs::one_list_call_per_socket_per_tick`。
- **经 shell 拼接调用 tmux** → 名字含空格 / `;` / `=` 时命令被改写。守卫：入口只收 argv；测试用含 `; =x:` 的会话名断言 tmux 收到字面量 → `tests/runtime_exec.rs::argv_is_never_shell_interpolated`。
- **`remove` 杀到活着的进程** → "清理"变成没有确认的 Kill（规则 6）。守卫：`remove` 先 `inspect`，`alive` 即拒绝 → `tests/runtime_tmux.rs::remove_refuses_alive_pane`。
- **SQLite 长出活性字段** → 不变量 7 失守，重启后库与运行时打架。守卫：schema 测试断言 `sessions` 表无 `status` / `alive` / `exit_code` 列，`list()` 每次 join 运行时 → `tests/schema.rs::no_liveness_columns`。
- **不变量 1–3 被某次重构悄悄破坏** → 守卫：真实 tmux、隔离 socket 上的 fake-agent 集成测试：杀 attach PTY、断 WS、销毁并重建节点对象，agent 仍活且退出码可读 → `tests/invariants.rs`（`agora-xqa.13`，建立在 `agora-3la` 的骨架上）。
- **`history-limit` 只在 create 后 set** → 3.7 以前的 tmux 上首 pane 拿到默认 2000 行。守卫：测试断言新建 pane 的 `#{history_limit}` 等于配置，CI 矩阵含 Ubuntu 22.04（3.2a）→ `tests/runtime_tmux.rs::history_limit_applies_to_first_pane`。
- **PATH 探测失败被静默吞掉** → agent "命令不存在"却报成 agent 崩溃。守卫：`SHELL=/bin/false` 时 health 的 `runtime.path_source = daemon` 且带原因 → `tests/env_probe.rs`。
- **attach 进程未死就释放 PTY** → `^D` 打进 pane 杀 agent。守卫：gateway 测试用一个忽略 SIGHUP 的假 attach，断言 PTY 泄漏而非写 `^D` → `tests/gateway.rs::never_writes_eof_to_live_attach`。

## Consequences

**正面**：不变量 1–3 与 A8–A12 零代码成立；A22 采纳与用户逃生口（`tmux -L agora attach`）免费；测试骨架每节点一个 socket，单进程多节点可行（`agora-3la` 解锁）；Restart 保留 scrollback；升级 agora（A39）与 agent 无关。

**负面 / 接受的代价**：tmux server 单点（D1 赌注）；client / server 版本不匹配期间运行时不可用（D7 只报不修）；用户手动看会话要多敲 `-L agora`；Kill 之后需人确认一次清理；Rust 的编译时间与 async 心智成本（D8 判据）。

**回写**（2026-09-02 已做）：MISSION §4.6 Kill 一行与清理段（D4，v0.11）；`docs/spec/config.md` 的 `runtime` 段与 `terminal.scrollback`。`AGENTS.md` 第 7 行改指 `docs/adr/` 等 ADR-002 / 003 一起做（`agora-90t.7`）。

**跟进 issue**：

- `agora-3la` 测试骨架：加注记——每 fake 节点一个 tmux socket；fake-agent 做成 agora 二进制的子命令（跨平台，不依赖 bash）；不变量 1–3 的三个测试形状见守卫。
- `agora-90t.3` ADR-002：加注记——`Exit::Signal` 的映射、agora 自己 terminate 的会话按什么报；每会话身份走 `LaunchSpec.env`。
- 新建（均 discovered-from `agora-90t.2`）：`agora-7ku.1` 安装脚本装 tmux ≥ 3.2 并写 `LANG`（M2，A26）；`agora-xqa.4` tmux 版本 / 协议不匹配的 health 降级（M1a）；`agora-fs8` Windows native supervisor 运行时 ADR（backlog）。

## 参考

- `docs/analysis/devcenter/README.md` §3.2、§3.4（运行时行、语言行）、§3.5 第 6 条、§6.2、§6.4 第 5 条、§6.5
- `docs/analysis/devcenter/appendix-a-backend.md` §2a（生命周期、先 tmux 后 SQLite）、§2c（VEOF、keepalive）、§4 借鉴第 1–6 条
- devcenter `src/which.rs` 文件头注释（pane 继承 server 环境的论证）；`src/tmux.rs`（`attach_args` 的 `-u` 位置、`window-size latest`）
- tmux 3.7c `CHANGES`：`history-limit` 作用于已有 pane（3.7）、`new-session -e`（3.2）、`window-size latest`（3.1）、`respawn-pane -e`（3.0）

## 附录 A：实测记录（2026-09-02，macOS 15.6.1，tmux 3.7c，zsh）

| # | 实测 | 结果 | 决定 |
|---|---|---|---|
| 1 | `new-session … \; set-option remain-on-exit on` 一次调用，命令 `bash -c 'exit 7'` 秒退 | `pane_dead=1`、`pane_dead_status=7` 可读 | D2 create 合并调用 |
| 2 | `new-session` 之后再 `set-option history-limit 12345` | 3.7c 上首 pane 拿到 12345；**但 `CHANGES` 3.7 条目："When history-limit is changed, apply to existing panes, not just new"——3.6 及以前只对新 pane 生效**，zuan 的 3.4 会拿到默认值 | D3 改在 server 级 `-f` 设 |
| 3 | 专用 socket + `-f conf`（history-limit / remain-on-exit / status / window-size latest） | server 启动即全部生效；`-c /tmp -e AGORA_TEST=1` 在 pane 内可见 | D3 conf、D2 `-c` / `-e` |
| 4 | pane 进程的 pgid | `pgid == pane_pid`（tmux 为每个 pane 新建进程组） | D2 terminate 整组杀 |
| 5 | `kill -TERM -- -<pane_pid>` / `-KILL` | `pane_dead=1`、`pane_dead_status` 为空、`pane_dead_signal=term` / `kill`、`pane_dead_time` 有值 | `Exit::Signal` 形态；list 字段含 signal |
| 6 | `respawn-pane -k -c <dir> -e K=V <cmd>` 对活 pane；再对 dead pane 不带 `-k` | 都成功；新 cwd / env 生效；`exit 3` 后 `pane_dead_status=3`；**前一轮输出仍在 scrollback**（补记 2026-09-03：只有已滚入 history 的行在，可见屏被 screen_reinit 清掉；这里能过是 pane 死时 "Pane is dead" 前的换行把那一行推进了 history，3.2a 信号退出没有这一行。现行做法见 D2 respawn 行，agora-6bo） | D2 respawn = Restart |
| 7 | 最小环境（launchd 式 PATH）起 tmux server，pane 内 `command -v claude` | PATH = `/usr/bin:/bin`，找不到 claude；`$SHELL -l -c` 包一层仍找不到（`~/.local/bin` 在 `.zshrc`）；`$SHELL -l -i -c` + `TERM=xterm-256color` + 哨兵行 → 完整 PATH（含 `~/.local/bin`、cargo、grok）；`TERM=dumb` 时 `.zshrc` 提前退出、探测失败 | D7 PATH 探测方案与 TERM 反例 |
| 8 | zsh 下 `-t =name:` 不加引号 | zsh 把 `=name` 展开为命令路径查找 | D2 argv 直传、不经 shell |
| 9 | devcenter 470 测试（`agora-90t.6`） | 全绿；不变量 1–3 的 tmux 机制亲手验证 | D1 |

## 附录 B：事故记录

（上线后追加）
