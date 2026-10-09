import { useEffect, useState } from "react";
import { anchoredNow, sectionOf, seenKey, taskLabel, type SeenSet, type ServerClock } from "./attention";
import type { SessionRow } from "./events";
import { mobileStatusText } from "./mobileStatus";
import { statusClass } from "./RowIdentity";
import { statusSymbol } from "./SessionRow";
import { visibleOrder } from "./sidebarMode";

/** 首页只给出下一步；点卡片走 Workspace 的唯一选中集合，不另存一份 active。 */
export function DesktopOverview({ rows, seen, serverClock, onOpen, onNewAgent, onPalette }: {
  rows: SessionRow[];
  seen: SeenSet;
  serverClock: ServerClock | null;
  onOpen: (id: string) => void;
  onNewAgent: () => void;
  onPalette: () => void;
}) {
  const [pageNow, setPageNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const timer = setInterval(() => setPageNow(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(timer);
  }, []);
  const now = serverClock ? anchoredNow(serverClock, pageNow) : pageNow;
  const ordered = visibleOrder("attention", rows, "", seen);
  const attention = ordered.filter((r) => sectionOf(r, seen, now) === "attention");
  const unclear = rows.filter((r) => sectionOf(r, seen, now) === "unclear").length;
  const running = rows.filter((r) => r.status === "running" || r.status === "starting").length;
  return (
    <div className="desktop-overview" data-testid="desktop-overview">
      <header className="overview-heading">
        <div><p className="eyebrow">你的工作台</p><h1>{attention.length ? `${attention.length} 个会话需要你` : rows.length ? "暂时没有待处理事项" : "从第一个任务开始"}</h1>
          <p className="muted">{rows.length ? "先处理请求，再看结果。运行中的任务会继续。" : "选择项目与 agent，让任务在节点上持续运行。"}</p></div>
        <button className="primary" onClick={onNewAgent}>+ 新建会话</button>
      </header>
      <div className="overview-summary" aria-label="会话概览">
        <span><strong>{rows.length}</strong> 全部会话</span><span><strong>{running}</strong> 运行中</span><span><strong>{unclear}</strong> 状态待确认</span>
      </div>
      <div className="overview-section-head"><h2>需要我</h2><span className="muted">按处理优先级排列</span></div>
      {attention.length ? (
        <div className="attention-cards">
          {attention.map((r) => (
            <button className="attention-card" key={r.id} onClick={() => onOpen(r.id)} data-testid={`overview-${r.id}`}>
              <span className="attention-card-top"><span className={statusClass(r.status)}>{statusSymbol(r.status)} {mobileStatusText(r, seen.has(seenKey(r)))}</span><span className="muted">{String(r.agent_type ?? "agent")} · {r.node}</span></span>
              <strong>{taskLabel(r)}</strong>
              <span className="attention-card-context muted">{r.project ? `${r.project.name} · ${r.project.branch ?? "detached"}` : "独立会话"}</span>
              <span className="attention-card-action">{r.status === "waiting" ? "查看请求" : r.status === "failed" ? "检查失败原因" : "阅读结果"}<span aria-hidden="true">↗</span></span>
            </button>
          ))}
        </div>
      ) : <div className="overview-clear"><span aria-hidden="true">✓</span><h2>{rows.length ? "可以继续专注了" : "还没有会话。"}</h2><p className="muted">{rows.length ? "从左侧选一个 agent，查看运行情况或继续之前的任务。" : "新建会话后，你可以在这里查看请求和结果。"}</p></div>}
      <footer className="overview-footer"><div><strong>快速到达任何会话</strong><p className="muted">按项目浏览，或搜索任务、节点与 agent。</p></div><button onClick={onPalette}>搜索与命令 <kbd>⌘ / Ctrl K</kbd></button></footer>
    </div>
  );
}
