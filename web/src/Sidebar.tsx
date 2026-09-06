import { Fragment, useEffect, useState, type ReactNode, type RefObject } from "react";
import type { AdoptBody } from "./api";
import { countByStatus, needsAttention, taskLabel } from "./attention";
import type { SessionRow, UnregisteredRow } from "./events";
import { Header, type NodeStatus } from "./Header";
import { rowName, SidebarRow, str } from "./SessionRow";

// 行组件与它的两个小工具搬去了 SessionRow.tsx（agora-h1k.3 接缝，2026-09-06）；Tabs / CommandPalette /
// SessionSettings 仍从这里 import，所以原样再导出一次，调用方一行不改。
export { rowName, SidebarRow, statusSymbol } from "./SessionRow";

/** 侧栏过滤匹配的字段（docs/spec/ux.md：name / node / agent / preview）：任务标签与两行预览也算。 */
export function rowHaystack(s: SessionRow): string {
  return [
    taskLabel(s),
    rowName(s),
    String(s.agent_type ?? ""),
    s.node,
    String(s.reason ?? s.status),
    str(s.prompt),
    str(s.progress),
    str(s.preview),
  ].join(" ");
}

/** 每 30 s 走一次的时钟，只为"waiting 3m"这种时长文案。 */
function useNowSeconds(periodMs = 30_000): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const t = setInterval(() => setNow(Math.floor(Date.now() / 1000)), periodMs);
    return () => clearInterval(t);
  }, [periodMs]);
  return now;
}

/** header 的计数行（docs/spec/ux.md 线框：Running 5 · Needs Input 2 · …）。 */
function CountsLine({ rows }: { rows: SessionRow[] }) {
  const c = countByStatus(rows);
  const parts: [string, number][] = [
    ["Running", c.running],
    ["Needs Input", c.needsInput],
    ["Turn Done", c.turnDone],
    ["Finished", c.finished],
    ["Failed", c.failed],
    ["Idle", c.idle],
    ["Unknown", c.unknown],
  ];
  return (
    <p className="counts muted" data-testid="counts">
      {parts
        .filter(([, n]) => n > 0)
        .map(([label, n]) => `${label} ${n}`)
        .join(" · ") || "no agents"}
    </p>
  );
}

interface SidebarProps {
  /** 已经按 attention 排好、NEEDS ATTENTION 在前的显示顺序——Alt/Option+N 跳的就是这个顺序
   * （agora-xqa.14 验收）；本组件只在分区交界处插标题。 */
  rows: SessionRow[];
  /** 过滤前的全部行：header 计数用。 */
  all?: SessionRow[];
  /** Header 上本机与每个 peer 的状态（MISSION §10.3）；只透传给 Header。 */
  nodes?: NodeStatus[];
  /** 本机 node.id（`/api/system` 的 node）；只透传给每一行，行据此决定标不标 `@ node`（agora-7ku.5）。 */
  localNode?: string;
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
  /** 选中行的展开区。 */
  renderExpanded?: (row: SessionRow) => ReactNode;
  /** 运行时里未登记的会话（Unknown Agent，MISSION §5.5）。 */
  unregistered?: UnregisteredRow[];
  onAdopt?: (body: AdoptBody) => void;
}

interface UnknownProps {
  row: UnregisteredRow;
  onAdopt?: (body: AdoptBody) => void;
}

/** `? name / Unknown Agent`（docs/spec/ux.md）+ 采纳表单：display name / project / agent type。 */
function UnknownRow({ row, onAdopt }: UnknownProps) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState(row.title || row.name);
  const [project, setProject] = useState(row.working_directory);
  const [agent, setAgent] = useState(row.agent_hint ?? "");
  const label = row.agent_hint ? `Unknown Agent（像 ${row.agent_hint}）` : "Unknown Agent";
  return (
    <li className="unknown">
      <button className="row" onClick={() => setOpen((v) => !v)} title={row.runtime_ref} data-testid={`unreg-${row.runtime_ref}`}>
        <span className="dot st-unknown">?</span>
        <span className="row-main">
          <span className="name">{row.name}</span>
          <span className="meta">
            <span>{label}</span>
            <span className="node">@ {row.node}</span>
          </span>
        </span>
      </button>
      {open && (
        <form
          className="adopt"
          data-testid={`adopt-${row.runtime_ref}`}
          onSubmit={(e) => {
            e.preventDefault();
            onAdopt?.({
              runtime_ref: row.runtime_ref,
              display_name: name.trim() || undefined,
              project: project.trim() || undefined,
              agent_type: agent.trim() || undefined,
            });
            setOpen(false);
          }}
        >
          <label>
            Name <input value={name} onChange={(e) => setName(e.target.value)} aria-label="采纳：名字" />
          </label>
          <label>
            Project <input value={project} onChange={(e) => setProject(e.target.value)} aria-label="采纳：项目" />
          </label>
          <label>
            Agent <input value={agent} onChange={(e) => setAgent(e.target.value)} placeholder="unknown" aria-label="采纳：agent 类型" />
          </label>
          <div className="adopt-actions">
            <button type="button" onClick={() => setOpen(false)}>
              取消
            </button>
            <button type="submit">采纳</button>
          </div>
        </form>
      )}
    </li>
  );
}

export function Sidebar({
  rows,
  all = rows,
  nodes,
  localNode,
  active,
  onOpen,
  onNewAgent,
  onRowRender,
  filter,
  onFilter,
  filterRef,
  onFilterEnter,
  total,
  renderExpanded,
  unregistered = [],
  onAdopt,
}: SidebarProps) {
  const now = useNowSeconds();
  const firstRunning = rows.findIndex((r) => !needsAttention(r));
  const hasAttention = rows.length > 0 && needsAttention(rows[0]);
  return (
    <aside className="sidebar">
      <Header agents={filter ? `${rows.length}/${total}` : total} nodes={nodes} />
      <CountsLine rows={all} />
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
      {total === 0 && unregistered.length === 0 && <p className="muted pad">还没有会话。</p>}
      {total > 0 && rows.length === 0 && <p className="muted pad">没有匹配的会话。</p>}
      {hasAttention && (
        <div className="sidebar-head section" data-testid="section-attention">
          <span className="muted">NEEDS ATTENTION</span>
        </div>
      )}
      <ul>
        {rows.map((r, i) => (
          <Fragment key={r.id}>
            {i === firstRunning && hasAttention && (
              <li className="section-row" data-testid="section-running">
                <span className="muted">RUNNING</span>
              </li>
            )}
            <SidebarRow
              row={r}
              active={r.id === active}
              ordinal={i + 1}
              onOpen={onOpen}
              onRender={onRowRender}
              expanded={r.id === active ? renderExpanded?.(r) : undefined}
              now={now}
              localNode={localNode}
            />
          </Fragment>
        ))}
      </ul>
      {unregistered.length > 0 && !filter && (
        <>
          <div className="sidebar-head">
            <span className="muted">UNREGISTERED {unregistered.length}</span>
          </div>
          <ul>
            {unregistered.map((u) => (
              <UnknownRow key={u.runtime_ref} row={u} onAdopt={onAdopt} />
            ))}
          </ul>
        </>
      )}
    </aside>
  );
}
