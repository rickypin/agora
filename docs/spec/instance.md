# 当前实例实测（2026-09-01）

本机与 zuan 均直接登录查看，tailnet 用 `tailscale status` 查看，已经用户确认。各 ADR 以此为输入，**但不以它为上限**；MISSION §0.2 只保留摘要。

| 项 | 实测 |
|---|---|
| MacBook Air | M3 / 16 GB / macOS 15.6.1 / arm64；既是节点（跑本地 agent）也是日常客户端所在；本机 105 个 git 仓库 |
| zuan | Ubuntu 24.04.4 LTS / x86_64，112 核 Intel Xeon Gold 5520+，503 GiB 内存，`/` 295 GB + `/home` 2 TB，无 GPU；私网 172.16.103.10，与 Mac（172.16.103.138）同网段可直连；几乎空机（python3 3.12、tmux 3.4；无 Docker / node / agent；sshd 未开） |
| 设备 | 日常 MacBook Air；部分时段只有 iPhone 或 Android 手机；偶尔 iPad |
| agent | 本机装有 Claude Code 2.1.261（9-05 复核；hook fixture 在 `testdata/claude/2.1.261/`，2.1.260 的合成件保留作对照）、Codex CLI 0.152.1（hook fixture 在 `testdata/codex/0.152.1/`）、Grok 1.0.13（`~/.grok/bin/grok`；hook fixture 在 `testdata/grok/1.0.13/`）、cursor-agent（版本 2026-09-02 以 `--version` 复核；9-01 记的 Codex 0.145.0 是过期的更新检查记录）；pi 未装；zuan 上尚无任何 agent |
| 运行时 | 两台机器都可按需安装配置（tmux / Docker / 容器 / SDK 均可），运行时选择不受现状约束（ADR-001） |
| 网络 | Mac（`rickys-macbook-air`）与 iPhone（`iphone171`）已在 tailnet（`tail6f613.ts.net`）；zuan 待装 tailscale；Android 待加入（tailnet 里只有一台 3 年未上线的 `pro`）；tailnet 内还有他人共享的节点与 tagged 设备（不变量 11「网络可达 ≠ 授权」与 ADR-003「没有 loopback 例外」的动机之一）；zuan 出网：Apple 推送 / Let's Encrypt / tailscale 可达，**FCM 不可达**（MISSION §6.6 降级策略的来源） |
| 规模 | 并发 ≤ 30 个会话（本机可达 10、zuan 其余）；多 git worktree 并行是常态 |
