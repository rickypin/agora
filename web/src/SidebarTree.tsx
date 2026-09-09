import { useEffect, useMemo, useState, type ReactNode } from "react";
import { needsAttention, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { describeNode, type NodeStatus } from "./Header";
import { SidebarRow } from "./SessionRow";
import { buildTree, flattenTree, loadCollapsed, rowGroupKeys, storeCollapsed, type TreeGroup } from "./sidebarTreeModel";

/**
 * 「按项目」视图（A48，agora-uvd.3；docs/spec/ux.md「按项目」线框）：节点组 → 仓库组 → worktree 组（⎇ 分支）
 * → 会话行，无仓库的行归「其它目录」。规则在 sidebarTreeModel.ts（文件名不叫 sidebarTree.ts：macOS 大小写不敏感，与本文件 SidebarTree.tsx 裸导入撞名，2026-09-09 实测 tsc TS1149）；本组件只画：组头是按钮（aria-expanded，折叠记
 * localStorage），折叠时组头右侧显示这组里 needsAttention 的行数（按过滤前的 all 算——过滤只是少画几行）；
 * 行复用 <SidebarRow> 原样，序号是 DFS 序，折叠的组行不画序号照数（MISSION §6.5，与 A46 Finished 区同一规则）。
 * FINISHED 行不搬家：留在原组里淡显（`li.done`）。不画三段标题、没有 Finished 折叠区（取舍）。
 */
interface Props {
  /** 已过滤、已按 treeOrder 排好的显示顺序（Workspace 的 visible）。 */
  rows: SessionRow[];
  /** 过滤前全部行：组头的需要关注计数按它算。 */
  all: SessionRow[];
  nodes?: NodeStatus[];
  localNode?: string;
  active: string | null;
  seen?: SeenSet;
  onOpen: (id: string) => void;
  onRowRender?: (id: string) => void;
  now: number;
  onOpenDiff?: (id: string) => void;
  /** 选中行的展开区；M4b（agora-4yr.1）合入前仍要透传，4yr.3 会删。 */
  renderExpanded?: (row: SessionRow) => ReactNode;
}

/** 组头文字：节点组是同 Header 的点 + 名字；仓库组是 name；worktree 组是 `label ⎇ branch [主]`。 */
function GroupLabel({ group, nodes }: { group: TreeGroup; nodes?: NodeStatus[] }) {
  if (group.kind === "node") {
    const n = nodes?.find((x) => x.name === group.node);
    const { symbol, cls } = n ? describeNode(n) : { symbol: "○", cls: "unknown" };
    return (
      <span className={`node-status ${cls}`}>
        <span className="node-name">{group.label}</span>
        <span className="dot">{symbol}</span>
      </span>
    );
  }
  if (group.kind === "worktree") {
    return (
      <>
        <span className="tree-label">{group.label}</span>
        <span className="tree-branch muted">⎇ {group.branch ?? "detached"}</span>
        {group.main && <span className="tree-main muted">主</span>}
      </>
    );
  }
  return <span className="tree-label">{group.label}</span>;
}

export function SidebarTree({ rows, all, nodes, localNode, active, seen, onOpen, onRowRender, now, onOpenDiff, renderExpanded }: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => loadCollapsed());
  const tree = useMemo(() => buildTree(rows, nodes, localNode), [rows, nodes, localNode]);
  const flat = useMemo(() => flattenTree(tree, collapsed), [tree, collapsed]);
  // 每个组（含祖先）里需要关注的行数，按过滤前的 all 算；只在折叠时显示。
  const attention = useMemo(() => {
    const count = new Map<string, number>();
    for (const r of all) {
      if (!needsAttention(r, seen)) continue;
      for (const k of rowGroupKeys(r)) count.set(k, (count.get(k) ?? 0) + 1);
    }
    return count;
  }, [all, seen]);

  function toggle(key: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      storeCollapsed(next);
      return next;
    });
  }

  // 选中行落进折叠的组时自动展开一次（照 Sidebar.tsx 里 agora-4nk 的写法）：Alt/Option+N、通知点击、命令面板
  // 都能从外面选中折叠组里的行，不展开就是主区有它、侧栏没有。只在 active 变化时把那条路径上的组 key 从
  // collapsed 里去掉，不派生成「active 在里面就一直开」——那样人点组头收不起来。
  const activePath = useMemo(() => {
    const row = active === null ? undefined : rows.find((r) => r.id === active);
    return row ? rowGroupKeys(row).join("\n") : "";
  }, [active, rows]);
  useEffect(() => {
    if (activePath === "") return;
    const keys = activePath.split("\n");
    setCollapsed((prev) => {
      if (!keys.some((k) => prev.has(k))) return prev;
      const next = new Set(prev);
      for (const k of keys) next.delete(k);
      storeCollapsed(next);
      return next;
    });
  }, [active, activePath]);

  return (
    <ul className="tree" data-testid="sidebar-tree">
      {flat.map((e) => {
        if (e.kind === "group") {
          // 祖先折叠了这一层组头也不画（只有折叠的那个组头自己留着）。
          if (e.hidden) return null;
          const g = e.group;
          const open = !collapsed.has(g.key);
          const n = open ? 0 : (attention.get(g.key) ?? 0);
          return (
            <li key={g.key} className={`tree-group depth-${g.depth} ${g.kind}`}>
              <button
                type="button"
                className="tree-head"
                aria-expanded={open}
                data-testid={`tree-group-${g.key}`}
                title={g.title}
                onClick={() => toggle(g.key)}
              >
                <span className="tree-caret muted">{open ? "▾" : "▸"}</span>
                <GroupLabel group={g} nodes={nodes} />
                {n > 0 && (
                  <span className="tree-attention" data-testid={`tree-group-attention-${g.key}`}>
                    {n} 需要关注
                  </span>
                )}
              </button>
            </li>
          );
        }
        if (e.hidden) return null;
        const r = e.row;
        const depth = rowGroupKeys(r).length;
        // <li class="done"> 由外面这一层包：SidebarRow 自己的 <li> 归 uvd.7，本任务不动它（不许改 SessionRow.tsx）。
        return (
          <li key={r.id} className={`tree-row depth-${depth}${r.status === "finished" ? " done" : ""}`}>
            <ul>
              <SidebarRow
                row={r}
                active={r.id === active}
                ordinal={e.ordinal}
                onOpen={onOpen}
                onRender={onRowRender}
                expanded={r.id === active ? renderExpanded?.(r) : undefined}
                now={now}
                localNode={localNode}
                onOpenDiff={onOpenDiff}
              />
            </ul>
          </li>
        );
      })}
    </ul>
  );
}
