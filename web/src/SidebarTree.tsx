import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { SessionApi } from "./api";
import { needsAttention, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { describeNode, type NodeStatus } from "./Header";
import type { NewAgentInitial } from "./NewAgentDialog";
import { SidebarRow } from "./SessionRow";
import { buildTree, flattenTree, loadCollapsed, rowGroupKeys, storeCollapsed, type TreeGroup } from "./sidebarTreeModel";

/**
 * 「按项目」视图（A48，agora-uvd.3；docs/spec/ux.md「按项目」线框）：节点组 → 仓库组 → worktree 组（⎇ 分支）
 * → 会话行，无仓库的行归「其它目录」。规则在 sidebarTreeModel.ts（文件名不叫 sidebarTree.ts：macOS 大小写不敏感，与本文件 SidebarTree.tsx 裸导入撞名，2026-09-09 实测 tsc TS1149）；本组件只画：组头是按钮（aria-expanded，折叠记
 * localStorage），折叠时组头右侧显示这组里 needsAttention 的行数（按过滤前的 all 算——过滤只是少画几行）；
 * 行复用 <SidebarRow> 原样，序号是 DFS 序，折叠的组行不画序号照数（MISSION §6.5，与 A46 Finished 区同一规则）。
 * FINISHED 行不搬家：留在原组里淡显（`li.done`）。不画三段标题、没有 Finished 折叠区（取舍）。
 *
 * 组头的就地动作（A48，agora-uvd.4；MISSION §6.4「常用项目最多 2–3 次操作」）：worktree 组头右侧「+」
 * 带着 Node / Project / Worktree 打开 New Agent 对话框（只剩选 Agent 一步），「shell」连对话框都不开、
 * 直接在这个 worktree 里 POST 一条 shell 会话；节点组头「+」只预填 Node；「其它目录」没有按钮。
 * stale 节点上一律禁用（一跳转发到不了）。
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
  /** 组头「+」：带着这个组的 Node / Project / Worktree 打开 New Agent 对话框（A48，agora-uvd.4）。 */
  onNewAgent?: (initial?: NewAgentInitial) => void;
  /** 组头「shell」直接起会话用的写端点；不给就没有这两个按钮。 */
  api?: Pick<SessionApi, "create">;
  /** 起成功了：交给 Workspace 的 pendingOpen，会话进列表后自动选中（同 New Agent 对话框）。 */
  onCreated?: (id: string) => void;
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

export function SidebarTree({
  rows,
  all,
  nodes,
  localNode,
  active,
  seen,
  onOpen,
  onRowRender,
  now,
  onOpenDiff,
  renderExpanded,
  onNewAgent,
  api,
  onCreated,
}: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => loadCollapsed());
  // 「shell」失败：组头下一行灰字，5 s 后自己消失（一次只留最后一条——同时点两个组头不是真实用法）。
  const [shellError, setShellError] = useState<{ key: string; message: string } | null>(null);
  // 正在 POST 的那些组（存组 key，不是一个布尔/单值）：只禁用发起的那个组头。同时对两个 worktree
  // 起 shell 是正当用法，不该互相挡——判断写成全局的 `!== null` 时，在途期间所有组头一起灰、点别的组
  // 还会被 openShell 开头的早退静默吞掉（连错误提示都没有）。本机看不见是因为窗口太短：隔离 daemon 上
  // 连打五次 POST /api/sessions 是 22 / 22 / 23 / 28 / 42 ms（2026-09-09 实测，agora-x1k）；peer 一跳
  // 转发、手机远程时就看得见。守卫：SidebarTree.test.tsx 里 in-flight 的两条（自己禁用 / 别人不受影响）。
  const [shellBusy, setShellBusy] = useState<Set<string>>(() => new Set());
  useEffect(() => {
    if (shellError === null) return;
    const t = setTimeout(() => setShellError(null), 5000);
    return () => clearTimeout(t);
  }, [shellError]);
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

  /** 组头动作里 create body / 预填的 `node`：本机不发这个键（与 New Agent 对话框同一口径）。 */
  function nodeOf(group: TreeGroup): string | null {
    return group.node === localNode ? null : group.node;
  }

  /** stale 的节点上不给起会话：一跳转发到不了，按了只会得到 502，不如按钮就是灰的（取舍）。 */
  function offline(group: TreeGroup): boolean {
    const n = nodes?.find((x) => x.name === group.node);
    // 本机「探测中」（health 还没回来）不算离线：那是页面自己刚打开，不是节点掉了。
    return n !== undefined && n.local !== true && !n.online;
  }

  /**
   * 「shell」：不开对话框、不问名字（取舍）——名字就是 worktree 目录名，要改名走 Settings。
   * 直接 POST 一条 shell 会话，成功交给 onCreated（Workspace 的 pendingOpen，行进列表后自动选中）。
   */
  async function openShell(group: TreeGroup) {
    if (!api || shellBusy.has(group.key)) return;
    const node = nodeOf(group);
    setShellBusy((prev) => new Set(prev).add(group.key));
    setShellError(null);
    const r = await api.create({
      ...(node ? { node } : {}),
      display_name: group.label,
      agent_type: "shell",
      // `worktree` 字段存的是分支名而不是路径（docs/spec/api.md「看结果」段）；主 worktree 留空，
      // 与 New Agent 对话框同一条规则。cwd 才是这个 worktree 的路径。
      working_directory: group.title,
      worktree: group.main ? null : (group.branch ?? group.title),
    });
    // 只删自己那个 key：别的组头可能同时也在途。
    setShellBusy((prev) => {
      const next = new Set(prev);
      next.delete(group.key);
      return next;
    });
    if (r.ok) {
      onCreated?.(r.value.id);
      return;
    }
    setShellError({ key: group.key, message: r.needsConfirmation ? "需要确认" : `${r.error.error}: ${r.error.message}` });
  }

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
          // 组头右侧的就地动作（A48，agora-uvd.4）：worktree 组两个（起 agent / 开 shell）、节点组一个；
          // 「其它目录」没有——它不是一个可以在里面干活的目录，只是"没有仓库"的兜底。
          const stale = offline(g);
          const staleTitle = stale ? "节点离线" : undefined;
          return (
            <li key={g.key} className={`tree-group depth-${g.depth} ${g.kind}`}>
              <div className="tree-head-row">
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
                {g.kind === "worktree" && onNewAgent && (
                  <button
                    type="button"
                    className="tree-action"
                    data-testid={`tree-new-agent-${g.key}`}
                    title={staleTitle ?? "在此起 agent"}
                    disabled={stale}
                    // 动作不是折叠：按钮在组头按钮外面，仍显式挡一次冒泡（将来整行可点也不会连带折叠）。
                    onClick={(e) => {
                      e.stopPropagation();
                      onNewAgent({ node: nodeOf(g), project: g.repo, worktree: g.title });
                    }}
                  >
                    +
                  </button>
                )}
                {g.kind === "worktree" && api && (
                  <button
                    type="button"
                    className="tree-action"
                    data-testid={`tree-new-shell-${g.key}`}
                    title={staleTitle ?? "在此开 shell"}
                    disabled={stale || shellBusy.has(g.key)}
                    onClick={(e) => {
                      e.stopPropagation();
                      void openShell(g);
                    }}
                  >
                    shell
                  </button>
                )}
                {g.kind === "node" && onNewAgent && (
                  <button
                    type="button"
                    className="tree-action"
                    data-testid={`tree-new-agent-node-${g.node}`}
                    title={staleTitle ?? "在此节点起 agent"}
                    disabled={stale}
                    onClick={(e) => {
                      e.stopPropagation();
                      onNewAgent({ node: nodeOf(g) });
                    }}
                  >
                    +
                  </button>
                )}
              </div>
              {shellError?.key === g.key && (
                <p className="muted tree-shell-error" role="status" data-testid={`tree-shell-error-${g.key}`}>
                  {shellError.message}
                </p>
              )}
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
                // 组头已经说明仓库与分支，行上再画一遍「name ⎇ branch」是噪音（agora-uvd.8 / agora-s7o）。
                showProject={false}
              />
            </ul>
          </li>
        );
      })}
    </ul>
  );
}
