# 配置与存储

原则在 MISSION §9；本文是文件形态。`runtime` 段按 ADR-001、`hooks` 段按 ADR-002、`server` / `tls` / `auth` 段按 ADR-003 定稿。配置文件在 `AGORA_HOME/config.yaml`（默认 `~/.agora`，目录 0700，ADR-003 D6）。文件可以不存在（全部默认）；存在时**未知键即启动失败**——默默忽略一个拼错的 `server.listen` 比启动失败危险。时长一律 `<整数><s|m|h|d>`。core 层只认 `runtime.kind`，`runtime.<kind>` 子段原样交给选中的运行时实现解析（ADR-001 D2）。

## 配置文件（每个节点一份）

```yaml
server:                       # ADR-003 D5：两个监听器
  listen: "127.0.0.1:7680"    # 明文监听器：只允许 loopback 地址，配置校验拒绝其它
  tls_listen: null            # TLS 监听器：非 loopback、永远 TLS；被 peer 或手机访问时才开，例 "0.0.0.0:7681"（端口须不同于 listen）
  public_url: null            # 远端配对链接与 QR 用的对外地址，例 "https://zuan.tail6f613.ts.net:7681"；不自动猜
node:
  id: "mac"                   # §3.5：全局会话 id `<node>:<id>` 的前缀，安装时生成，改名需迁移；安装脚本落地前默认 "local"
peers: []                     # §3.5：默认空。每项 { name, url, token_file, cert_fingerprint: "sha256:<SPKI hex>" }（ADR-003 D3 / D4）；
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
hooks:                        # ADR-002 D1 / D5 / D3
  silence_after: "10m"        # 有 hook 的 agent 无事件超过此时长且屏幕像在等人 → UNKNOWN（hook 沉默规则）
  unheard_after: "90s"        # 装了 hook 却一条事件没收到过、终端在启动 10 s 宽限后又活动了这么久 → 行上"hook 没接上"提示
  hold_timeout: "55m"         # 挂起的权限决定的上限；安装到 agent 配置里的 hook timeout 必须大于它
  hold_per_session: 8         # 并行工具调用可同时产生多个 PermissionRequest
  hold_per_node: 256
  inbox_retention: "24h"      # 已应用的事件文件在 done/ 保留时长
notifications:
  enabled: true
tls:                          # ADR-003 D4；agora 永远自己终止 TLS，不支持 HTTP 终止型反向代理
  mode: "self-signed"         # self-signed（默认：首次开 tls_listen 时生成 10 年自签证书到 tls/）| external；self-ca 未实现
  external:
    cert_file: null
    key_file: null
    renew_command: null       # 例 ["tailscale", "cert", "--cert-file", "…", "--key-file", "…", "zuan.tail6f613.ts.net"]
    renew_before: "720h"      # 到期前多久调用 renew_command；证书文件变化即热加载，SPKI 变了则警告 peer 需更新指纹
auth:                         # ADR-003 D2
  pair_ttl: "5m"              # 配对链接有效期；单次使用
  pair_pending_max: 4         # 同时未用的配对链接上限
  session_idle: "30d"         # 距最近使用
  session_max: "365d"         # 距配对
project_roots:                # 扫描而非手写，按最近使用排序
  - "/Users/ricky/code"
worktree_root: "../{repo}-wt" # §6.4：新建 worktree 的存放约定，{repo} = 仓库目录名，worktree 名接在其下
agents:                       # Adapter 默认命令的覆盖（§5.2）；存可移植形式，不写绝对路径（ADR-001 D7）
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
    runtime_ref TEXT UNIQUE,                       -- origin = external 时为 NULL（§5.5）
    display_name TEXT NOT NULL,
    name_locked BOOLEAN NOT NULL DEFAULT FALSE,
    agent_type TEXT NOT NULL,
    working_directory TEXT,
    worktree TEXT,                                 -- git worktree 路径；可空（§4.2）
    task_ref TEXT,                                 -- issue id 或摘要；为空时首条 prompt 的首行补上（ADR-002 D8）
    command TEXT,
    agent_session_id TEXT,                         -- agent 自报的当前对话 id（§5.6），Restart resume 依据
    epoch INTEGER NOT NULL DEFAULT 1,              -- 进程代次：create 为 1，每次 respawn +1；旧代次的 hook 事件丢弃（ADR-002 D1）
    transcript_path TEXT,                          -- agent 自报的 transcript 路径；V1 只存不读（ADR-002 D8）
    created_at DATETIME NOT NULL,
    ended_at DATETIME,                             -- 进程退出时刻（§4.2）；等待时长与 attention 用，A42
    updated_at DATETIME NOT NULL,
    origin TEXT NOT NULL DEFAULT 'agora',          -- agora | adopted | external（§5.5）
    spawned_at DATETIME,                           -- 本代进程（epoch）起始时刻：create / respawn 写；STARTING 窗口只看它（v2）
    killed_at DATETIME                             -- 用户执行过 Kill 的时刻，Restart 清空；事件不是活性，重启后仍报 killed by user（v2，ADR-001 D4）
);
CREATE TABLE projects (path TEXT PRIMARY KEY, name TEXT NOT NULL, last_used_at DATETIME);  -- 扫描发现 + 最近使用
CREATE TABLE devices (                             -- ADR-003 D2：已配对设备，人的 session；只存哈希
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,                            -- 由 User-Agent 生成，可改名
    session_sha256 TEXT NOT NULL UNIQUE,
    paired_via TEXT NOT NULL,                      -- socket | session
    paired_from_addr TEXT,
    created_at DATETIME NOT NULL,
    last_seen_at DATETIME NOT NULL,                -- 每小时至多写一次
    revoked_at DATETIME
);
CREATE TABLE peer_tokens (                         -- ADR-003 D3：按 peer 签发的机器 token；只存哈希
    name TEXT PRIMARY KEY,
    token_sha256 TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    last_used_at DATETIME,
    revoked_at DATETIME
);
CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

MVP 不需要保存大量 operational telemetry。peer 的最后视图只在内存（重启后等待重连），不落库；持久化留 V2 peer 历史（§11）。迁移带版本号（§2.3 规则 10）。


## 机器 token 文件

- 形态 `apt_<name>_<base64url(32 字节随机)>`（一行，随机段恰好 43 字符）：前缀让日志与 secret scanner 认得出它，`<name>` 是被访问节点眼里这个 peer 的名字（字符集与 `node.id` 相同：字母、数字、`-`、`_`，最长 64）。name 与随机段都可能含 `_`，解析按尾部定长 43 切，不按 `_` 切（`src/auth/peer_token.rs`）。
- 由被访问的节点签发：`agora peer token create <name>`——stdout **只有 token 一行**（`> mac.token` 直接就是 token_file），提示走 stderr；明文只输出这一次，该节点的 `peer_tokens` 表（schema v4）只存整串的 SHA-256。已有有效 token 的 name 再 `create` 拒绝（退出 1），加 `--rotate` 才换新——同一行换哈希，旧 token 立即失效；已吊销的 name 直接重签、不需要 `--rotate`。`agora peer token list` 列出 name / 签发时刻 / 最近使用 / active|revoked（不显示哈希，更没有明文）；`agora peer token revoke <name>` 即时生效（daemon 每次请求查库、不缓存）。三条命令直接操作 `AGORA_HOME/agora.db`，不需要 daemon 在跑（ADR-003 D6）；库文件若由 CLI 首次创建也只属主可读。签发没有前置条件（ADR-003 D3）；token 只在 TLS 监听器上被接受（`docs/spec/api.md`「认证」）。
- 持有方写入 `peers[].token_file`：明文、`0600`、不进 git、不进日志（MISSION §8）。读它（`peer_token::load_token_file`）先查权限再读内容：文件必须属于当前 uid、group / other 不得有任何位（与 `AGORA_HOME` 自检同一尺度）、内容去掉首尾空白后必须是上面的形态——任何一条不满足都是**配置错误**（`TokenFileError`），该 peer 应显示为「配置错误」而不是"离线"或"未授权"，也不进退避重试（重试改不了文件权限）。守卫 `tests/peer_token.rs::token_file_too_open_is_config_error`。 peer 客户端每次请求时读它、不缓存（换文件即生效）；配置错误时一个字节都不会发出去。

## TLS 证书（`tls` 段；ADR-003 D4 / D5）

agora 永远自己终止 TLS。`server.tls_listen` 一配，监听器上就只有 TLS，握手不成的连接直接关闭，没有"按明文继续"的分支（守卫 `tests/listen.rs::tls_listener_never_serves_plaintext`）；`server.listen` 反过来只接受 loopback 地址。两个模式最后都归到一对 PEM 文件，daemon 与 `agora tls …` 看到的是同一对：

| `tls.mode` | 证书从哪来 | `agora tls rotate-key` |
|---|---|---|
| `self-signed`（默认） | 首次开 `tls_listen`（或首次 `agora tls fingerprint`）时生成 ECDSA P-256 私钥与 10 年自签证书到 `AGORA_HOME/tls/key.pem` / `cert.pem`（0600，目录 0700，先写 `.tmp` 再 rename），之后**复用**——指纹稳定是 peer 配置成立的前提；只剩半对（手动删了一个）当作没有，重新生成一对 | 换钥重签，指纹变，peer 要改 |
| `external` | `tls.external.cert_file` 与 `key_file` 都必填（PEM；证书可带链，第一张是叶子）。agora 不校验它是谁签的、主机名对不对——peer 只认 SPKI 指纹，浏览器认 CA 是浏览器的事。`renew_command` 是 argv 直传不经 shell，距 notAfter 不到 `renew_before` 时调用（上限 120 s），失败一小时后再试；新文件由热加载装上 | 拒绝：密钥归外部工具管 |

- **指纹**：`sha256:<64 位小写 hex>`，是叶子证书 SubjectPublicKeyInfo（DER）的 SHA-256——同一把钥重签不变，换钥才变。`agora tls fingerprint` 打印它（stdout 只有这一行，能直接粘进对方 YAML；自签模式下证书还没生成就先生成，不必先起 daemon），daemon 启动日志也打。别的节点把本节点配成 peer 时 `peers[].cert_fingerprint` 填它。
- **peer 客户端只比指纹**：不看 CA、主机名、有效期；没有 TOFU——`cert_fingerprint` 为空或不是 `sha256:<64 hex>` 是配置错误，连 TCP 都不拨；对端指纹对不上是独立的「指纹不匹配」状态，绝不并进"离线"（守卫 `tests/peer_tls.rs::fingerprint_mismatch_is_refused_not_stale`、`::no_pin_no_connect`）。`peers[].url` 必须是 `https://host[:port]`（缺省 443；IPv6 写 `[fd00::1]:7681`），`http://` 是配置错误。
- **热加载**：两个模式都由 daemon 每 30 s 比对两个文件**内容**的哈希（不是 mtime），变了就装上新证书，daemon 不重启、已建立的连接不断；半写或钥证不配就拒绝、旧证书继续服务，文件再变再试。SPKI 变了（换钥）打 warn 日志"每个把本节点配成 peer 的节点都要更新 peers[].cert_fingerprint"，只续签不换钥则 peer 不用动（守卫 `tests/peer_tls.rs::external_cert_hot_reload_warns_on_spki_change`）。
- **零凭据只警告**：开了 `tls_listen` 却既没有机器 token 也没有已配对设备时，daemon 启动打 warn（"没有人能连进来"）而不是拒绝启动——没有直通路由，这个状态只是没用不是漏洞；此时 TLS 端口上是 401（守卫 `tests/listen.rs::tls_listen_without_credentials_warns_not_refuses`）。
- `self-ca` 未实现；`server.public_url` 不自动猜。
