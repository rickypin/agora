import { statusLine } from "./attention";
import type { SessionRow } from "./events";
import { clockText } from "./Header";

/**
 * 侧栏行的 meta 一行（agent / origin / 节点 / stale / state）。
 *
 * 2026-09-09 从 SessionRow.tsx 原样抽出（agora-uvd.7 的接缝 commit，不改行为、DOM 与 testid 不变）：
 * 行身份（徽标、本机也标节点）与 agora-4yr.3（展开区搬走）都要动这一块，抽成独立组件后各改各的。
 */

/** stale 行 `.meta` 里那一段的文案与 title；非 stale 行返回 null（不占位）。 */
export function staleSeen(row: SessionRow): { text: string; title: string } | null {
  if (!row.stale) return null;
  const seen = typeof row.last_seen === "string" ? row.last_seen : null;
  // 有 last_seen 才有"上次见到"；没有（理论上不会——连上过才有行可标）就只说离线，不编时间。
  if (seen === null) return { text: "○ 离线", title: "该节点离线，正在重连" };
  return { text: `○ 上次见到 ${clockText(seen)}`, title: `该节点离线，正在重连；上次见到 ${seen}` };
}

export function RowIdentity({
  row,
  localNode,
  now,
}: {
  row: SessionRow;
  localNode?: string;
  now: number;
}) {
  const seen = staleSeen(row);
  return (
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
  );
}
