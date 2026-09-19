# ADR-004: 节点拓扑——互为 peer、一跳转发

- 状态：**Accepted**（2026-09-02；决策随 MISSION v0.5 做出并经用户确认，v0.8 将论证迁入本文）
- beads：—（决策已定，无决策工作项；实现验收在 M2a epic `agora-7ku`（MISSION §12 A27、A30、A31、A33、A38）与 M2b `agora-s4r`（A29））

## Context

多节点是产品定义的一部分（MISSION §0.1：节点数量任意，新增节点不改变架构）。devcenter 的多主机方案是 hub + `ssh -N -L` 隧道 + 轮询快照（其 §11、ADR-007）：固定的 hub / worknode 角色，节点侧零认证（"隧道端口即信任"），hub 轮询各节点全量快照。

把"多节点"剥到不可再剥，只剩五件事：

1. 每个节点暴露一个 API（单节点也要）；
2. 跨机器连接必须认证 + 加密；
3. 会话身份全局唯一；
4. **某个地方**把 N 个节点并成一个视图；
5. 终端字节从会话所在机器到达屏幕。

前三件与架构选择无关，任何方案都要做；真正的选择只在第 4、5 件。

## 决策问题

视图合并放在哪里；节点间链路用什么信任模型。

## 备选项

| 方案 | 问题 |
|---|---|
| 独立 hub 组件（devcenter 形态） | 多一种部署角色与单点；hub↔node 若走隧道则节点侧零认证（报告 §3.5 第 5 条） |
| 浏览器端合并（客户端直连 N 个节点） | 浏览器的同源模型对跨源合并是敌意的：混合内容限制、SameSite、本地网络访问权限、按 origin 绑定的推送订阅；且每个节点都需要浏览器可信证书 |
| **节点互为 peer（选定）** | 代价见 Consequences：每对 peer 各一行配置与一次 token 签发 |

daemon↔daemon 链路没有浏览器的同源规则，这是把合并放进节点而不是浏览器的决定性事实。

## Decision

节点互为 peer；细则已定格在 MISSION §3.5（peers 配置、节点是 peer 的 API 客户端、只导出本机会话 + 一跳防环、断线 stale、浏览器一次只连一个节点、peer 访问默认关闭、按所属节点路由、时间用本地时钟）。

保留 devcenter 多主机里正确的三件事：**工作节点默认只对本机开放；汇聚只转发不拥有；断线 stale 不消失**。去掉：ssh 隧道、hub / worknode 固定角色、轮询快照、跨节点历史、跨 host 搬迁。

## Non-Goals

- 跨 host 搬迁 session（MISSION §1.4；识别机制 session.id 单独保留，§5.6）。
- peer 历史、反向拨号、多跳转发（V2，MISSION §11）。

## 什么会让它变危险

- **转发从 peer 收来的会话** → 环路与所有权混淆。守卫：只导出本机会话；fake 节点测试三节点链路不成环（规则 9）。
- **peer 默认开放或免签发** → "隧道即信任"重演。守卫：未签发机器 token 拒绝一切 Bearer 调用（MISSION §8，A31）。
- **信任 peer 报的时间戳** → 时钟漂移污染 attention 排序。守卫：peer 视图时间由本节点时钟打。（2026-09-19 附录：这条规则的另一面——它使出来的数是个**下界**，而 UI 把下界画成了精确值。）

## Consequences

- 汇聚点由配置决定（浏览器指向哪个节点，哪个就是汇聚点），不由架构决定；新增节点 = 加一行 peer。
- 代价：每对 peer 各一行配置与一次 token 签发；N 节点全互联是 N×(N−1) 行——单用户 2–3 台可接受，规模化时再谈发现机制（不在 V1/V2 范围）。
- Mac 带出门且 zuan 够不着它时，zuan 上显示 Mac 为"上次见到"——用户在哪台机器前面就看得到那台，这是物理事实不是设计缺陷；部署 tailnet 则随处可达。
- 当前实例的四种形态见 `docs/spec/architecture.md`。

## 附录：本机重启把 peer 行的等待时长归零（2026-09-18，beads `agora-5gg.12`）

**现场**：Mac 的 daemon 重启后，侧栏里 zuan 那 24 行的等待时长全成了 0.7 h；其中 `ef0e50` 真实
STARTING 已经 8 天。peer 那侧什么都没变，变的是本机。

**为什么**：本 ADR 把"等待时长"的起点交给本节点时钟（上面危险句第 3 条），实现是**本节点第一次看见
该行处于当前状态**的时刻（`src/peer/view.rs` 的 `stamp`）。这个时刻只活在内存里，本机 daemon 一重启
就空了——「重连不重置」管的是 peer 断线重连，没管本机重启，于是画出来的数从"我看了它多久"退化成
"我刚起来"。更根子上的问题是：这个数**一直**是个下界（真实起点可以早于我第一次看见它），而 UI 把
下界画成了精确值。

**改了什么**（不改本 ADR 的决策：peer 的绝对时刻仍然不信）：

1. **信 peer 报的时长差，不信它的绝对时刻**。`GET /api/sessions` 顶层多一个 `now`：打这份快照时
   报告方自己时钟的读数。并入方只拿它与行上的 `status_since` 相减——两个读数出自同一只表，差是
   相对量，两节点之间的时钟偏差在这个减法里自己抵消。只在"本节点第一次看见这一状态"时用它把起点
   往前推（含本机重启后的第一眼）；状态没变就不再动那个起点——`status_since` 后退一改，前端「看过」
   的键（`<id>@<status_since>`，`agora-23h`）就对不上，已经看过的 FINISHED 行会弹回 NEEDS
   ATTENTION。读不出 / 为负 / 超过 30 天一律当它没报：坏掉的 `status_since`（0 / 1970）算出的差不
   是下界，截断到 30 天照样是撒谎，退回本机此刻只会短、不会长。老节点不报 `now` → 与改之前一致。
2. **下界画成下界**：peer 行的时长带 `≥`（`web/src/attention.ts` 的 `statusLine`；判据是行上有
   `stale` 键 = 它是 peer 行）。本机行的起点是自己打的，精确，不带 `≥`。

**没有采纳的**：把 `(id, token, since)` 落盘、重启恢复（B 方案）。它把"我记得看过这行多久"变成第三
份要维护的持久状态，而它想保住的那个数本来就该由 peer 报（peer 才是它自己状态起点的权威）；而且重启
恢复出来的起点仍然只是下界，UI 照样得画 `≥`——等于两件事各做一半。

**守卫**：`src/peer/view.rs` 单测 `a_local_restart_keeps_the_wait_the_peer_reports`、
`a_later_snapshot_does_not_push_an_existing_stamp_earlier`、
`an_untrusted_reported_wait_is_ignored_rather_than_guessed`；`tests/peer_view.rs::
a_local_restart_does_not_reset_a_peer_rows_wait`（真客户端循环，peer 报 8 天）、
`::peer_timestamps_use_local_clock`（绝对时刻仍然不信）；`web/src/attention.test.ts` 的 peer 行时长
文案。形态变更见 `docs/spec/api.md`「peer 视图」。
