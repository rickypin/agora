import { memo, type RefObject } from "react";
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

/** 侧栏过滤匹配的字段（docs/spec/ux.md：name / node / agent / preview）。 */
export function rowHaystack(s: SessionRow): string {
  return [rowName(s), String(s.agent_type ?? ""), s.node, String(s.reason ?? s.status)].join(" ");
}

interface RowProps {
  row: SessionRow;
  active: boolean;
  /** 1…9 显示成 Alt/Option 跳转的序号；其余不显示（MISSION §6.5）。 */
  ordinal: number;
  onOpen: (id: string) => void;
  /** 测试注入：数渲染次数。 */
  onRender?: (id: string) => void;
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, ordinal, onOpen, onRender }: RowProps) {
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
        <span className="ord muted">{ordinal >= 1 && ordinal <= 9 ? ordinal : ""}</span>
      </button>
    </li>
  );
});

interface SidebarProps {
  /** 已经按过滤后的显示顺序——Alt/Option+N 跳的就是这个顺序（agora-xqa.14 验收）。 */
  rows: SessionRow[];
  active: string | null;
  onOpen: (id: string) => void;
  onNewAgent?: () => void;
  onRowRender?: (id: string) => void;
  filter: string;
  onFilter: (v: string) => void;
  /** Cmd/Ctrl+F 把焦点放进来。 */
  filterRef?: RefObject<HTMLInputElement | null>;
  /** 过滤框里按 Enter：打开第一条（docs/spec/ux.md）。 */
  onFilterEnter?: () => void;
  /** 过滤前的总数：过滤后要让人看得出"还有多少被藏起来了"。 */
  total: number;
}

export function Sidebar({
  rows,
  active,
  onOpen,
  onNewAgent,
  onRowRender,
  filter,
  onFilter,
  filterRef,
  onFilterEnter,
  total,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <h1>agora</h1>
        <span className="muted">AGENTS {filter ? `${rows.length}/${total}` : total}</span>
      </div>
      <input
        ref={filterRef}
        className="filter"
        value={filter}
        placeholder="过滤（Cmd/Ctrl+F）"
        aria-label="过滤侧栏"
        data-testid="sidebar-filter"
        onChange={(e) => onFilter(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onFilterEnter?.();
          } else if (e.key === "Escape") {
            e.preventDefault();
            onFilter("");
            e.currentTarget.blur();
          }
        }}
      />
      <button className="new-agent" onClick={onNewAgent}>
        + New Agent
      </button>
      {total === 0 && <p className="muted pad">还没有会话。</p>}
      {total > 0 && rows.length === 0 && <p className="muted pad">没有匹配的会话。</p>}
      <ul>
        {rows.map((r, i) => (
          <SidebarRow
            key={r.id}
            row={r}
            active={r.id === active}
            ordinal={i + 1}
            onOpen={onOpen}
            onRender={onRowRender}
          />
        ))}
      </ul>
    </aside>
  );
}
