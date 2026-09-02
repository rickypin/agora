# 架构图与 peer 形态

原则见 MISSION §3；为什么选 peer 模型见 ADR-004。

## 总体架构

```
┌───────────────────────────────────────────────────────────────┐
│  CLIENT（Mac 浏览器 / iPhone / Android / iPad —— 同一个 Web） │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │ Dashboard │ Agent Tabs │ xterm.js │ Notifications       │  │
│  │      （一次只连一个节点，看到它 + 它的 peer）            │  │
│  └───────────────┬──────────────────────────┬──────────────┘  │
└──────────────────┼──────────────────────────┼─────────────────┘
          HTTPS / WebSocket            HTTPS / WebSocket
              （任意网络；二选一，不是同时）
                   │                          │
┌──────────────────┼──────────┐   ┌───────────┼──────────────────┐
│  NODE A (macOS)             │   │  NODE B (Linux)   … NODE N   │
│         agora daemon ◄──── peer 链路（同一套 API，机器 token）────► agora daemon │
│   ┌─────────┼─────────┐     │   │   ┌─────────┼─────────┐      │
│ Session   State   Terminal  │   │ Session   State   Terminal   │
│ Manager  Detector Gateway   │   │ Manager  Detector Gateway    │
│   └─────────┼─────────┘     │   │   └─────────┼─────────┘      │
│        运行时【ADR-001】     │   │        运行时【ADR-001】      │
│   ┌──────┼──────┐           │   │   ┌──────┼──────┐            │
│ Claude  Codex  Grok         │   │ Claude  Codex  Grok          │
│ Metadata: SQLite            │   │ Metadata: SQLite             │
└─────────────────────────────┘   └──────────────────────────────┘
   节点互为 peer、一跳、只导出本机会话（MISSION §3.5）；节点数量任意
```

## 当前实例的 peer 形态

| 形态 | 怎么走 |
|---|---|
| Mac 浏览器看自己 + zuan | 开 `127.0.0.1`；Mac 配了 zuan 为 peer。**V1 唯一形态**：只需 Mac 这一行配置，Mac 自己保持只听 loopback |
| Mac 浏览器只看 zuan | 开 zuan（需 zuan 有浏览器可信证书 + TOTP，手机阶段之后才可用） |
| 手机看 Mac + zuan | 开 zuan；zuan 配了 Mac 为 peer（V2 首批） |
| 手机只看一个 | 开该节点（V2 首批） |

两台互为 peer 不是新机制，就是两边各写一行配置。Mac 带出门且 zuan 够不着它时，zuan 上显示 Mac 为"上次见到"。
