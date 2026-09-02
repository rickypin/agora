# ADR-003: 节点认证模型

- 状态：Proposed（待写）
- 日期：—
- beads：`agora-90t.4`
- 依赖：MISSION §0.2（几台机器、几个人、在哪）、§0.1（多用户是 Non-Goal；同机多用户多实例互为陌生人）

## Context（预填）

devcenter 的 hub↔node 联邦走 `ssh -N -L` 隧道，**节点侧零认证**："隧道端口即信任"，hub 本机任何进程都能以用户身份在所有节点执行任意命令（报告 §3.5 第 5 条）。用户侧 TOTP + cookie + 阶梯限流做得扎实，但那是浏览器→hub 这一跳，不是 hub→node。

## 决策问题

节点之间以及客户端到节点，以什么身份模型、什么凭据互认；单人多机与多人各需要什么。

## 备选项（预填）

| 备选 | 一句话 | 适用 |
|---|---|---|
| 每节点共享密钥 / token | 注册时发放，请求头携带 | 单人 ≤ 十几台，最省事 |
| mTLS | 节点证书由 hub CA 签发 | 多人 / 跨网络；证书轮换成本 |
| ssh 隧道即信任 | devcenter 现状 | **不借鉴**（报告 §6.3） |
| principal-based（用户 × 节点 × 动作） | 多用户与审计的前提 | 若 MISSION 把多用户放进范围，第一天就要是这个 |

## 预置约束

- 第一天就做节点侧认证（报告 §3.4 安全边界行）。
- 继承联邦**纪律**不继承实现：节点自治、hub 只做快照、stale 而非消失、写至多一次、不可逆步骤最后做；节点主动外连 + 事件推送替代 ssh 隧道 + 轮询（报告 §6.2 联邦行）。
- 登录限流的阶梯锁定 + 受信网段豁免可直接沿用（§6.2）。

## 参考

- `docs/analysis/devcenter/README.md` §3.4（安全边界、多主机）、§3.5 第 5 条、§4.5、§6.2、§6.3
- `docs/analysis/devcenter/appendix-b-multihost.md`

## MISSION 迁入的 devcenter 默认值（2026-09-02）

### 人的凭据：TOTP（原 §8）

- 只绑 loopback 时可不配置（loopback 即安全边界）；一旦要让**人**从非 loopback 访问，必须先 `agora otp setup`。V1 没有这种访问（浏览器只开 loopback，zuan 只接受机器 token），TOTP 随手机阶段落地。注册后**人的**请求（含 loopback 的浏览器与 WS 握手）必须携带登录 cookie；hook 投递不经 HTTP（ADR-002 D3：投递箱文件 + unix socket，无 `/api/hooks` 端点）。
- RFC 6238（HMAC-SHA1 / 30 s / 6 位 / ±1 窗口）；重放规则 `counter > last_used`；session token 只存 SHA-256 哈希，30 天过期。

### 面向非 loopback 时的加固（原 §8）

一旦 daemon 不再只绑 loopback，认证就是唯一控制点，登录限流的强度直接等于整个系统的强度：

- **按来源限流 + 递增锁定**：单来源 5 次失败后锁定 60 s → 5 m → 30 m → 2 h → 6 h，24 h 无失败清零。
- **全局兜底**：未受信来源累计 50 次失败则关闭公网侧登录；loopback / RFC1918 / 100.64.0.0/10 豁免全局锁。
- **可信代理白名单**：只采信来自它的 `X-Real-IP` / `X-Forwarded-*`。
- **Cookie**：`HttpOnly; SameSite=Lax`，经 TLS 时加 `Secure`。
- **响应头**：`X-Content-Type-Options`、`X-Frame-Options: DENY`、`Referrer-Policy`、CSP（含 `frame-ancestors 'none'`）；经 TLS 时追加 HSTS。
- **WS 跨站防护**：`/api/events` 与 terminal 升级校验 `Origin` 与 `Host` 同源。

### 本 ADR 必须回答的（MISSION v0.8 §3.5 / §8）

- 两种 principal：人（TOTP）与节点（机器 token，Bearer）。token 按 peer 签发 `agora peer token create <name>`、可吊销、被访问方只存哈希；持有方 `token_file` 明文 `0600`。
- 证书指纹钉住：peer 之间自签证书即可；只有被浏览器直接打开的节点需要浏览器可信证书（V1 没有；手机阶段的 zuan 需要）。TLS 三路径（ACME DNS-01 / 私有 CA / 外部证书）默认选哪条。
- peer 访问默认关闭：未签发 token 即拒绝一切 Bearer；签发前置条件（非 loopback 监听 + TLS）不满足拒绝签发，守卫测试（A31）。非 loopback 无任一 principal 凭据 → 拒绝启动（A34）。
- ~~hook 端点不适用 cookie~~ → 已无 hook 端点（ADR-002 D3，2026-09-02）；本 ADR 只需定 unix socket 与投递箱目录的权限（0700 / 0600 = 这台机器上的这个 OS 用户），它同时回答 §0.1 多用户主机的 loopback 问题。
- 危险操作确认逻辑在会话所在节点执行，转发节点不代替判断。
- 见 beads `agora-90t.4` 注记的输入清单。

### MISSION v0.8 迁入的论证与细节（2026-09-02）

- **"非 loopback 无认证拒绝启动"的动机**：devcenter 的前提是"loopback 即安全边界、OTP 可选"，但被 peer 或手机访问的节点必须对外监听；且 tailnet 不是私有网段——实测有他人共享的节点与 tagged 设备（`docs/spec/instance.md`）。"改了 listen 忘了配凭据"不能是一个可运行的状态。
- **TLS 三条候选路径**（默认选哪条由本 ADR 定）：① 内置 ACME DNS-01（需域名与 DNS API；zuan 可达 Let's Encrypt，但私网无入站，HTTP-01 不可用）；② 自带私有 CA，一条命令导出根证书装到手机；③ 接受外部证书（`tailscale cert`、反向代理）。
- **机器 token 的已知代价**：持有 peer token 的节点被攻破 = 能控制那些 peer。单用户下接受；按 peer 吊销 token 即止损。
- **CLI 形态参考**：`agora otp setup`、`agora peer token create <name>`。
