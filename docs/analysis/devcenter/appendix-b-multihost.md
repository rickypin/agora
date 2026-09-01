# 附录 B：多主机联邦 / 跨主机搬迁 / 远程浏览器 代码级评审

> 对象：`~/code/devcenter` v0.5.1（commit `1540ac4`，2026-08-28）。
> 范围：`src/{hosts,federation,tunnel,relay,ws_proxy,deploy,unpack,moving,transfer,candidates,fetcher}.rs`、`src/browser/*`、`src/server.rs` 中相关 handler、ADR-007/008/011/012 与对应测试。
> 本附录是分领域评审的原始结论，综合评价见 [README.md](README.md)。行号以上述 commit 为准。

## 1. 联邦架构的实际实现

**轮询与快照。** hub 复用 observer 的 `status.detector_interval`（默认 2s，`src/config.rs:187`，`src/main.rs:130`）驱动 `federation::spawn`（`src/federation.rs:503-514`：`poll_once` 后 `sleep(interval)`，无漂移补偿）。`poll_once` 每节点一个 task（L335-347），共享 reqwest client 硬绑 `NODE_TIMEOUT=2s`（L23, L194-197）。节点不可达时 `mark_down` 保留上次 `sessions` 仅置 `reachable=false`（L477-487），`remote_sessions` 以 `stale=!reachable` 标记（L291-303）——"节点消失时会话不消失"由 `tests/federation_test.rs:133-189` 钉住。

**元数据缓存是真正的陈旧点。** `meta_fresh` 只在 `set_endpoint`/`mark_down` 时复位（L213-221, L477-487），即 hostname / agents 可用性 / `home_free_bytes` 只在隧道重连时刷新。而搬迁计划的磁盘检查读的正是这份缓存（`src/server.rs:2746-2754`），隧道一周不断的节点，free_bytes 就是一周前的数。

**写路由。** 所有变更 handler 以 `parse_key` 分流后 `proxy_call`（`update_session` L1117-1147 手工拼 body，`kill` L1184，`restart` L4156，tabs L4351/4381/4556/4603）。写后 `refresh_host` 只刷新不判死（L895-920，测试 L930-976）——这是正确的：健康判定只归 ticker。

**终端 WS 透传。** `terminal_ws` 在 upgrade 前解析 endpoint 使错误以 HTTP 状态返回（L4720-4752），随后 `ws_proxy::bridge` 逐帧转发（`src/ws_proxy.rs:43-100`）。但 bridge 自称 protocol-blind，连接失败时却发送终端协议的 `{"type":"output",...}`（L47-57），而 `browser_ws`（L4773-4803）复用同一 bridge——浏览器客户端收到的是它不认识的帧。文档与代码在此不一致。

**poll-diff 事件。** `poll_node` 仅当上次状态存在且不同才合成 `Event::Status`（`federation.rs:373-475`），首次出现静默，A→B→A 在一个周期内丢失；测试 L563-638 只覆盖无重复，不覆盖丢失。

**会话键。** `<host>:<bare>`、本地裸 id、`local` 保留（`src/hosts.rs:14-39`）；`parse_key` 对 `":abc"` 之类退化键静默当本地（L34-39）。

**隧道监督。** 默认 grace 2s / backoff 1s→60s / healthy_reset 60s（`src/tunnel.rs:38-48`）；`supervise`（L282-409）`kill_on_drop`、排空 stderr 留末行、grace 内死亡即退避×2+抖动、up≥healthy_reset 则重置退避；pidfile 三行 `pid\nargv\nowner_pid`（L101-106）；`reap_stale`（L141-188）+ `reap_orphans`（L235-260，`ppid==1` 且 argv 前缀精确匹配）。两处弱点：`alloc_port` 先 bind 再释放给 ssh，TOCTOU（L77-82）；`owner_still_running` 用 `ps -o args=` 含 `"devcenter"` 判定（L117-127），任何 argv 里带这个词的进程（比如编辑器打开 devcenter/ 目录）都会让陈旧隧道免于回收。另外 `relay.rs:184-196` 的 `spawn_ssh` 把 stderr 丢 `/dev/null`，正是 `tests/tunnel_test.rs:169-216` 那个"隧道死了不知道为什么"的教训在同一仓库里重演。

## 2. 跨主机搬迁

**plan/execute/undo。** `build_plan`（`server.rs:2852-3054`）把拒绝理由当数据返回；`fetch_context` 对远端用 `MEASURE_LIMIT+60s` 而非 2s（L2838-2846）；吞吐用真实 socket 先直连后中继实测（L2999-3020）。`start_move`（L3256-3381）先落 `agent_session_id` 再 plan（L3267-3276），对"会丢对话且客户端没说话"的搬迁拒绝并列出候选（L3288-3329），然后 `tokio::spawn(run_move)`；失败时只在未过不可返回点才清墓碑（L3371-3377）。`run_move`（L3625-3877）顺序：Preparing → 目录 leg → 上下文 leg + `/import-context` → DestReady（同时备份 `.premove.bak`）→ kill 源 → SourceKilled → 目的端 create → 700ms 后 `session_pane` 查活（L3829-3859）→ Done。`undo_move`（L4106-4142）先 `destination_of`（L3942-3981，hub 内存登记表优先，回退到源行的 `moved_to_hostname + agent_session_id`，L4017-4092）杀目的端，再清墓碑，源仍活则只清标志。`restart_session` 对过不可返回点的墓碑拒绝（L4177-4193）——防"一个对话两个作者"。

**墓碑。** `MoveState` 五态，`past_no_return = SourceKilled|Done`（`src/moving.rs:24-64`），全部写在源节点行上（`mark` L3407-3444 → 节点 `/move-state`），hub 的 `moves` map 明确"允许蒸发"（L2571-2573）。

**传输握手。** 一次性 listener：`listen` 绑随机端口（`src/transfer.rs:177-185`），`receive_into`（L229-347）120s 内 accept 后立即 drop listener、读 token 行、常量时间比较（L165-174）、回 `ok\n`、再喂 `tar -x ... --no-same-owner --no-same-permissions`，字节经 `TarGuard` 逐头校验（`src/unpack.rs:114-294`：拒绝设备/FIFO/稀疏/setuid、拒绝 gzip 魔数、路径逃逸）；stderr 单独线程排空留 16KiB 尾（L522-540）。`claim` 父目录 `create_dir_all`、叶子 exclusive `create_dir` + marker（L587-616），失败 `unclaim` 只删自己建的（L638-648）。节点侧 `transfer_offer`（`server.rs:1328-1479`）：`expand_home` → 必须绝对 → 拒 `..` → `starts_with(home)`；`bind` 取自请求或 `listen_addr_for(peer)`（L1272-1280，用 UDP connect 问路由表）；先 claim 后 listen；`spawn_blocking(receive_into → verify_staged → clear_marker)`。

**候选识别与改写。** `candidates::list`（`src/candidates.rs:335-395`）优先 `adapter.context_dir_for(cwd)` 单目录读取，否则深度≤4 遍历后按 cwd 过滤；`summarize` 只读头尾 1MiB/4MiB 窗口（L32-51, L227-245）。`set_agent_session_id` 拒绝重贴标签、非候选、cwd 不符、已被占用（L1808-1892）。改写只针对 4 个 JSON 键（`moving.rs:187-207`：cwd/trackingPath/payload.cwd/persistedOutputPath），按路径分量前缀替换（L142-151），带 `Scope` 区分工作目录与 sidecar（L163-170）；文件内容引用的旧路径刻意不改（`tests/move_test.rs:422-432`）。`export_context` 发送前再用 `summarize` 核对 transcript 的 cwd（L1981-2071）。

**失败路径的弱点。**
- `kill_one` 远端走 2s 的 `federation.call`（L3884-3893）：kill 已执行但响应超时 → 状态停在 DestReady（可恢复）→ 墓碑被清，源已死，目的端 `~/.claude/projects` 里已装好的 transcript 成孤儿。
- `one_leg` 等目的端 `verify_staged` 最多 600×200ms=120s（L3580-3609），对 ADR 自述的 208 万文件树不够。
- `TcpStream::connect` 无超时（`transfer.rs:392, 430`），防火墙 DROP 的节点对导致 plan 先在 hub 侧 45s 上限（L3235）上等满才回退中继；每次失败的直连探测在目的端泄漏一个 `drain` 阻塞线程 120s（L2533-2537）。
- `state.transfers` 只 insert 不 prune（L1394-1470）。
- `durable_destination` 远端分支读 poll 缓存（L4079-4087），与 L3957 自己写的"不能读缓存"理由相悖。

**复杂度 vs 价值。** 搬迁相关 `server.rs:1227-2530 + 2569-4142` 约 2900 行，加 moving/transfer/candidates/unpack/relay 约 2600 行，为一个功能写了 5000+ 行，其中每道防线都对应 ADR-011 附录里一次舰队事故。对作者的舰队它值；对新项目，这是"除非核心卖点否则别碰"的级别。

## 3. 远程浏览器

**管线。** `launch_args`（`src/browser/mod.rs:74-91`）`--remote-debugging-port=0 --user-data-dir=<profile>`；`parse_active_port`/`wait_for_endpoint` 100ms 轮询 `DevToolsActivePort`（`cdp.rs:92-133`，超时 60s `bridge.rs:44`）；`Cdp::connect` 单 reader task 把响应与事件都投进一条 mpsc(256)（`cdp.rs:161-238`）；`ALLOWED` 21 个方法二分查找（L39-65），`tests/browser_test.rs:36-69` 逐字钉死。`bridge::run`（`bridge.rs:110-194`）`setAutoAttach` 扁平化会话，`refresh_targets` 关掉不可视 target（L205-261），`start_screencast` `everyNthFrame:1`（L348-368），`forward_frame` 先发 FrameMeta 文本再发 Binary，按 `frame_interval` sleep 后才 `screencastFrameAck`（L498-552）——用扣押 ack 做背压是正确的做法。`on_client_message`（L554-764）全部翻译、从不透传 CDP；`navigable` 只放 http(s)（L831-845）。

**输入翻译。** `to_page` 分数坐标→viewport 像素并夹边（`input.rs:31-38, 201-224`），`key_params` 区分 keyDown/rawKeyDown（L85-119），`virtual_key_code` 表（L130-193）。**实锤 bug**：`bridge.rs:603` 与 `:654` 调 `to_page(x, y, viewport, 0.0)`，offset_top 恒为 0，而 `browser_test.rs:167-173` 专门测试"offset_top 必须减掉"——单元测试通过，集成路径根本没传值，移动仿真下点击必偏。

**安装与默认。** `install.rs` 生成 sh 脚本经 `deploy::ssh_run` 执行（L173-319），`DC-*` 标记回读；`BrowserConfig.enabled` 默认 false（`config.rs:119`），关闭时 `agent_offered` 直接隐藏浏览器 agent（`server.rs:4870-4872`）；每会话一次性 profile，删会话即删。

**评价。** 约 1500 行换来"像素流 + 白名单"的安全模型，围栏做得扎实（`Runtime.evaluate` 被点名排除）。但代价明确：CDP 单通道意味着事件消费慢会拖住 `send` 响应；每帧两条 WS 消息经 ssh 隧道再经 hub 转发；`navigable` 自认"不是 SSRF 防护"（ADR-012），即一个拿到 OTP 的 hub 用户等于拥有节点内网里的一个浏览器。

## 4. 安全评价

**认证与限速**：TOTP 计数器严格递增（`src/auth.rs:68-85`），token SHA-256 存储（L94-96），阶梯锁定 60s→6h + 全局 50 次 + 可信网段豁免（L99-113, L157-173）；限速器纯内存（L178-238），重启清零。`client_ip` 只在对端是配置代理时信 XFF 最后一项（`server.rs:415-436`），`tests/security_test.rs:64-99` 双向验证。`is_authenticated` 与 `auth_middleware` 每请求各查一次 SQLite（L502-530）。`origin_allowed` 无 Origin 即放行（L467-483）——对 hub 自身的 `connect_async` 是必需的，对拿到 cookie 的非浏览器攻击者本来也拦不住，可接受。

**fetcher SSRF**：`resolve_in_scope` canonicalize 后 `starts_with`（`src/fetcher.rs:161-277`）；`fetch_at_depth` 每跳 `parse_url → 解析全部地址 → net::allowed → --resolve 钉地址 --max-redirs 0`（L415-537, L598-702），DNS TOCTOU 被 `--resolve` 关闭；`net::judge` 链路本地永禁、回环放行、私网/CGNAT/ULA 需开关（`src/net.rs:135-176`），IPv4-mapped 先 unmap（L125-133）。`tests/viewer_test.rs:1014-1059` 钉住重定向到 169.254 的第二跳。以我读到的 `judge` 为准，未见对 0.0.0.0/8、NAT64 `64:ff9b::/96`、组播的显式处理。

**hub↔node 信任边界是最大软肋**：节点无任何认证（未注册 OTP 时 `auth_middleware` 直通，L513-530），隧道在 hub 本机的回环端口即节点全部写 API：`POST /api/sessions` 的 `command` 字段等于在节点以其用户身份任意执行命令，还有 `transfer/offer` 写入 home、`import-context`、`move-state`、`kill`。MISSION §8 承认并推迟到 "0600 unix socket"。再加三点：`transfer_offer` 的 `bind` 按请求原样采用（L1233-1236），hub 可指定节点在任一接口监听，仅靠 256 位 token 挡；token 明文走 LAN TCP（无 TLS）；`starts_with(home)` 是展开后的字面比较，`create_dir_all(parent)` 会跟随 home 内符号链接（`transfer.rs:587-616`），需攻击者先在节点 home 放链接才可利用，风险低但非零。

## 5. 工程质量

- **组织**：`server.rs` 约 5000 行 god file；每个跨节点步骤都是 `if host==LOCAL { 直接调 handler(State, Json) } else { node_json }` 的手写双分支（L3422-3439, L3500-3521, L3705-3722, L3795-3815, L4005-4015），并用 `serde_json::from_value(...).expect("well-formed")` 做类型往返——把 HTTP handler 当内部 API 用，能跑但耦合。`update_session` 远端分支手拼 body 的风险作者自己写在 L1119-1125 并用 `federation_test.rs:640-728` 钉住。
- **错误处理**：`ApiError` 带状态码；空 404 → `old_node_message` 提示升级节点（L4492-4516）很贴心；`is_already_dead` 字符串匹配（L4290-4293）与 L4198 `contains("is not running")` 是自认的味道。
- **并发**：`moving::dir_bytes`（`du` 轮询 `thread::sleep(100ms)` 最长 240s，`moving.rs:455-499`）在 async fn 里直接调用：`server.rs:1643, 1652, 2362, 2392`；`free_bytes`（`df`）在 L736, 2721；`local_hostname()` 每次 spawn 子进程（L4948-4954）。本地端 `fetch_context` 直接 `await session_context`（L2820-2825）会把一个 tokio worker 卡满 4 分钟。反例做对了：`tab_content` 用 `spawn_blocking`（L4634, 4663），`receive_into` 也是（L1405）。
- **可测试性**：`direct:` 传输（`config.rs:165-167`）让 2-3 个真守护进程跑在一个进程里（`federation_test.rs:13-43`, `move_test.rs:26-57`）；`with_home` 给每个守护进程独立 HOME（`move_test.rs:1-8`）；`TunnelOpts.ssh_bin` 注入 bash 假 ssh（`tunnel_test.rs:23-48`）；真 tmux socket 隔离；真 Chrome 可选、全局互斥（`browser_test.rs:542-588`）；手工拼 tar 块（`transfer_test.rs:263-330`）；原始 HTTP 伪造头（`security_test.rs:45-61`）。用 `include_str!`/读源码 grep 做"围栏测试"（`security_test.rs:456-475`, `browser_test.rs:375-382`）——聪明但脆。
- **真实度**：高，但慢且机器敏感（`move_test.rs:367-373` 记录了预算从 20s 放到 60s 的原因）；ssh 中继路径（WP7）零自动化测试，只有 ADR 附录里的手测数据。

## 6. 值得借鉴 / 应避免

**借鉴**
1. 节点自治 + hub 无状态快照、`stale` 而非消失（`federation.rs:291-303, 477-487`）。
2. 健康判定只归 ticker，写后刷新不判死（`server.rs:895-920`）。
3. 隧道监督的全套细节：`ExitOnForwardFailure`、grace 内死亡指数退避、健康期重置、pidfile 记 argv + 精确前缀回收孤儿（`tunnel.rs`）。
4. 子进程 stderr 必须排空并留尾（`tunnel.rs`、`transfer.rs:522-540`）——两处事故换来的规则。
5. 变更前先出"计划即数据"，拒绝理由随计划返回（`build_plan`）。
6. 不可逆步骤前先写"去了哪"，kill 放最后，删除永远单独显式（`run_move` 顺序，`drop_source_copy`）。
7. 传输接收端在 tar 之前做流式头校验 + 拒绝压缩流 + 事后 `verify_staged`（`unpack.rs`）。
8. 出站抓取的"canonicalize 后比较 / 每跳判地址 / `--resolve` 钉 DNS"三件套（`fetcher.rs`）。
9. 一次性 listener + 随机 token + 常量时间比较 + ACK 的最小握手（`transfer.rs`）。
10. 用 `direct:` 传输和注入 `ssh_bin` 把多机集成测试压进单进程。

**避免**
1. 无认证节点 + "隧道端口即信任"：任何 hub 本机进程可在节点执行任意命令。新项目第一天就该有节点侧共享密钥或 mTLS。
2. 在 async handler 里直接跑 `du`/`df`/`hostname` 子进程（L1643, 1652, 2362, 736, 4948）。
3. 把 HTTP handler 当内部函数调用并用 JSON 往返转类型。
4. 元数据"只在重连时刷新"却拿来做磁盘容量决策。
5. 字符串匹配错误消息和 `ps args` 含关键字判归属（`tunnel.rs:117-127`）。
6. 跨主机搬迁整条链（候选识别 + JSONL 改写 + 中继 + 墓碑 + undo）：5000 行为一个功能，且强绑定各 agent 的私有 transcript 格式（4 个键、claude 目录 slug 规则），agent 一升级就漂。
7. 像素流远程浏览器：白名单虽严，仍等于给每个 hub 用户一个内网浏览器；除非是产品核心，不要背这个面。
8. 无 connect 超时的阻塞 TCP（`transfer.rs:392, 430`）和只增不减的 job map。
9. 靠读源码字符串的围栏测试代替类型/结构约束。
10. 同一个 `bridge` 给两种协议复用，却在失败路径写死一种协议的帧格式。

## 7. 总体判断

"ssh -N -L hub/spoke + 节点自治 + 无状态 hub 代理"对**单人、≤10 台、已有 ssh 信任**的场景是最省事的正确选择：零证书、零消息总线、断线时旧快照可读、节点对 hub 死亡完全无感（`tests/federation_test.rs:421-493` 的 detach 不变量）。它的瓶颈也清楚：每节点一条常驻 ssh + 每 2s 一次全量 `GET /api/sessions`，O(节点数) 轮询且事件粒度受轮询周期限制；所有终端/浏览器字节双跳（节点→隧道→hub→浏览器），ADR-011 自测 hub 路径 3.9 MB/s 对直连 114MB/s；hub 是唯一入口和唯一认证点，也是唯一单点。

对比：**mTLS 直连**去掉 ssh 进程监督和 pidfile 那一整套（`tunnel.rs` 550 行），换来证书分发的运维负担，但节点侧天然有认证——正是 devcenter 现在最缺的；**消息总线（NATS/MQTT）**把 poll-diff 变成推送，事件不再丢 A→B→A，节点数上百仍是常数开销，代价是多一个组件和"总线挂了谁兜底"；**agent 主动外连 hub（反向长连接）**最适合节点在 NAT 后的情况，也天然解决 devcenter 里"hub 不知道节点哪个地址可达"的 `peer` 猜测问题（`server.rs:1237-1254` 那段自我纠错的注释就是证据）。新项目若节点数可能超过十几台或有多用户，建议：节点主动外连 + 共享密钥/mTLS + 事件推送；保留 devcenter 的"节点自治、hub 只做快照、写入至多一次、不可逆步骤最后做"这几条设计纪律，而不是它的 ssh 隧道实现。
