import { memo, useState, type ReactNode } from "react";
import { promptRepeatsLabel, statusLine, taskLabel, taskOf } from "./attention";
import type { SessionRow } from "./events";
import { clockText } from "./Header";

/**
 * 侧栏的一行会话（docs/spec/ux.md「Attention Dashboard 线框」）。
 *
 * 2026-09-06 从 Sidebar.tsx 原样抽出（agora-h1k.3 的接缝 commit，不改行为、DOM 不变）：
 * 后续往行上加东西——验收标准折叠块（h1k.3）、node 标签（7ku.5）、stale「上次见到」（7ku.6）——
 * 都只改这个文件，Sidebar.tsx 只管列表、分区标题与未注册段。
 *
 * node 标签（MISSION §3.5 "每行标明节点"；agora-7ku.5）：只给 `node !== localNode` 的行——本机的
 * 行一律不标，满屏 `@ mac` 是噪音；`localNode` 是 `/api/system` 报的本机 node.id，还没拉到时谁都不
 * 标（先满屏 @ 再消失更难看）。
 *
 * stale 行（MISSION §3.5 "peer 断线保留最后视图并标记（上次见到 23:10）"；不变量 8；agora-7ku.6）：
 * `stale: true` 的 peer 行整行淡显（`<li class="stale">`，index.css）但一条不少，`.meta` 里多一段
 * `○ 上次见到 HH:MM`——时间是行上的 `last_seen`（节点按本机时钟打的 UTC 文本，与 `/api/health`
 * peers 段同一个值），用 Header 同一个 `clockText` 转本地时区，完整 UTC 放 title。点开 stale 行
 * 不需要前端做任何事：TerminalView 建终端 WS / 任何写操作到节点就触发一次重试（`forward::hop`）。
 */

/** stale 行 `.meta` 里那一段的文案与 title；非 stale 行返回 null（不占位）。 */
export function staleSeen(row: SessionRow): { text: string; title: string } | null {
  if (!row.stale) return null;
  const seen = typeof row.last_seen === "string" ? row.last_seen : null;
  // 有 last_seen 才有"上次见到"；没有（理论上不会——连上过才有行可标）就只说离线，不编时间。
  if (seen === null) return { text: "○ 离线", title: "该节点离线，正在重连" };
  return { text: `○ 上次见到 ${clockText(seen)}`, title: `该节点离线，正在重连；上次见到 ${seen}` };
}

/** `<li>` 的类名：选中 / stale 两个正交的状态。 */
export function rowClasses(active: boolean, stale?: boolean): string | undefined {
  const cls = [active ? "selected" : null, stale ? "stale" : null].filter((c): c is string => c !== null);
  return cls.length ? cls.join(" ") : undefined;
}

/** 状态符号（docs/spec/ux.md 线框）。 */
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

export function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/**
 * 展开区里的验收标准（MISSION §6.3「看结果」；A40；agora-h1k.3）：`task.acceptance` 是 `bd show`
 * 的 acceptance_criteria 全文，读自 beads、不复制进 agora 的库（不变量 12）。位置在就地 respond
 * 之下（docs/spec/ux.md）：回答问题 / 给下一条指令是先做的事，对照验收是看结果时的事。
 * 默认展开——行展开就是为了看"做完算什么"；一行 summary 可折叠。没有任务或没写验收标准不占位。
 * 折叠状态自己管而不用 <details>：内容折起来就真的不在 DOM 里，测试与读屏都不用猜 open 属性。
 */
function Acceptance({ row }: { row: SessionRow }) {
  const [open, setOpen] = useState(true);
  const task = taskOf(row);
  const text = typeof task?.acceptance === "string" ? task.acceptance.trim() : "";
  if (!task || !text) return null;
  return (
    <div className="acceptance" data-testid={`acceptance-${row.id}`} onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        className="acceptance-toggle muted"
        aria-expanded={open}
        data-testid={`acceptance-toggle-${row.id}`}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? "▾" : "▸"} 验收标准 · {task.id}
      </button>
      {open && (
        <pre className="acceptance-body" data-testid={`acceptance-body-${row.id}`}>
          {text}
        </pre>
      )}
    </div>
  );
}

interface RowProps {
  row: SessionRow;
  active: boolean;
  /** 1…9 显示成 Alt/Option 跳转的序号；其余不显示（MISSION §6.5）。 */
  ordinal: number;
  onOpen: (id: string) => void;
  /** 测试注入：数渲染次数。 */
  onRender?: (id: string) => void;
  /** 选中行下方的展开区（就地回答，MISSION §6.3）。 */
  expanded?: ReactNode;
  /** unix 秒；"waiting 3m"的基准。 */
  now: number;
  /** 本机 node.id（`/api/system` 的 node）：node 标签只给不是本机的行；undefined = 还不知道，谁都不标。 */
  localNode?: string;
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, ordinal, onOpen, onRender, expanded, now, localNode }: RowProps) {
  onRender?.(row.id);
  const prompt = str(row.prompt);
  const progress = str(row.progress);
  const preview = str(row.preview);
  // 装了 hook 却从没收到过事件（Codex 未在 /hooks 信任是最常见的一种，agora-dvh.15）：
  // 行上一行醒目提示，全文放 title；服务端判定，前端不猜。
  const unheard = str(row.hooks_unheard);
  // 两行预览读自 hook（❯ 用户最后输入 / ↳ agent 正在做或最后说的）；没有 hook 的会话保持一行
  // pane preview；两者都没有时退回状态理由（MISSION §6.3；ADR-002 D8）。
  const lines = prompt || progress ? null : preview || String(row.reason ?? "");
  const seen = staleSeen(row);
  return (
    <li className={rowClasses(active, row.stale)}>
      <button
        className={active ? "row selected" : "row"}
        onClick={() => onOpen(row.id)}
        title={`${rowName(row)} (${row.id})`}
        data-testid={`row-${row.id}`}
      >
        <span className={`dot st-${row.status}`}>{statusSymbol(row.status)}</span>
        <span className="row-main">
          <span className="name" data-testid={`label-${row.id}`}>
            {taskLabel(row)}
          </span>
          <span className="meta">
            <span>{String(row.agent_type ?? "")}</span>
            {row.origin === "external" && <span className="origin">external</span>}
            {localNode !== undefined && row.node !== localNode && (
              <span className="node" data-testid={`row-node-${row.id}`}>
                @ {row.node}
              </span>
            )}
            {seen && (
              <span className="stale-seen" data-testid={`row-stale-${row.id}`} title={seen.title}>
                {seen.text}
              </span>
            )}
            <span className={`state st-${row.status}`} data-testid={`state-${row.id}`}>
              {statusLine(row, now)}
            </span>
          </span>
          {prompt && !promptRepeatsLabel(row) && (
            <span className="preview line-prompt" data-testid={`prompt-${row.id}`}>
              <span className="muted">❯ </span>
              {prompt}
            </span>
          )}
          {progress && (
            <span className="preview line-progress" data-testid={`progress-${row.id}`}>
              <span className="muted">↳ </span>
              {progress}
            </span>
          )}
          {lines && (
            <span className="preview muted" data-testid={`preview-${row.id}`}>
              {lines}
            </span>
          )}
          {unheard && (
            <span className="preview hooks-unheard" data-testid={`hooks-unheard-${row.id}`} title={unheard}>
              ⚠ hook 没接上：{unheard}
            </span>
          )}
        </span>
        <span className="ord muted">{ordinal >= 1 && ordinal <= 9 ? ordinal : ""}</span>
      </button>
      {active && expanded}
      {active && <Acceptance row={row} />}
    </li>
  );
});
