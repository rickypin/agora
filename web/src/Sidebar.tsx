import { Fragment, useEffect, useRef, useState, type PointerEvent, type RefObject } from "react";
import type { AdoptBody, SessionApi, WriteResult } from "./api";
import { countByStatus, sectionOf, type SeenSet } from "./attention";
import { ConfirmDialog } from "./ConfirmDialog";
import type { SessionRow, UnregisteredRow } from "./events";
import { Header, type NodeStatus } from "./Header";
import { isDesktop } from "./keys";
import type { NewAgentInitial } from "./NewAgentDialog";
import { SidebarRow } from "./SessionRow";
import type { SidebarMode } from "./sidebarMode";
import { SidebarTree } from "./SidebarTree";
import { clearWidth, clamp, DEFAULT, loadWidth, storeWidth } from "./sidebarWidth";

// 行组件与它的两个小工具搬去了 SessionRow.tsx（agora-h1k.3 接缝，2026-09-06）；CommandPalette /
// SessionSettings 仍从这里 import，所以原样再导出一次，调用方一行不改。rowHaystack 也住在
// SessionRow（agora-uvd.2：sidebarMode 不能从本文件进，否则 uvd.3 一加运行时 import 就成环）。
export { rowHaystack, rowName, SidebarRow, statusSymbol } from "./SessionRow";

/** 每 30 s 走一次的时钟，只为"waiting 3m"这种时长文案。 */
function useNowSeconds(periodMs = 30_000): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const t = setInterval(() => setNow(Math.floor(Date.now() / 1000)), periodMs);
    return () => clearInterval(t);
  }, [periodMs]);
  return now;
}

/**
 * header 的计数行（docs/spec/ux.md 线框：Running 5 · Needs Input 2 · …）。`Finished N` 仍按状态数，但折叠区
 * 里有行时它是按钮——一键清理（A46，agora-j4w.2）：点了弹确认框，确认后对折叠区里的每一行各发一次
 * DELETE metadata。没有可清理的行就是普通文字。
 */
function CountsLine({ rows, clearable, onClear }: { rows: SessionRow[]; clearable: number; onClear?: () => void }) {
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
  const shown = parts.filter(([, n]) => n > 0);
  return (
    <p className="counts muted" data-testid="counts">
      {shown.length === 0 && "no agents"}
      {shown.map(([label, n], i) => (
        <Fragment key={label}>
          {i > 0 && " · "}
          {label === "Finished" && clearable > 0 && onClear ? (
            <button type="button" className="clear-finished" data-testid="clear-finished" title={`清理 Finished 区里的 ${clearable} 行（删记录，不 kill）`} onClick={onClear}>
              {label} {n}
            </button>
          ) : (
            `${label} ${n}`
          )}
        </Fragment>
      ))}
    </p>
  );
}

/** 一键清理跑完的一句话：清了几行、跳过几行（peer 离线）、失败几行。 */
export function clearSummary(r: { removed: number; skipped: number; failed: number }): string {
  const parts = [`已清理 ${r.removed} 行`];
  if (r.skipped > 0) parts.push(`跳过 ${r.skipped} 行（节点离线）`);
  if (r.failed > 0) parts.push(`失败 ${r.failed} 行`);
  return parts.join("，");
}

interface SidebarProps {
  /** 已经按 attention 排好、NEEDS ATTENTION → RUNNING → FINISHED 三段拼好的显示顺序——Alt/Option+N 跳的
   * 就是这个顺序（agora-xqa.14 验收）；本组件只在分区交界处插标题、把 Finished 段折起来。 */
  rows: SessionRow[];
  /** 侧栏视图（A47，agora-uvd.2）：缺省 attention，既有测试一行不改。tree 时不画三段标题、平铺全部行。 */
  mode?: SidebarMode;
  onMode?: (mode: SidebarMode) => void;
  /** 看过的 FINISHED 行（MISSION §4.6；A46）：与 Workspace 拼 rows 时用的是同一个集合，交界才对得上。 */
  seen?: SeenSet;
  /** 过滤前的全部行：header 计数用。 */
  all?: SessionRow[];
  /** Header 上本机与每个 peer 的状态（MISSION §10.3）；只透传给 Header。 */
  nodes?: NodeStatus[];
  /** 本机 node.id（`/api/system` 的 node）；只透传给每一行，行据此决定标不标 `@ node`（agora-7ku.5）。 */
  localNode?: string;
  active: string | null;
  onOpen: (id: string) => void;
  /** 打开 New Agent 对话框；树视图的组头带一份预填过来（A48，agora-uvd.4），别处不传参、行为不变。 */
  onNewAgent?: (initial?: NewAgentInitial) => void;
  onRowRender?: (id: string) => void;
  filter: string;
  onFilter: (v: string) => void;
  /** Cmd/Ctrl+F 把焦点放进来。 */
  filterRef?: RefObject<HTMLInputElement | null>;
  /** 过滤框里按 Enter：打开第一条（docs/spec/ux.md）。 */
  onFilterEnter?: () => void;
  /** 过滤前的总数：过滤后要让人看得出"还有多少被藏起来了"。 */
  total: number;
  /** 运行时里未登记的会话（Unknown Agent，MISSION §5.5）。 */
  unregistered?: UnregisteredRow[];
  onAdopt?: (body: AdoptBody) => void;
  /** 「看 diff」（agora-h1k.5）；只透传给每一行。 */
  onOpenDiff?: (id: string) => void;
  /** 一键清理用的 DELETE metadata（agora-j4w.2）；不给就没有清理按钮。 */
  onDeleteMetadata?: (id: string) => Promise<WriteResult<unknown>>;
  /** 树视图组头「shell」用的写端点（agora-uvd.4）；只透传给 <SidebarTree>，不给就没有那个按钮。 */
  api?: Pick<SessionApi, "create">;
  /** 组头「shell」起成功了：交给 Workspace 的 pendingOpen（同 New Agent 对话框的 onCreated）。 */
  onCreated?: (id: string) => void;
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
  mode = "attention",
  onMode,
  seen,
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
  unregistered = [],
  onAdopt,
  onOpenDiff,
  onDeleteMetadata,
  api,
  onCreated,
}: SidebarProps) {
  const now = useNowSeconds();
  // 一键清理的对象是折叠区的定义本身（已看过的 agora / adopted FINISHED + 全部 external FINISHED），按过滤
  // 前的 all 算——过滤只是暂时少画几行，不改变哪些行「可以清」；NEEDS ATTENTION 里没看过的 FINISHED 不碰
  // （MISSION §4.6「不得在用户看到结果之前清理」）。没有批量端点也不加：逐行 DELETE /api/sessions/:id
  // （MISSION §11 不引入 Archive）；peer stale 的行跳过（一跳转发到不了）并在结果里说明。
  // 树视图下同样成立（agora-uvd.3）：清理对象是折叠区的定义，与视图无关——树里 FINISHED 行只是原位淡显。
  const clearable = all.filter((r) => sectionOf(r, seen) === "finished");
  const [clearAsk, setClearAsk] = useState(false);
  const [clearNote, setClearNote] = useState<string | null>(null);
  useEffect(() => {
    if (clearNote === null) return;
    const t = setTimeout(() => setClearNote(null), 8000);
    return () => clearTimeout(t);
  }, [clearNote]);
  async function clearFinished() {
    setClearAsk(false);
    if (!onDeleteMetadata) return;
    const result = { removed: 0, skipped: 0, failed: 0 };
    for (const r of clearable) {
      if (r.stale) {
        result.skipped += 1;
        continue;
      }
      const w = await onDeleteMetadata(r.id);
      if (w.ok) result.removed += 1;
      else result.failed += 1;
    }
    setClearNote(clearSummary(result));
  }
  const ownClearable = clearable.filter((r) => r.origin !== "external").length;
  // 三段的交界：rows 已经按 attention → running → finished 拼好（partitionByAttention），这里只在交界处插标题。
  // tree 视图整段列表换成 <SidebarTree>（A48，agora-uvd.3）：组头、折叠与 DFS 序号都在那边；下面 attention
  // 分支一个字不动。
  const showSections = mode !== "tree";
  const sections = rows.map((r) => sectionOf(r, seen));
  const firstRunning = sections.indexOf("running");
  const firstFinished = sections.indexOf("finished");
  const hasAttention = sections[0] === "attention";
  const finishedCount = firstFinished < 0 ? 0 : rows.length - firstFinished;
  // Finished 区默认收起（A46：2026-09-08 现场 58 行里 39 行是 external FINISHED，摊开就是它们占满第一屏）；
  // 折叠状态只活在这个组件里，刷新页面回到收起。收起时行不画但序号照数：Alt/Option+N 的第 N 条与
  // 展开时一样（MISSION §6.5；折叠不改变序号）。
  const [finishedOpen, setFinishedOpen] = useState(false);
  // 选中行落进收起的 Finished 区时自动展开（agora-4nk，2026-09-08 对抗审查实证）：Alt/Option+N、finished
  // 通知点击、命令面板都能选中折叠区里的行，收起时那一行不画——主区 crumb 有它、侧栏没有 selected 行、
  // 展开区也不渲染。只在 active 变化时展开一次而不是派生成「active 在里面就一直开」：后者会让人点标题
  // 收不起来（点了 aria-expanded 还是 true）。人展开后再手动收起是人的选择，行不画但序号照数。
  const activeInFinished = active !== null && sections[rows.findIndex((r) => r.id === active)] === "finished";
  useEffect(() => {
    if (activeInFinished) setFinishedOpen(true);
  }, [active, activeInFinished]);
  // 侧栏宽度：挂载时把上次拖出来的 px 写进 --sidebar-w；拖柄只在桌面断点渲染（agora-uvd.5）。
  useEffect(() => {
    applySidebarWidth(loadWidth());
  }, []);
  const drag = useRef<{ startX: number; startW: number; last: number } | null>(null);
  function onResizerPointerDown(e: PointerEvent<HTMLDivElement>) {
    const startX = e.clientX;
    if (!Number.isFinite(startX)) return;
    const startW = currentSidebarWidth();
    drag.current = { startX, startW, last: startW };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // jsdom 没有指针捕获；测试直接往拖柄上 fireEvent.pointerMove。
    }
    document.body.style.userSelect = "none";
  }
  function onResizerPointerMove(e: PointerEvent<HTMLDivElement>) {
    if (!drag.current) return;
    const x = e.clientX;
    if (!Number.isFinite(x)) return;
    const w = clamp(drag.current.startW + (x - drag.current.startX));
    drag.current.last = w;
    applySidebarWidth(w);
  }
  function onResizerPointerUp() {
    if (!drag.current) return;
    storeWidth(drag.current.last);
    drag.current = null;
    document.body.style.userSelect = "";
  }
  function onResizerDoubleClick() {
    drag.current = null;
    document.body.style.userSelect = "";
    applySidebarWidth(DEFAULT);
    clearWidth();
  }
  return (
    <aside className="sidebar">
      <Header agents={filter ? `${rows.length}/${total}` : total} nodes={nodes} />
      <CountsLine rows={all} clearable={clearable.length} onClear={onDeleteMetadata ? () => setClearAsk(true) : undefined} />
      <div className="sidebar-mode" role="group" aria-label="侧栏视图">
        <button
          type="button"
          aria-pressed={mode === "attention"}
          data-testid="sidebar-mode-attention"
          onClick={() => onMode?.("attention")}
        >
          需要我
        </button>
        <button
          type="button"
          aria-pressed={mode === "tree"}
          data-testid="sidebar-mode-tree"
          onClick={() => onMode?.("tree")}
        >
          按项目
        </button>
      </div>
      {clearNote !== null && (
        <p className="muted pad clear-note" data-testid="clear-finished-note" role="status">
          {clearNote}
        </p>
      )}
      {clearAsk && (
        <ConfirmDialog
          title="清理 Finished 区"
          body={`将删除 Finished 区里 ${clearable.length} 行的记录${ownClearable > 0 ? `（其中 ${ownClearable} 行是 agora 起的会话，它们已退出的运行时会话与输出会一并清掉）` : ""}。只删记录、不 kill；NEEDS ATTENTION 里没看过的 FINISHED 行不动。不可撤销。`}
          confirmLabel={`Delete ${clearable.length}`}
          onConfirm={() => void clearFinished()}
          onCancel={() => setClearAsk(false)}
        />
      )}
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
      {/* 不写成 onClick={onNewAgent}：那会把 MouseEvent 当预填传进去（agora-uvd.4 加了参数之后）。 */}
      <button className="new-agent" onClick={() => onNewAgent?.()}>
        + New Agent
      </button>
      {total === 0 && unregistered.length === 0 && <p className="muted pad">还没有会话。</p>}
      {total > 0 && rows.length === 0 && <p className="muted pad">没有匹配的会话。</p>}
      {showSections && hasAttention && (
        <div className="sidebar-head section" data-testid="section-attention">
          <span className="muted">NEEDS ATTENTION</span>
        </div>
      )}
      {mode === "tree" ? (
        <SidebarTree
          rows={rows}
          all={all}
          nodes={nodes}
          localNode={localNode}
          active={active}
          seen={seen}
          onOpen={onOpen}
          onRowRender={onRowRender}
          now={now}
          onOpenDiff={onOpenDiff}
          onNewAgent={onNewAgent}
          api={api}
          onCreated={onCreated}
        />
      ) : (
      <ul>
        {rows.map((r, i) => (
          <Fragment key={r.id}>
            {showSections && i === firstRunning && hasAttention && (
              <li className="section-row" data-testid="section-running">
                <span className="muted">RUNNING</span>
              </li>
            )}
            {showSections && i === firstFinished && (
              <li className="section-row finished-head">
                <button
                  type="button"
                  className="finished-toggle muted"
                  aria-expanded={finishedOpen}
                  data-testid="section-finished"
                  title={finishedOpen ? "收起已完成的会话" : "展开已完成的会话"}
                  onClick={() => setFinishedOpen((v) => !v)}
                >
                  {finishedOpen ? "▾" : "▸"} FINISHED {finishedCount}
                </button>
              </li>
            )}
            {showSections && sections[i] === "finished" && !finishedOpen ? null : (
            <SidebarRow
              row={r}
              active={r.id === active}
              ordinal={i + 1}
              onOpen={onOpen}
              onRender={onRowRender}
              now={now}
              localNode={localNode}
              onOpenDiff={onOpenDiff}
            />
            )}
          </Fragment>
        ))}
      </ul>
      )}
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
      {isDesktop() && (
        <div
          className="sidebar-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="调整侧栏宽度"
          data-testid="sidebar-resizer"
          onPointerDown={onResizerPointerDown}
          onPointerMove={onResizerPointerMove}
          onPointerUp={onResizerPointerUp}
          onPointerCancel={onResizerPointerUp}
          onDoubleClick={onResizerDoubleClick}
        />
      )}
    </aside>
  );
}

function applySidebarWidth(w: number): void {
  document.documentElement.style.setProperty("--sidebar-w", `${w}px`);
}

function currentSidebarWidth(): number {
  const raw = document.documentElement.style.getPropertyValue("--sidebar-w").trim();
  const n = Number.parseFloat(raw);
  return Number.isFinite(n) ? n : loadWidth();
}
