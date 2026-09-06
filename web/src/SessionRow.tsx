import { memo, type ReactNode } from "react";
import { promptRepeatsLabel, statusLine, taskLabel } from "./attention";
import type { SessionRow } from "./events";

/**
 * 侧栏的一行会话（docs/spec/ux.md「Attention Dashboard 线框」）。
 *
 * 2026-09-06 从 Sidebar.tsx 原样抽出（agora-h1k.3 的接缝 commit，不改行为、DOM 不变）：
 * 后续往行上加东西——验收标准折叠块（h1k.3）、node 标签（7ku.5）、stale「上次见到」（7ku.6）——
 * 都只改这个文件，Sidebar.tsx 只管列表、分区标题与未注册段。
 */

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
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, ordinal, onOpen, onRender, expanded, now }: RowProps) {
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
    <li className={active ? "selected" : undefined}>
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
            <span className="node">@ {row.node}</span>
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
    </li>
  );
});
