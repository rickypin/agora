# 配置与存储

原则在 MISSION §9；本文是文件形态。`runtime` 段已按 ADR-001 定稿；`tls` 段等 ADR-003 定稿后重写。

## 配置文件（每个节点一份）

```yaml
server:
  listen: "127.0.0.1:7680"    # 要被 peer 或手机访问时显式改为非 loopback，且必须已配置认证与 TLS（§8）
node:
  id: "mac"                   # §3.5：全局会话 id `<node>:<id>` 的前缀，安装时生成，改名需迁移
peers: []                     # §3.5：默认空。每项 { name, url, token_file, cert_fingerprint }；
                              #   本节点作为这些 peer 的 API 客户端并入其会话
runtime:                      # ADR-001 D3 / D6 / D7
  kind: tmux                  # V1 唯一实现；Windows 的 native supervisor 另立 ADR（ADR-001 D9）
  tmux:
    socket: "agora"           # 专用 socket（-L）：agora 创建的会话都在这里，用户 kill-server 杀不到
    adopt_sockets: ["default"]  # 只读扫描、可采纳；绝不对其写任何选项
    prefix: "ag-"
    history_limit: 10000      # 服务器级（-f）设置：3.7 以前的 tmux 对已存在的 pane 不生效（实测 2026-09-02）
    exec_timeout: "5s"        # 每次 tmux 子进程调用的超时（不变量 5）
    min_version: "3.2"        # new-session -e / window-size latest / respawn-pane -e / pane_dead_signal
terminal:
  scrollback: 10000           # xterm.js，与运行时 history_limit 对齐
status:
  idle_after: "60s"
  detector_interval: "2s"
notifications:
  enabled: true
tls:                          # §8：非 loopback 必须 HTTPS
  mode: "external"            # acme-dns | self-ca | external
project_roots:                # 扫描而非手写，按最近使用排序
  - "/Users/ricky/code"
worktree_root: "../{repo}-wt" # §6.4：新建 worktree 的存放约定，{repo} = 仓库目录名，worktree 名接在其下
agents:                       # 
  claude: { command: "claude" }
  codex:  { command: "codex" }
  grok:   { command: "grok" }
  pi:     { command: "pi" }
```

机器 token 由被访问的节点签发（§8），存在对方的 `token_file` 里；本节点只存哈希。浏览器一次只连一个节点，只记住最近打开的地址（不变量 6：可丢弃）。

## SQLite Schema（MVP）

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    runtime_ref TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    name_locked BOOLEAN NOT NULL DEFAULT FALSE,
    agent_type TEXT NOT NULL,
    working_directory TEXT,
    worktree TEXT,                                 -- 
    task_ref TEXT,                                 -- issue id 或摘要
    command TEXT,
    agent_session_id TEXT,                         -- agent 自报的当前对话 id（§5.6），Restart resume 依据
    created_at DATETIME NOT NULL,
    ended_at DATETIME,                             -- 进程退出时刻（§4.2）；等待时长与 attention 用，A42
    updated_at DATETIME NOT NULL,
    adopted BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE projects (path TEXT PRIMARY KEY, name TEXT NOT NULL, last_used_at DATETIME);  -- 扫描发现 + 最近使用
CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

MVP 不需要保存大量 operational telemetry。peer 的最后视图只在内存（重启后等待重连），不落库；持久化留 V2 peer 历史（§11）。迁移带版本号（§2.3 规则 10）。


## 机器 token 文件

- 由被访问的节点签发：`agora peer token create <name>`，输出一行 token；该节点只存 SHA-256 哈希，可按 name 吊销。
- 持有方写入 `peers[].token_file`：明文、`0600`、不进 git、不进日志（MISSION §8）。
