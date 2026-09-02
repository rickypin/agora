# agora — ROADMAP（视图）

> **由 `scripts/roadmap-view.sh` 生成，不要手改。** 真相源是 beads：阶段 = epic，阶段门 = epic 之间的 `blocks` 依赖，验收标准 = epic 的 `--acceptance`。
> 本文件不放任务 checkbox（避免 devcenter 式双轨，见 `docs/analysis/beads/README.md` §6.3 / §8.2）。任务级细节：`bd ready`、`bd dep tree <epic>`。
> 生成时间：2026-09-02

| 阶段 | epic | 目标 | 阶段门（被谁阻塞） | 验收要点 | 状态 / 进度 |
|---|---|---|---|---|---|
| M0 | `agora-90t` | MISSION 定稿与首批 ADR | — | MISSION.md 各节无 TODO、候选标记全部清除；ADR-001/002/003 状态 Accepted 且 docs/adr/README.md 索引更新；M1a/M1b/M2/M3 epic 已在 beads 建立（agora-xqa / agora-dvh / agora-7ku / agora-h1k）并带阶段门依赖（M1a→M1b→{M2∥M3}），验收引用 §12 编号；M1a/M1b 已拆任务（agora-90t.5）；ROADMAP.md 视图已刷新。 | open 6/7 |
| M1a | `agora-xqa` | 终端底座 | `agora-90t` | MISSION §12：A1、A2、A3、A4、A5、A6–A12、A20、A21、A24、A25；A36 中不变量 1–5、7 的测试在本 epic 钉死（10 在 M1b、8/11 在 M2 补齐）。逐条可打勾；每条对应 epic 内至少一个 issue。（A4 曾悬置待 Grok hooks 实测，2026-09-02 实测证实走 hook 路线后归位本 epic，见 agora-90t.3。） | open 0/14 |
| M1b | `agora-dvh` | Agent 感知 | `agora-xqa` | MISSION §12：A14–A18、A22、A23、A32；补 A36 中不变量 10 的测试。逐条可打勾；每条对应 epic 内至少一个 issue。 | open 0/13 |
| M2 | `agora-7ku` | peer 与安装运维 | `agora-dvh` | MISSION §12：A26、A27、A29–A31、A33、A34、A38、A39；补齐 A36 中不变量 8、11 的测试。 | open 0/2 |
| M3 | `agora-h1k` | 产出与起会话增强 | `agora-dvh` | MISSION §12：A40–A44。 | open 1/1 |
| V2-1 | `agora-thc` | 手机客户端与 PWA（iOS / Android） | `agora-7ku` | MISSION §11 手机条目：A13、A19、A28、A35、A37。 | open 0/2 |
