import { memo } from "react";
import { promptRepeatsLabel, taskLabel } from "./attention";
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
 *
 * 2026-09-10 展开区的验收标准 / 改动列表抽到 RowResult.tsx（agora-4yr.3 接缝），随即整段搬进主区的
 * 看结果面板（A50，兑现 agora-03k）。至此 `<li>` 里只剩行按钮本身：行是"选哪一个"，选中之后要看的
 * 东西全在主区，260 px 的窄列不再承担可读性。守卫 Sidebar.test.tsx「the sidebar DOM never contains
 * respond-, acceptance- or changes- testids in either mode」——两种视图各一次，别让任何一段回来。
 */

/** `<li>` 的类名：选中 / stale / 刚换位，三个正交的状态。 */
export function rowClasses(active: boolean, stale?: boolean, moved?: boolean): string | undefined {
  const cls = [active ? "selected" : null, stale ? "stale" : null, moved ? "moved" : null].filter((c): c is string => c !== null);
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

/** 侧栏过滤匹配的字段（docs/spec/ux.md：name / node / agent / preview）：任务标签与两行预览也算。
 * 从 Sidebar.tsx 挪来（agora-uvd.2）：sidebarMode.ts 要它，不能再从 Sidebar 进，否则 uvd.3 一加运行时
 * import 就成环。Sidebar 原样再导出，调用方一行不改。 */
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

interface RowProps {
  row: SessionRow;
  active: boolean;
  /** 1…9 显示成 Alt/Option 跳转的序号；其余不显示（MISSION §6.5）。 */
  ordinal: number;
  onOpen: (id: string) => void;
  /** 测试注入：数渲染次数。 */
  onRender?: (id: string) => void;
  /** unix 秒；"waiting 3m"的基准。 */
  now: number;
  /** 本机 node.id（`/api/system` 的 node）：已知后每一行都标节点 chip；undefined = 还不知道，谁都不标。 */
  localNode?: string;
  /** 只透传给 RowIdentity（agora-uvd.8）：树视图（SidebarTree，agora-uvd.3）传 false——组头已说明仓库与分支。 */
  showProject?: boolean;
  /** 这一帧刚落位、位置真的变了（A51，agora-4yr.4）：`<li class="moved">` 触发 1.2 s 的背景高亮。 */
  moved?: boolean;
}

/** memo：行对象引用没变就不重渲染（store.ts）。 */
export const SidebarRow = memo(function SidebarRow({ row, active, ordinal, onOpen, onRender, now, localNode, showProject, moved }: RowProps) {
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
    // data-ordinal 挂在 <li> 上而不是只靠行里那个 span（agora-5ri）：span 从来只画 1…9，第 10 行往后
    // 渲染的是空字符串，代检按 `.ord` 文本读序号在 2026-09-08 现场那种 58 行的列表里直接读不到。
    // 属性两种视图共用（SidebarRow 是同一个组件），显示规则不变。
    <li className={rowClasses(active, row.stale, moved)} data-ordinal={ordinal}>
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
          <RowIdentity row={row} localNode={localNode} now={now} showProject={showProject} />
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
    </li>
  );
});
