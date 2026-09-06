# 配置与存储

原则在 MISSION §9；本文是文件形态。`runtime` 段按 ADR-001、`hooks` 段按 ADR-002、`server` / `tls` / `auth` 段按 ADR-003 定稿。配置文件在 `AGORA_HOME/config.yaml`（默认 `~/.agora`，目录 0700，ADR-003 D6）。文件可以不存在（全部默认）；存在时**未知键即启动失败**——默默忽略一个拼错的 `server.listen` 比启动失败危险。时长一律 `<整数><s|m|h|d>`。core 层只认 `runtime.kind`，`runtime.<kind>` 子段原样交给选中的运行时实现解析（ADR-001 D2）。

## 配置文件（每个节点一份）

```yaml
server:                       # ADR-003 D5：两个监听器
  listen: "127.0.0.1:7680"    # 明文监听器：只允许 loopback 地址，配置校验拒绝其它
  tls_listen: null            # TLS 监听器：非 loopback、永远 TLS；被 peer 或手机访问时才开，例 "0.0.0.0:7681"（端口须不同于 listen）
  public_url: null            # 远端配对链接与 QR 用的对外地址，例 "https://zuan.tail6f613.ts.net:7681"；不自动猜
node:
  id: "mac"                   # §3.5：全局会话 id `<node>:<id>` 的前缀，安装脚本写短主机名（文末「安装」），改名需迁移；没有 config.yaml 时默认 "local"
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
worktree_root: "../{repo}-wt" # §6.4：新建 worktree 的存放约定（POST /api/projects/worktrees，A44）。相对路径相对主 worktree 所在目录解析，{repo} = 主 worktree 的目录名，worktree 名接在其下：~/code/agora + "h1k" → ~/code/agora-wt/h1k；绝对路径原样用；从 linked worktree 发起也按主 worktree 算
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
    killed_at DATETIME,                            -- 用户执行过 Kill 的时刻，Restart 清空；事件不是活性，重启后仍报 killed by user（v2，ADR-001 D4）
    ended_at_approximate BOOLEAN NOT NULL DEFAULT FALSE  -- ended_at 是 daemon 时钟补的近似值（运行时会话已不在 / 运行时还没报退出时刻），不是运行时报的退出时刻；准确值到了就覆盖近似值，Restart 清空（v5，A42，agora-h1k.4）
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

## 安装（`scripts/install.sh`；A26）

一条命令把一台机器装成 agora 节点（macOS 与 Ubuntu 24.04；POSIX sh，`set -eu`）。当前没有发布渠道，二进制由用户拷过去（zuan 是 x86_64：Mac 上 `cargo zigbuild --target x86_64-unknown-linux-gnu`），模板在 `scripts/templates/`，脚本按 `$(dirname "$0")/templates` 找它——所以要把整个 `scripts/` 目录一起拷。守卫 `tests/install_script.rs`（`install_writes_config_link_and_unit_idempotently`、`dry_run_writes_nothing`、`refuses_tmux_below_minimum`；两个 CI OS 都跑，不需要 root）。

```
scripts/install.sh --binary <path> [--home <dir>] [--node-id <id>] [--listen <addr>] [--tls-listen <addr>]
                   [--tmux-socket <name>] [--unit-dir <dir>] [--no-service] [--skip-tmux] [--dry-run]
```

| 旗标 | 默认 | 说明 |
|---|---|---|
| `--binary <path>` | 必填 | 要安装的 agora 二进制 |
| `--home <dir>` | `$AGORA_HOME`，再默认 `~/.agora` | AGORA_HOME（ADR-003 D6） |
| `--node-id <id>` | 短主机名（`hostname -s`，去掉域名，非法字符换成 `-`） | `node.id`。`--home` 不是默认路径（同机第二个实例）时默认 `<主机名>-<用户名>`，让 `<node>:<id>` 在 peer 眼里不撞；同机多个 OS 用户各装各的时请显式给 `--node-id` |
| `--listen <addr>` | `127.0.0.1:7680` | `server.listen`。未指定且 7680 已被占（`nc -z` / `ss` / `lsof` / bash `/dev/tcp` 探测，都没有就当空闲）→ 从 7681 起顺延到第一个空闲端口并在 stderr 说明；这只发生在**首次写 config.yaml** 时，daemon 自己仍是"端口被占就报错退出"（ADR-003 D6） |
| `--tls-listen <addr>` | 不写 | `server.tls_listen`（例 `0.0.0.0:7681`）；被 peer / 手机访问的节点才开。与 `--listen` 同端口在写文件之前就拒绝 |
| `--tmux-socket <name>` | 不写 | 写 `runtime.tmux.socket` 并把 `adopt_sockets` 置空——开发机上起第二个实例验证用（AGENTS.md「并行施工」的隔离规矩），正常安装不用 |
| `--unit-dir <dir>` | `~/.config/systemd/user` / `~/Library/LaunchAgents` | 单元文件目录；测试与开发机验证指到临时目录 |
| `--no-service` | | 只写单元文件，不 `enable` / `bootstrap` |
| `--skip-tmux` | | 只校验 tmux 版本，不安装；不满足则退出非零 |
| `--dry-run` | | 只在 stderr 打印将做的事，什么都不写 |

**步骤**（顺序即脚本顺序）：

1. **tmux ≥ 3.2**（ADR-001 D7）：`tmux -V` 按 major.minor **数值**比（`3.7c`、`3.2a`、`next-3.4` 都能解析，`10.0` 不会被当成比 `3.2` 小）。不满足：macOS `HOMEBREW_NO_AUTO_UPDATE=1 brew install tmux`，Ubuntu `sudo -n apt-get update && sudo -n apt-get install -y tmux`（`-n`：要密码就直接失败），装不了或装完仍旧版就把命令打给人、退出非零。
2. **`<home>`** 0700（已存在且权限过宽则收紧）；**`config.yaml` 只在不存在时写**（0600）：`server.listen`、可选 `server.tls_listen`、`node.id`、可选 `runtime.tmux`，其余键留默认。已有的配置一个字节都不动——改配置是人的事。
3. **二进制**：先算 sha256，放到 `<home>/versions/<sha 前 12 位十六进制>/agora`（0755；先写 `.tmp` 再 rename；已存在同 sha 的就复用），再 `ln -sfn` 成 `<home>/bin/agora`（目标写绝对真实路径，`pwd -P` 解析过符号链接）。`bin/agora` 是 hook 命令与 Codex 内容哈希信任依赖的稳定路径（ADR-002 D4、ADR-003 D6）；升级只换链接目标（`agora upgrade`，agora-7ku.8）。链接已指向同一目标就不重做。
4. **随登录自启**，单元里写 `LANG=C.UTF-8`（devcenter CJK 乱码教训的另一半，ADR-001 D7）、`AGORA_HOME`、够找到 tmux 与二进制的 `PATH`（`~/.local/bin:/usr/local/bin:/usr/bin:/bin`，macOS 多一个 `/opt/homebrew/bin`；tmux 装在这些目录之外就把它所在目录也加上；完整 PATH 由 daemon 启动时自己探测）。内容没变就不重写文件。
   - Linux：`~/.config/systemd/user/agora.service`（模板 `scripts/templates/agora.service`：`ExecStart=<home>/bin/agora serve`、`Restart=on-failure`、`WantedBy=default.target`），非 `--no-service` 时 `systemctl --user daemon-reload && systemctl --user enable --now agora.service`；没有 `systemctl`（容器）只写文件、说一句、不报错。**linger**：`loginctl show-user $USER -p Linger` 不是 `yes` 就提示 `sudo loginctl enable-linger $USER`（用户单元随登录起、随登出停，不开 linger 重启后要等人登录 daemon 才起）——脚本不自己 sudo，装软件与改系统设置的每一步都由人敲。`/etc/systemd/logind.conf`（含 `logind.conf.d/*.conf`）里 `KillUserProcesses=yes` 时警告：登出会杀掉 daemon 与 tmux server。
   - macOS：`~/Library/LaunchAgents/dev.agora.daemon.plist`（label `dev.agora.daemon`，模板 `scripts/templates/dev.agora.daemon.plist`：`ProgramArguments <home>/bin/agora serve`、`RunAtLoad`、`KeepAlive` 仅 `SuccessfulExit=false`——非零退出才拉起，等价 `Restart=on-failure`；stdout / stderr 落 `<home>/daemon.log`），非 `--no-service` 时 `launchctl bootstrap gui/$(id -u) <plist>`，已加载则 `launchctl kickstart -k`。
5. **sshd**（Linux）：`systemctl is-active ssh|sshd` 都不活跃就提示装 `openssh-server`——ADR-003 的兜底是"ssh 上去 `agora pair`"，没有 sshd 就只能物理登录后配对；脚本只提示不装。
6. 结尾打印下一步：`<home>/bin/agora url`。

**幂等**：重跑退出 0，config.yaml / versions 里的二进制 / 单元文件 mtime 与内容都不变，链接不重做（守卫 `install_writes_config_link_and_unit_idempotently`）。**输出纪律**：给人看的话一律走 stderr，stdout 留给将来机器可读的输出（MISSION §2.3 规则 10）。

目录布局（安装后）：

```
<AGORA_HOME>/
  config.yaml                 # 首次安装写；之后人改
  bin/agora -> versions/<sha12>/agora   # 稳定路径；hook 命令、Codex 哈希信任、升级都指它
  versions/<sha12>/agora      # 每个装过的版本一份，按内容 sha256 前 12 位命名
  agora.db / agora.sock / tls/ / hooks/ / tmux.conf   # daemon 运行时自建（ADR-003 D6）
```

**zuan 实机（用户在场的专场；A26 的实机半边）**：Mac 上 `CARGO_BUILD_JOBS=2 cargo zigbuild --release --target x86_64-unknown-linux-gnu`，`scp -r scripts target/x86_64-unknown-linux-gnu/release/agora zuan:~/agora-install/`；zuan 上 `cd ~/agora-install && ./scripts/install.sh --binary ./agora --tls-listen 0.0.0.0:7681`（tmux 不满足时脚本会 `sudo -n apt-get`，sudo 要密码就按它打印的命令手动装）→ `systemctl --user is-enabled agora.service` 应为 `enabled`、`is-active` 为 `active` → 按提示 `sudo loginctl enable-linger $USER` → `sudo reboot` → 重新 ssh 上去 `~/.agora/bin/agora url` 可达、起一个会话跑 `printf '中文\n'`，`tmux -L agora capture-pane -p` 里是 `中文` 的 UTF-8 字节而不是问号。Mac 与 ubuntu:24.04 容器里的同一套断言已由实施 agent 跑过（2026-09-06，见 agora-7ku.1 notes）。

## 升级（`agora upgrade`；A39）

一条命令升级本节点（MISSION §2.3 规则 10：N 个节点各自原地升级；agora-7ku.8）。分工：安装脚本（agora-7ku.1）装运行时与自启单元，`agora upgrade` **只换 agora 自己**（ADR-001 D7），两边共用下面的路径约定，谁都不许改形态：

| 约定 | 值 |
|---|---|
| 二进制 | `<AGORA_HOME>/versions/<sha256 前 12 位十六进制>/agora`（0755，按内容哈希存放，同一份只放一次） |
| 稳定路径 | `<AGORA_HOME>/bin/agora`：指向上面那份的符号链接（目标写绝对路径、经 canonicalize；hook 命令与 Codex 的按条哈希信任都依赖这条路径不变，ADR-002 D4）。开发机上指向 `target/debug/agora` 是允许的特例 |
| systemd 用户单元 | `agora.service`（`~/.config/systemd/user/agora.service`：ExecStart 是 `<AGORA_HOME>/bin/agora serve`，Environment=LANG=C.UTF-8，Restart=on-failure，WantedBy=default.target） |
| launchd | label `dev.agora.daemon`（`~/Library/LaunchAgents/dev.agora.daemon.plist`：ProgramArguments `<AGORA_HOME>/bin/agora serve`，EnvironmentVariables 含 LANG=C.UTF-8 与够用的 PATH，RunAtLoad + KeepAlive） |
| pid 文件 | `<AGORA_HOME>/agora.pid`（0600，十进制 pid + 换行）：`serve` 三个监听器都绑上之后写，正常退出与 SIGTERM 收尾时删（只删内容仍是自己 pid 的那份）。只给 upgrade 与人看；单实例判定仍是先绑监听器、socket 探活（agora-apr），不读它 |

**命令**

- `agora upgrade --from <新二进制路径> [--no-restart]`：升级。人话全走 stderr；stdout 留给将来机器可读的形态。
- `agora upgrade --probe`：**新**二进制自报能力——stdout 一行 JSON `{"schema_version": <本程序能打开的最高 user_version>, "api_version": "<major>.<minor>"}`，退出 0；不读配置、不碰 `AGORA_HOME`，被问的是这份二进制本身。这是给 upgrade 读的（规则 10：程序读 JSON，不读人话）；旧到没有这个子命令的二进制会打印用法、退出 2，upgrade 按"probe 跑不起来"处理。

**`--from` 的步骤**（`src/cli/upgrade.rs`）

1. 读 `<from>` 算 SHA-256，复制到 `versions/<sha 前 12 位>/agora`（先写 `.part` 再 rename；同 sha 已在就复用）。`<from>` 可以是链接本身、也可以是正在跑的那份——按内容放置，没有"自己复制自己"的坑。
2. 以那一份跑 `upgrade --probe`（≤ 10 s），解析 JSON；再**只读**打开 `agora.db` 读 `PRAGMA user_version`（不经 `Db::open`——那会先把库迁到本程序的版本）。`schema_version` < 库的 `user_version` → 退出码 2「新版本不认识这个库」，链接不动、daemon 不动、本次放进 `versions/` 的副本删掉。这是规则 10 的**前向**守卫：升级到一个更老的二进制不会让它读错新库；运行期的**后向**守卫是 `DbError::TooNew`（旧程序打开更新的库拒绝启动，守卫 `tests/schema.rs::newer_database_is_refused_not_downgraded`）。迁移带版本号、只前进（`src/session/db.rs`）。
3. 重指 `<AGORA_HOME>/bin/agora`：与 `agora hooks install` 同一个 `hook::install::ensure_bin_link`，exe 传 canonicalize 后的真路径。hook 条目里的命令是这条稳定路径，条目内容不变，升级后**不必**重新 `hooks install` / 重新在宿主里信任。
4. 重启 daemon（`--no-restart` 跳过，"daemon 下次启动即新版本"），按顺序探测、命中即用：
   1. `systemctl --user is-active agora.service` 答 `active` → `systemctl --user restart agora.service`；
   2. macOS 上 `launchctl print gui/<uid>/dev.agora.daemon` 成功 → `launchctl kickstart -k gui/<uid>/dev.agora.daemon`；
   3. 都不是 → 读 `agora.pid`：进程活着（`kill(pid, 0)`）就 SIGTERM、等它退出（≤ 10 s；超时报错**不 SIGKILL**，它可能正在收尾），再经 `sh` 以 `<AGORA_HOME>/bin/agora serve` 起新的——stdin `/dev/null`、stdout+stderr 追加到 `<AGORA_HOME>/daemon.log`、`AGORA_HOME` 显式传入；有 `setsid`（Linux 的 util-linux）就开新会话彻底脱离终端，没有（macOS 默认没有）就只放后台——macOS 的正常路径是 launchd，这一支是开发机与测试的兜底。pid 文件不在或进程不在 → 只重指链接，"daemon 未在运行，下次启动即新版本"。
5. 轮询 `GET http://<server.listen>/api/health`（公开子集）到 200 `{"status":"ok"}` **且** pid 文件里换成了新 pid（≤ 15 s；systemd / launchd 重启时旧进程可能还在答最后几个请求，只看 200 会把旧的当新的），最后打印 `<旧目标> → <新目标>，daemon pid <新>`。

升级窗口里 agent 不死：agent 挂在运行时之下、不挂在 daemon 之下（MISSION §3.4），新 daemon 起来 reconcile 找回全部会话，期间 hook 事件落投递箱、起来后重放。

**退出码与状态**

| 情形 | 退出码 | 链接 | daemon |
|---|---|---|---|
| 用法错误 | 2 | 不动 | 不动 |
| 新版本不认识这个库（probe 的 `schema_version` < 库的 `user_version`） | 2 | 不动 | 不动 |
| probe 跑不起来 / 非 0 退出 / 输出不是 JSON | 1 | 不动 | 不动 |
| 复制或重指失败（IO） | 1 | 错误里说明 | 不动 |
| 旧 daemon 10 s 内没退出、`systemctl` / `launchctl` 失败、起新的失败 | 1 | **已重指** | 仍是旧版本（错误文本说明；手动停掉再起 `agora serve`） |
| 重启后 15 s 内没有以新 pid 答 200 | 1 | **已重指** | 看 `<AGORA_HOME>/daemon.log` |

**经链接跑也安全**（agora-78f）：`~/.agora/bin/agora upgrade --from …` 这样经链接调用时，macOS 的 `current_exe()` 拿到的是链接本身；upgrade 根本不看 `current_exe`——新二进制按内容放进 `versions/`、按真路径重指，`ensure_bin_link` 的硬守卫另外保证任何情况下都造不出 link → link。

已知盲点（2026-09-06）：pid 文件那一支用 `kill(pid, 0)` 判活，僵尸也算活着——旧 daemon 的父进程不收尸时会等满 10 s 再报错；launchd / systemd / 交互 shell / sshd 都立刻收尸，`tests/upgrade.rs` 起的 daemon 由测试自己另起线程 wait。

守卫：`tests/upgrade.rs::daemon_restart_keeps_agents_sessions_and_metadata`（十个 fake-agent 会话，换二进制路径 + 重启：链接真路径 == `versions/<sha12>/agora`、新 pid ≠ 旧 pid、同样十行 id / name / status 仍 running、pane pid 一个没变）、`::bin_link_repointed_and_hooks_still_deliver`（升级后经链接跑 `hook`，SessionStart 让 external 行出现在 `GET /api/sessions`）、`::migration_versioned_and_older_daemon_refuses`（库的 `user_version` == 程序自报的 `schema_version`；一个只认识 schema 1 的假"新版本"被拒：退出 2、链接与 daemon 不动、`versions/` 里不留它）。
