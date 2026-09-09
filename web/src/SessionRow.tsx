import { memo, useState, type ReactNode } from "react";
import { promptRepeatsLabel, taskLabel, taskOf } from "./attention";
import { Changes } from "./Changes";
import type { SessionRow } from "./events";
import { RowIdentity } from "./RowIdentity";

export { staleSeen } from "./RowIdentity";

/**
 * 侧栏的一行会话（docs/spec/ux.md「Attention Dashboard 线框」）。
 *
 * 2026-09-06 从 Sidebar.tsx 原样抽出（agora-h1k.3 的接缝 commit，不改行为、DOM 不变）：
 * 后续往行上加东西——验收标准折叠块（h1k.3）、node 标签（7ku.5）、stale「上次见到」（7ku.6）——
 * 都只改这个文件，Sidebar.tsx 只管列表、分区标题与未注册段。
 *
 * 2026-09-09 meta 一行抽到 RowIdentity.tsx（agora-uvd.7 接缝，不改行为）：行身份与展开区搬走
 * 不再抢同一文件。node / stale 的规则仍在 RowIdentity 里，注释随那一处。
 */

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
  /** 本机 node.id（`/api/system` 的 node）：已知后每一行都标节点 chip；undefined = 还不知道，谁都不标。 */
  localNode?: string;
  /** 「看 diff」：开该会话的只读 diff 标签页（MISSION §6.3 看结果；agora-h1k.5）。 */
  onOpenDiff?: (id: string) => void;
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, ordinal, onOpen, onRender, expanded, now, localNode, onOpenDiff }: RowProps) {
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
          <RowIdentity row={row} localNode={localNode} now={now} />
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
      {active && <Changes row={row} onOpenDiff={onOpenDiff} />}
    </li>
  );
});
