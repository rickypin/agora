# ADR-003: 节点认证模型

- 状态：**Accepted**（2026-09-02；草案经第一性原理复审后修订，用户采纳全部修订：设备配对替代 TOTP、A34 降为警告、A31 删签发前置条件、证书默认 external、`AGORA_HOME = ~/.agora`）
- 日期：2026-09-02
- beads：`agora-90t.4`
- 依赖：MISSION §0.1（多用户是 Non-Goal；同机多用户多实例互为陌生人）、§0.2 / `docs/spec/instance.md`（tailnet 里有他人的节点）、§2.2 不变量 9 / 11、§3.5（peer 模型，ADR-004）、§8；ADR-002 D3（hook 投递不经 HTTP）

## Context

agora 本质上是 Remote Shell Access：`POST /api/sessions` 的 `command` 等于以安装用户身份任意执行命令，安全等级等价于 SSH。它要认证的连接有四种：

| 连接 | 谁发起 | 走什么 | V1 是否存在 |
|---|---|---|---|
| 本机浏览器 → 本机节点 | 人 | loopback HTTP | 是（唯一的人机入口） |
| 远端浏览器 / 手机 → 节点 | 人 | 非 loopback HTTPS | 否（V2-1 手机阶段） |
| 节点 → peer 节点 | 节点（作为 API 客户端） | 非 loopback HTTPS | 是（Mac → zuan） |
| agent 的 hook / CLI → 本机 daemon | 本机同一 OS 用户 | unix socket | 是 |

本文从五条第一性事实推导，每个决策都能回指到其中一条：

1. **任何已认证 principal = 以安装用户身份开 shell**。安全目标只有一句：只有主人（和替主人干活的节点）能到达 API。
2. **零成本的信任锚只有三个**：同机 OS 用户身份（文件权限、socket 对端 uid）、主人随身携带的设备、对方节点的公钥指纹。其它机制都是在这三者之上搭桥。
3. **浏览器只会说 TCP，不能出示 OS 身份**。所以必须有一条"OS 身份 → 浏览器"的桥，且桥本身是一个只有该用户拿得到的秘密。
4. **网络上唯一保护字节的手段是 TLS**。凭据一旦明文上网就等于公开。
5. **若没有任何路由能在无 principal 时被服务，那么"监听在网络上但没配凭据"不是漏洞，只是没用**。

限定解空间的既有事实：

- **devcenter 的教训**（报告 §3.5 第 5 条、§4.5、附录 B）：hub↔node 是 `ssh -N -L` 隧道 + 节点侧零认证，hub 本机任何进程都能在所有节点以用户身份执行任意命令；作者把修补推迟到"0600 unix socket"。它的浏览器→hub 一跳用 TOTP + cookie + 阶梯限流，做得扎实，但那套限流的存在理由是 TOTP 只有一百万个码（见备选项）。
- **loopback 不是安全边界**：MISSION §0.1 允许同一主机多个 OS 用户各跑各的实例；zuan 本来就是多人共用的主机。devcenter 与 MISSION v0.8 都把"只绑 loopback 时可不配置认证"当前提，这个前提在本项目不成立。
- **网络可达 ≠ 授权**（不变量 11）：tailnet 里有他人共享的节点与 tagged 设备。
- **hook 投递已不经 HTTP**（ADR-002 D3），本文只需定本机通道与目录的权限模型。
- **单人**，认证按 principal 留口（§0.1）；**Thin**（§2.1）：单 binary，证书、token 都由 agora 自己发，不引入 OAuth、账号体系、KMS。

## 决策问题

节点之间以及客户端到节点，以什么身份模型、什么凭据互认；本机通道在多用户主机上如何隔离；单人多机与多人各需要什么。

## 备选项

| 备选 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| ssh 隧道即信任 | 零证书、零 token | 节点侧零认证，隧道端口就是后门 | 否决（报告 §6.3） |
| 网络层信任（tailnet ACL / Cloudflare Access） | 零代码 | 把授权交给部署；tailnet 里有陌生人；违反不变量 11 | 否决（devcenter ADR-006 的 MVP 做法，后来还是补了 TOTP） |
| **机器 token（Bearer）+ 服务端证书指纹钉住**（节点间） | 一行配置一个 peer；对任何 TLS 终止拓扑透明；实现是一个请求头 | token 是要传输、要存储的秘密；持有方被攻破 = 控制其 peer | **选定** |
| 钉 SPKI 的 mTLS（无 CA） | 秘密永不出节点；配置里只有公开的指纹，双向对称 | 客户端证书插件在 rustls 两端各一套；任何在 agora 之外终止 TLS 的东西都会打断它。**被否决的真实原因是收益小于这套插件的成本，不是"要 CA、要吊销"——钉指纹的 mTLS 不需要 CA** | 否决；若将来禁止一切外部 TLS 终止，可重开 |
| CA 型 mTLS | 传输层双向身份 | 要 CA、分发、轮换、吊销；对 2–3 台机器是净负担 | 否决 |
| principal-based 授权（用户 × 节点 × 动作） | 多用户与审计的前提 | 单人时是空转的框架 | 否决，只留 `Principal` 类型的口 |
| **设备配对**：一次性链接（256 位、单次、5 分钟）+ 按设备的长期 session（人） | 没有可猜的秘密、没有重放窗口、没有锁定 DoS、不需要 authenticator；本机与远端同一机制 | 手上没有任何已配对设备时无法远程重新登录 | **选定** |
| TOTP + 服务端 cookie session（人） | 无账号体系；随时可用 authenticator 重新登录；devcenter 实现可直接沿用 | 码空间 10^6：安全性完全压在限流上；限流本身引入"别人能把主人锁在门外"这一类攻击；阶梯锁定 / 全局兜底 / 受信网段豁免 / 可信代理 / 来源表淘汰全是为它付的账 | 否决：它唯一的优势（无配对设备时远程登录）不值这整套复杂度；单人 + 滑动续期 + ssh 兜底下损失可接受 |
| Passkey / WebAuthn（人） | 抗钓鱼 | RP ID 绑定域名，与"任意网络、自签证书"冲突；实现面大 | 否决 |
| 常驻 local token 文件（本机） | 不依赖 daemon 在跑 | 多一个静态秘密；与远端是两套机制 | 否决：本机也走配对，链接经 socket 向 daemon 铸造 |

## Decision

### D1 principal 模型：每个请求先解析出一个 Principal，没有例外路由（事实 5）

- 两种 principal：`Human { device }`（人，带已配对设备 id）与 `Peer { name }`（节点作为 peer 客户端）。V1 单人，`Human` 不带用户名；多用户只需加字段，handler 不变——这就是 §0.1 说的"留口"。
- `Principal` 是 axum 提取器，**每个** handler 的签名都要它。未认证白名单只有：SPA shell 与静态资源、`GET /api/health` 的公开子集（只有 `{ "status": "ok" }`）、`POST /api/auth/pair`。
- 带 `Authorization: Bearer` 的请求按 D3 解析，否则按 D2 找 cookie；都没有 → 401。
- 授权在 V1 只有一条规则：任一 principal 对本节点全部 API 全权——Remote Shell Access 的诚实写法。每条写请求的日志行带 principal（`human:<device>` / `peer:mac`），转发时两跳各记各的。
- **代码里没有 loopback 例外分支**：多用户主机上的后门就是从"本机调试方便"的例外长出来的。

### D2 人的凭据：设备配对，本机与远端同一机制（事实 2、3）

**配对链接**：

- 32 字节随机、base64url、**单次使用、5 分钟过期**、节点上同时未用的链接最多 4 条。形态 `<origin>/#pair=<token>`：token 在 fragment 里，不进服务器日志、不进 Referer；前端读到后 `POST /api/auth/pair { token }` 换 cookie，再清掉 fragment。
- 只有两个地方能铸造它：① 本机 CLI 经 unix socket（D6 的对端 uid 校验就是 OS 身份）：`agora open`（铸造 + 打开默认浏览器，origin 为 `http://127.0.0.1:<port>`）、`agora url`（只打印）、`agora pair`（origin 为 `server.public_url`，终端打印 QR）；② 已认证的 Human session：Dashboard "配对新设备"（V2-1），同样用 `public_url`。未认证的请求无法铸造——这是配对模型成立的前提。
- 配对端点**不需要限流**：256 位没有暴力猜测的余地。每次失败（未知 / 已用 / 过期）记日志含来源地址；已用的链接再次出现是"有人偷看了链接"的信号。

**session**：

- 配对成功 → 发放 32 字节随机 session token；SQLite 存 `devices (id, name, session_sha256, paired_via: socket|session, paired_from_addr, created_at, last_seen_at, revoked_at)`；`name` 从 User-Agent 生成、可改名。
- 过期：**距最近使用 30 天**（滑动）且**距配对不超过 365 天**（绝对上限）。`last_seen_at` 每小时至多写一次。
- cookie `agora_session`：`HttpOnly; SameSite=Lax; Path=/; Max-Age=<session_idle 秒数>`；经 TLS 监听器发放时加 `Secure`（`127.0.0.1` 与远端主机名是不同 origin，cookie 罐互不干扰）。WS 握手是同站 GET，cookie 自动携带。
- **`Max-Age` 不可省，且要跟着上一条的滑动窗口一起推。** 上面那句"距最近使用 30 天"是**两个**窗口：服务端按 `last_seen_at` 判过期，浏览器按 cookie 的 `Max-Age` 决定还留不留这条 cookie。不发 `Max-Age` 就是 session cookie，浏览器一关就丢，服务端的 30 天判得再对也没有凭据可判（agora-z8b：本 ADR 早先版本就漏写了这一段，实现照着写，于是文档与代码一致地错）。只在配对时发一次也不行：那是"距配对 30 天"，天天在用的人第 31 天照样被浏览器丢掉。因此每次刷新 `last_seen_at`（每小时至多一次）时随响应重发一遍 cookie。
- 吊销：`agora auth devices` / `agora auth revoke <device-id | --all>`；`POST /api/auth/logout` 只删当前设备。Dashboard 设备列表随 V2-1。
- 接受的代价：所有已配对设备都失效时（全部过期、或手机丢了而笔记本又不在身边），只能回到节点本机或 ssh 上去 `agora pair`。滑动 30 天让这种情况在正常使用下不出现。

### D3 节点凭据：按 peer 签发的机器 token，被访问方只存哈希（事实 1、4）

- 形态：`apt_<peer name>_<base64url(32 字节随机)>`。前缀让日志与 secret scanner 认得出它；name 段让服务端按名字查一条记录再做常量时间比对。
- 签发：在**被访问**的节点上 `agora peer token create <name>`，明文只打印这一次；SQLite 存 `(name, sha256, created_at, last_used_at, revoked_at)`。`list` / `revoke <name>` / `create <name> --rotate`（新的生效、旧的立即吊销；单用户接受短暂断连）。
- **没有签发前置条件**：token 在节点对外监听之前签出来没有任何危害——它用不了。MISSION v0.8 要求"签发时必须已在非 loopback 监听且已配置 TLS"，目的是保证 token 只在 TLS 上用，而这由 D5 结构性保证，不由签发时机保证；前置条件只会制造"启动前要有凭据、凭据要启动后签"的鸡生蛋。A31 后半句据此改写。
- **Bearer 只在 TLS 监听器上被接受**：明文监听器（D5）收到 `Authorization` 头一律 401 `bearer_requires_tls`。
- 持有方：`peers[].token_file` 明文、`0600`、不进 git、不进日志；启动时校验权限，过宽则该 peer 显示为"配置错误"而非离线。
- 验证：查 name → 常量时间比对 → 未吊销 → `Peer { name }`。**没有签发过任何 token 的节点拒绝一切 Bearer**（A31）；吊销即时生效（每请求一次索引命中，不缓存）。
- peer 不能再签发 token、不能铸造配对链接、不能改配置：没有委托链。
- 已知代价（单用户接受）：持有 peer token 的节点被攻破 = 能控制那些 peer；止损是按 peer 吊销，这也是 token 必须按 peer 单独签发的原因。

### D4 传输：peer 链路只认指纹；证书来源两种模式，浏览器可信的默认 external（事实 2、4）

**peer 链路（V1 就有）**：

- 连接方按 `peers[].cert_fingerprint = "sha256:<hex>"` 钉住对方 **SPKI**（公钥）的 SHA-256；只比对指纹，**不看 CA、主机名、有效期**——信任就是这一个指纹，像 ssh 的 host key。钉 SPKI 而非整张证书，是让"换证书不换密钥"（续期）不打断 peer。
- **没有 TOFU**：指纹必须来自配置；不匹配 → 该 peer 显示为"指纹不匹配"，与"离线 / stale"是不同状态，绝不自动接受。`agora tls fingerprint` 在被访问节点打印当前指纹；`agora tls rotate-key` 换密钥并列出需更新的 peer。
- 没有钉住，LAN 上的中间人拿到 Bearer 明文就是全权（devcenter 的 token 明文走 LAN TCP，附录 B §58）。

**证书来源 `tls.mode`**：

| 模式 | 做什么 | 谁需要 |
|---|---|---|
| `self-signed`（默认） | 首次开 TLS 监听器时自动生成密钥 + 10 年自签证书到 `<AGORA_HOME>/tls/`；零配置 | 只被 peer 访问的节点（V1 的 zuan） |
| `external` | 用户给 `cert_file` / `key_file`，可选 `renew_command`（到期前 `renew_before` 调用）；文件变化即热加载；SPKI 变了则日志警告"peer 需更新指纹" | 被浏览器直接打开的非 loopback 节点（V2-1 的 zuan） |
| `self-ca` | agora 自建根 CA、签发各节点证书、一条命令导出根证书装到手机 | **未实现**；只在 `external` 不可用（无域名、无 tailscale 的私网）时评估（`agora-thc.2`） |

- **浏览器可信证书的默认路径是 `external`**。浏览器的信任库是外部约束：零安装的唯一办法是公共 CA 的证书，公共 CA 需要公共名字。当前实例的名字来自 tailnet，`tailscale cert <node>.tail6f613.ts.net` 一条命令给出 Let's Encrypt 证书（本机 `tailscale cert --help` 核对，2026-09-02），手机侧零安装；zuan 出网可达 Let's Encrypt（instance.md）。这是实例事实，不是架构偏好——换一个没有公共名字的部署，就是 `self-ca` 的评估结果说了算。
- **agora 永远自己终止 TLS**：MISSION §8"TLS 是 agora 的事"指的是这一点；`external` 只是证书文件从哪来。**不支持 HTTP 终止型反向代理**（TCP 透传对 agora 不可见，无需支持），因此没有 `trusted_proxies`、不采信任何转发头。
- **不内置 ACME**：任何 challenge 类型都是 `external` 的特例（lego / certbot 出文件再交给 `external`）。

### D5 监听模型：明文只在 loopback，非 loopback 永远 TLS；零凭据只警告（事实 4、5）

- 两个监听器：`server.listen`（明文，**配置校验拒绝非 loopback 地址**，默认 `127.0.0.1:7680`，本机浏览器与 `agora open` 用）与 `server.tls_listen`（TLS，非 loopback，默认关，被 peer 或手机访问时才开）。两个监听器是为了本机浏览器永远不撞自签证书或主机名不匹配的警告，同时让"非 loopback 永远 TLS"成为结构而不是检查（A34 的新表述）。
- MISSION v0.8 的"非 loopback 监听且未配置认证 → 拒绝启动"**降为启动警告**："TLS 监听器已开但没有任何机器 token 与已配对设备，没有人能连进来；`agora peer token create <name>` 或 `agora pair`"。理由是事实 5：devcenter 需要拒绝启动，是因为它未注册 OTP 时中间件直通；agora 的 D1 没有直通，零凭据状态下监听在公网上什么也做不了。
- `public_url`：远端配对链接与 QR 用的对外地址（例 `https://zuan.tail6f613.ts.net:7681`），不自动猜。

### D6 本机通道与目录：`AGORA_HOME = ~/.agora` 0700，socket 仅属主可访问 + 对端 uid 校验（事实 2）

- `AGORA_HOME` 默认 `~/.agora`（与 `~/.claude` / `~/.codex` / `~/.grok` 同一习惯，两平台一致，路径短、无空格——unix socket 路径上限 104 字节），环境变量可改。内含 `config.yaml`、`agora.db`、`agora.sock`、`tls/`、`hooks/{inbox,done}/`、`tmux.conf`、`bin/agora`（指向当前二进制的稳定路径，安装与升级维护；hook 命令与 Codex 的内容哈希信任依赖它，ADR-002 D4）。ADR-001 / ADR-002 写的 `<state_dir>` 即此目录。
- 权限：目录 0700、文件 0600；启动**自检**：目录不属于当前 uid、或 group / other 有任何位 → 拒绝启动并打印 `chmod` 命令（与 ssh 对 `~/.ssh` 相同）。
- `agora.sock` 以 umask 077 创建（仅属主可访问）；此外每个连接取对端凭据（tokio `UnixStream::peer_cred()`：Linux `SO_PEERCRED`、macOS `getpeereid`，两平台都在 tokio 1.53 的支持列表里，2026-09-02 本机核对），uid ≠ 自己 → 立即关闭。文件权限挡正常情况，peer_cred 挡权限被改错的情况。
- socket 上跑的：hook 的唤醒 / 挂起（ADR-002 D3）、配对链接铸造（D2）、CLI 查询。改配置、签 token、吊销设备的 CLI 直接操作 `config.yaml` / SQLite，不需要 daemon 在跑。
- 同一主机多个 OS 用户各跑各的实例：各自的 `AGORA_HOME`、socket、端口；端口被占报错退出，不自动换端口（否则 `agora open` 不知指向谁）。

### D7 浏览器侧加固（只剩与凭据强度无关的部分）

- **响应头**：`X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`、CSP 含 `frame-ancestors 'none'`；TLS 监听器追加 HSTS。
- **CSRF 与 WS**：cookie 认证的非 GET 请求必须带同源 `Origin`（或 `Sec-Fetch-Site: same-origin`）；`/api/events` 与终端升级校验 `Origin` 与 `Host` 同源。Bearer 认证的 peer 客户端不是浏览器，跳过。
- **没有登录限流、没有锁定、没有可信代理**：它们是 TOTP 码空间的账，TOTP 不在了账也不在了。配对与 Bearer 的失败只记日志。

### D8 危险操作的确认在会话所在节点执行

- Kill / Restart 请求带 `confirmed`；**所属节点**按 agent 状态判断是否需要确认（MISSION §8"确认跟着杀走"），需要而未确认 → `NeedsConfirmation`，客户端弹框后重发。
- 转发节点只原样转发 `confirmed`，绝不自己置 true；peer principal 不享有免确认。fake 节点测试三节点链路。

### D9 多人：两种场景，两个结论

- **同一主机多个人，各跑各的实例**：支持，靠 D6 + 各自端口；互为陌生人。
- **多个人共用一个实例**：**不支持**，V1 / V2 都不设计（§11 RBAC / Teams）。留口：`Principal::Human` 加用户字段、`devices` 表加 user 列。到那一天加的是用户表，不是认证路径。

### D10 威胁与接受的代价

| 威胁 | 挡它的 | 剩余风险（接受） |
|---|---|---|
| 同机其他 OS 用户 | D6 0700 / 0600 / peer_cred；D1 无 loopback 例外 | root，无法防 |
| 网络上的陌生人 | D1；D2 配对链接 256 位单次；D3 Bearer；D5 永远 TLS | HTTP 栈本身的攻击面（静态资源、配对端点） |
| 中间人 | D4 指纹钉住（peer）；浏览器可信证书（人） | `external` 换密钥忘更新指纹 → 显示"指纹不匹配"，不静默降级 |
| 配对链接在 5 分钟内被偷看 | 单次使用：攻击者用了，主人的配对就失败"已使用"，且设备列表多一台 | 主人没注意设备列表 |
| peer 节点被攻破 | 按 peer 吊销 | 攻破期间它能控制所有把它当 peer 的节点 |
| 笔记本 / 手机被偷（带 session） | 从任何其它已配对设备或节点 CLI 吊销该设备 | 吊销前的窗口；滑动 30 天 |
| 拿到 `token_file` | 吊销 + `--rotate` | 同上 |
| 拿到 `AGORA_HOME` 备份 | 里面没有可远程使用的人的凭据（session 只存哈希）；`token_file` 是别的节点的 | peer token 明文在持有方 |
| 所有已配对设备失效 | 节点本机或 ssh 上 `agora pair` | 无远程自助恢复——这是放弃 TOTP 付的唯一代价；zuan 当前未开 sshd（instance.md），安装脚本须开 sshd 或接受只能物理登录（`agora-7ku.1`） |

## Non-Goals

- 多人共用实例的授权、RBAC、审计追溯到人（§11）。
- TOTP、Passkey、OAuth、任何账号体系。
- 内置 ACME；反向代理与转发头；登录限流。
- 节点发现、自动配对、反向拨号（§11）。
- 防 root；tmux 默认 socket 的访问控制（由 tmux 按 uid 管，ADR-001）。

## 什么会让它变危险

- **某个 handler 忘了要 Principal** → 免认证端点。守卫：遍历路由表，白名单以外每条路由不带凭据必须 401 → `tests/auth.rs::every_route_requires_principal_except_allowlist`。
- **loopback 免认证的特例回来了** → 多用户主机后门。守卫：`tests/auth.rs::loopback_requires_session`。
- **配对链接可重用、不过期、或未认证也能铸造** → 守卫：`tests/auth.rs::pair_token_single_use_and_expires`、`::pair_token_minted_only_via_socket_or_session`、`::pending_pair_tokens_capped`。
- **session 不过期或吊销不生效** → 守卫：`tests/auth.rs::session_idle_and_absolute_expiry`、`::revoked_device_rejected_immediately`。
- **明文监听器绑到非 loopback**或**TLS 监听器降级明文**（A34） → 凭据上网。守卫：`tests/listen.rs::plaintext_listener_refuses_non_loopback`、`::tls_listener_never_serves_plaintext`。
- **未签发 token 也接受 Bearer** / **Bearer 在明文监听器上被接受**（A31） → 守卫：`tests/peer_token.rs::no_token_issued_rejects_all_bearer`、`::bearer_rejected_on_plaintext_listener`、`::revoked_token_rejected_immediately`、`::plaintext_never_stored`。
- **TOFU 或指纹不匹配时降级为"离线"** → 中间人静默成功。守卫：`tests/peer_tls.rs::fingerprint_mismatch_is_refused_not_stale`、`::no_pin_no_connect`。
- **socket / 目录权限过宽仍照常运行** → 守卫：`tests/local_channel.rs::socket_rejects_other_uid`、`::home_perms_too_open_refuses_start`。
- **cookie 认证的跨站请求被接受** → 守卫：`tests/auth.rs::cross_origin_cookie_request_rejected`（含 WS 升级）。
- **session cookie 不带 `Max-Age`，或带了但不随使用续期** → "配对一次后 30 天免登录"在浏览器侧不成立，用户被迫反复回到节点本机 `agora pair`，而这正是本 ADR 用配对换掉 TOTP 时承诺不会发生的事。守卫：`tests/auth.rs::session_cookie_max_age_tracks_config_and_slides_with_use`、`::secure_cookie_still_carries_max_age`。注意 `::session_idle_and_absolute_expiry` **不**覆盖这一条：它全程复用同一个 cookie 串，等于假设浏览器永远不丢 cookie，只验了服务端那一半。
- **转发节点替客户端置 `confirmed`** → 守卫：`tests/forward.rs::kill_confirmation_enforced_at_owner`。
- **有人好心把限流加回来** → 它会重新引入"锁住主人"这一类攻击，而配对链接不需要它。守卫是本文与 D7 的注释；若将来引入可猜的凭据，先改本 ADR。
- 不变量 11 的 fake-agent 集成测试（A36）由以上守卫合并覆盖；逐条关掉守卫，对应测试变红。

## Consequences

**正面**：人的认证只有一种机制（配对），本机与远端、V1 与 V2-1 共用；节点间只有一种机制（Bearer + 指纹）；代码里没有 loopback 例外、没有限流器、没有转发头逻辑。V1 要做的只有：配对 + session、机器 token、自签证书 + 指纹、两个监听器、目录与 socket 权限。

**负面**：本机第一次打开浏览器要经 `agora open`（之后 30 天滑动免登录）；所有已配对设备失效时无远程自助恢复；`external` 换密钥要手动更新 peer 指纹。

**MISSION 回写**（v0.13，2026-09-02）：不变量 11"人：TOTP"→"人：已配对设备的 session"；§3.5"也不需要 TOTP"→"也不需要远端配对"；§8 三处（拒绝启动→警告 + 两个监听器、人的凭据小节重写、签发前置条件删除）；§9.1 加 `AGORA_HOME`；§11 手机条目"TOTP 人机认证"→"远端设备配对"；A31 / A34 改写。spec：`config.md`（`server` / `tls` / `auth` 段）、`api.md`（`/api/auth/*`、401、`NeedsConfirmation`、health 公开子集）、`architecture.md` 第二行。

**跟进 issue**（均 discovered-from `agora-90t.4`）：`agora-xqa.5` M1a 配对 + session + `agora open` / `agora url`；`agora-7ku.2` M2 机器 token、自签证书 + SPKI 指纹钉住、两个监听器与启动警告、`external` 续期与热加载；`agora-thc.1` V2-1 远端配对（`agora pair` QR、Dashboard 设备列表与吊销）+ 加固头 + Origin 校验；`agora-thc.2` V2-1 `self-ca` 评估。

## 参考

- `docs/analysis/devcenter/README.md` §3.4、§3.5 第 5 条、§4.5、§6.2、§6.3
- `docs/analysis/devcenter/appendix-b-multihost.md` §54–58、§96–98
- devcenter `docs/adr/ADR-006-security-boundary.md`、`ADR-008-totp-auth.md`（含 Phase 4.1 限流修正——本文否决 TOTP 的论据主要来自它对限流必要性的论证）
- MISSION §0.1、§2.2 不变量 9 / 11、§3.5、§8、§11、§12 A27 / A30 / A31 / A34 / A36；ADR-002 D3；ADR-004

## 附录 A：本机核对（2026-09-02）

| 项 | 结果 |
|---|---|
| tokio `UnixStream::peer_cred()` 平台支持 | `~/.cargo/registry/.../tokio-1.53.1/src/net/unix/ucred.rs`：Linux / Android 走 `SO_PEERCRED`，macOS / iOS 走 `getpeereid`；两平台均在列 |
| `tailscale cert` | `/usr/local/bin/tailscale cert [--cert-file --key-file --min-validity] <domain>`；`--min-validity` 可作 `renew_command` 的幂等续期开关 |
| devcenter `auth.rs` | TOTP ±1 窗口 + `counter > last_used`、阶梯 `[60, 300, 1800, 7200, 21600]`、全局 50、来源表 4096；**未沿用**——这套东西存在的前提（可猜的码）在本文已被去掉 |
| 本机 tailnet | `tail6f613.ts.net`；zuan 待装 tailscale——`external` 路径在 zuan 上走通是 A35 的前提 |

## 附录 B：事故记录

（上线后追加）
