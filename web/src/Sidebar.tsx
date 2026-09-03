import { memo } from "react";
import type { SessionRow } from "./events";

/** 状态符号（docs/spec/ux.md 线框）。attention 排序与两行预览归 M1b。 */
export function statusSymbol(status: string): string {
  switch (status) {
    case "waiting":
      return "⚠";
    case "turn_done":
      return "◆";
    case "running":
      return "●";
    case "starting":
      return "…";
    case "idle":
      return "○";
    case "finished":
      return "✓";
    case "failed":
      return "✗";
    default:
      return "?";
  }
}

export function rowName(s: SessionRow): string {
  return String(s.name ?? s.display_name ?? s.id);
}

interface RowProps {
  row: SessionRow;
  active: boolean;
  onOpen: (id: string) => void;
  /** 测试注入：数渲染次数。 */
  onRender?: (id: string) => void;
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, onOpen, onRender }: RowProps) {
  onRender?.(row.id);
  return (
    <li>
      <button
        className={active ? "row selected" : "row"}
        onClick={() => onOpen(row.id)}
        title={row.id}
        data-testid={`row-${row.id}`}
      >
        <span className={`dot st-${row.status}`}>{statusSymbol(row.status)}</span>
        <span className="row-main">
          <span className="name">{rowName(row)}</span>
          <span className="meta">
            <span>{String(row.agent_type ?? "")}</span>
            <span className="node">@ {row.node}</span>
          </span>
          <span className="preview muted">{String(row.reason ?? row.status)}</span>
        </span>
      </button>
    </li>
  );
});

interface SidebarProps {
  rows: SessionRow[];
  active: string | null;
  onOpen: (id: string) => void;
  onRowRender?: (id: string) => void;
}

export function Sidebar({ rows, active, onOpen, onRowRender }: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <h1>agora</h1>
        <span className="muted">AGENTS {rows.length}</span>
      </div>
      {rows.length === 0 && <p className="muted pad">还没有会话。</p>}
      <ul>
        {rows.map((r) => (
          <SidebarRow key={r.id} row={r} active={r.id === active} onOpen={onOpen} onRender={onRowRender} />
        ))}
      </ul>
    </aside>
  );
}
