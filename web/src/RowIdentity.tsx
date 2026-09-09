import type { CSSProperties } from "react";
import { agentBadge } from "./agentBadge";
import { statusLine } from "./attention";
import type { SessionRow } from "./events";
import { clockText } from "./Header";
import { nodeHue } from "./nodeColor";

/** React 的 CSSProperties 不认自定义属性；`--hue` 给 hsl(var(--hue) …) 用。 */
function hueStyle(hue: number): CSSProperties {
  return { ["--hue"]: hue } as CSSProperties;
}

/**
 * 侧栏行的 meta 一行（品牌徽标 / 节点 chip / origin / stale / state）。
 *
 * 2026-09-09 从 SessionRow.tsx 抽出（agora-uvd.7 接缝），随后同一 issue 把行身份换成徽标 +
 * 本机也标的节点 chip（A49）：两台机常态并行，用户反馈「在本机还是 zuan 不明显」，反转
 * agora-7ku.5「本机不标」。仓库 · 分支一行归 agora-uvd.8，这里不读行上其它派生字段。
 *
 * 节点 chip：`localNode` 已知之后每一行都标（本机 class=local 不着色，peer class=peer 按
 * nodeHue 着色）；还没拉到 `/api/system` 的 node 时谁都不标——先满屏 chip 再把本机改成不着色
 * 更难看。stale「上次见到」段不变。
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
  const agentType = String(row.agent_type ?? "");
  const badge = agentBadge(agentType);
  const local = localNode !== undefined && row.node === localNode;
  return (
    <span className="meta">
      <span
        className="badge"
        data-agent={agentType}
        style={badge.hue ? hueStyle(badge.hue) : undefined}
      >
        {badge.glyph} {badge.label}
      </span>
      {localNode !== undefined && (
        <span
          className={local ? "node local" : "node peer"}
          data-testid={`row-node-${row.id}`}
          data-node={row.node}
          style={local ? undefined : hueStyle(nodeHue(row.node))}
        >
          @ {row.node}
        </span>
      )}
      {row.origin === "external" && <span className="origin">external</span>}
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
