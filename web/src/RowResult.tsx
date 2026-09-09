import { useState } from "react";
import { taskOf } from "./attention";
import { Changes } from "./Changes";
import type { SessionRow } from "./events";

/**
 * 「看结果」的两段：验收标准（A40，agora-h1k.3）+ 改动列表（A41，agora-h1k.5）。
 *
 * 2026-09-10 从 SessionRow.tsx 原样抽出（agora-4yr.3 的接缝 commit，不改行为、DOM 与 testid 一字不变）：
 * 抽走之后 SessionRow 只剩行本身，与 RowIdentity（agora-uvd.7 抽的行身份）互不占同一个文件；
 * 下一步这两段整体搬进主区（A50），搬的时候只动这个文件与 Workspace，行的代码不再被牵动。
 */

/**
 * 验收标准（MISSION §6.3「看结果」；A40；agora-h1k.3）：`task.acceptance` 是 `bd show`
 * 的 acceptance_criteria 全文，读自 beads、不复制进 agora 的库（不变量 12）。位置在就地 respond
 * 之下（docs/spec/ux.md）：回答问题 / 给下一条指令是先做的事，对照验收是看结果时的事。
 * 默认展开——看结果就是为了看"做完算什么"；一行 summary 可折叠。没有任务或没写验收标准不占位。
 * 折叠状态自己管而不用 <details>：内容折起来就真的不在 DOM 里，测试与读屏都不用猜 open 属性。
 */
export function Acceptance({ row }: { row: SessionRow }) {
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

interface Props {
  row: SessionRow;
  /** 「看 diff」：开该会话的只读 diff（MISSION §6.3 看结果；agora-h1k.5）。 */
  onOpenDiff?: (id: string) => void;
}

/** 验收标准 + 改动列表，顺序固定：对照"做完算什么"看"改了什么"。 */
export function RowResult({ row, onOpenDiff }: Props) {
  return (
    <>
      <Acceptance row={row} />
      <Changes row={row} onOpenDiff={onOpenDiff} />
    </>
  );
}
