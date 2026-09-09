import { useContext, useState } from "react";
import { taskOf } from "./attention";
import { Changes, ChangesApiContext, showsChanges } from "./Changes";
import type { SessionRow } from "./events";

/**
 * 「看结果」面板（MISSION §6.3；A50，agora-4yr.3，兑现 agora-03k）：主区回答面板之下、终端之上的
 * 那一段——① 验收标准（A40，agora-h1k.3）② 改动列表（A41，agora-h1k.5）。顺序固定：先回答 /
 * 下指令，再对照"做完算什么"看"改了什么"。
 *
 * 2026-09-10 之前这两段长在侧栏选中行的 `<li>` 里（先由本文件的接缝 commit 从 SessionRow.tsx 抽出）。
 * 260 px 的窄列里一段验收标准要折七八行、路径全被省略号吃掉（agora-03k）；进主区只改布局，
 * 数据来源与显示规则一个字没动。
 *
 * 与回答面板分成两个 `<section>` 而不是合成一个：respond 随状态出现 / 消失（WAITING / TURN_DONE），
 * result 看的是另一个状态集合（TURN_DONE / FINISHED / FAILED / RUNNING），生命周期不同——
 * 合成一个就得让两套判据互相迁就，而且 diff 视图下要藏掉的只是其中一半。
 *
 * **diff 视图下这个面板照画**（2026-09-10 定，沿用 2026-09-06 的实测决定）：验收标准 / 改动列表
 * 与 diff 并排对照本来就是 MISSION §6.3 要的；而且 Changes 的拉取挂在 mount 的 useEffect、卸载即
 * 丢 state，藏掉它会让「关掉 diff」必然重发一次 GET /changes。守卫见 Workspace.test.tsx
 * 「the diff view hides the respond panel, keeps the result panel …」。
 */

/**
 * 这一行有没有验收标准可看：读自 beads 的 `task.acceptance`（`docs/spec/api.md`，不复制进 agora
 * 的库，不变量 12）。面板要不要占位与块自己画不画必须是同一个判据，否则会画出一个空面板。
 */
export function acceptanceOf(row: SessionRow): { id: string; text: string } | null {
  const task = taskOf(row);
  const text = typeof task?.acceptance === "string" ? task.acceptance.trim() : "";
  if (!task || !text) return null;
  return { id: task.id, text };
}

/**
 * 验收标准折叠块：`task.acceptance` 全文、多行原样。默认展开——看结果就是为了看"做完算什么"；
 * 一行 summary 可折叠。没有任务或没写验收标准不占位。
 * 折叠状态自己管而不用 <details>：内容折起来就真的不在 DOM 里，测试与读屏都不用猜 open 属性。
 */
export function Acceptance({ row }: { row: SessionRow }) {
  const [open, setOpen] = useState(true);
  const acc = acceptanceOf(row);
  if (!acc) return null;
  return (
    <div className="acceptance" data-testid={`acceptance-${row.id}`}>
      <button
        type="button"
        className="acceptance-toggle muted"
        aria-expanded={open}
        data-testid={`acceptance-toggle-${row.id}`}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? "▾" : "▸"} 验收标准 · {acc.id}
      </button>
      {open && (
        <pre className="acceptance-body" data-testid={`acceptance-body-${row.id}`}>
          {acc.text}
        </pre>
      )}
    </div>
  );
}

interface Props {
  row: SessionRow;
  /** 「看 diff」：把主区切成该会话的只读 diff（MISSION §6.3 看结果；agora-h1k.5）。 */
  onOpenDiff?: (id: string) => void;
}

/** 两段都没有内容时整个面板不占位——空面板会在终端上方留一条无缘无故的边线。 */
export function RowResult({ row, onOpenDiff }: Props) {
  const api = useContext(ChangesApiContext);
  if (!acceptanceOf(row) && !showsChanges(row, api)) return null;
  return (
    <section className="result-panel" data-testid={`result-panel-${row.id}`}>
      <Acceptance row={row} />
      <Changes row={row} onOpenDiff={onOpenDiff} />
    </section>
  );
}
