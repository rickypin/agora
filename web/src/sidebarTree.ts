/**
 * 「按项目」视图的树（A48，agora-uvd.3）：节点组 → 仓库组 → worktree 组 → 会话行；没有仓库的行归该节点的
 * 「其它目录」组。纯函数、可测；组件在 SidebarTree.tsx。
 *
 * 唯一的硬规则（用户 2026-09-08 的第一条反馈：行随状态自己换位置，人跟不上）：**行的位置只随创建 / 删除变，
 * 不随状态变**——本文件从头到尾不读 `status` / `status_since`（守卫 sidebarTree.test.ts「order never changes
 * when status or status_since changes」）。FINISHED 行不搬家、不折叠，只由组件淡显。
 *
 * 分组键（也是折叠记忆的键，MISSION §6.5 折叠不改变序号）：
 * - `node:<node>`：本机排第一，其余按 `nodes`（Header 那一排）的顺序，`nodes` 里没有的按名字；
 * - `repo:<node>:<project.repo>`：组内按 `project.name` 字母序；
 * - `wt:<node>:<project.worktree>`：主 worktree 排第一，其余按目录最后一段字母序；
 * - `other:<node>`：`project == null` 的行，排在该节点所有仓库组之后，组内平铺不再分层（取舍：不按目录再分）；
 * - 行按 `created_at` 升序、同值按 id。
 * 层级固定三层 + 其它目录，不做「只有一个仓库就省略仓库层」之类的自适应（规则越少越好记）。
 */
import type { SessionRow } from "./events";
import type { NodeStatus } from "./Header";

export interface TreeGroup {
  key: string;
  kind: "node" | "repo" | "worktree" | "other";
  /** 组头正文：节点名 / 仓库名 / worktree 目录最后一段 / 「其它目录」。 */
  label: string;
  /** 组头 title：仓库路径 / worktree 路径。 */
  title: string;
  depth: 0 | 1 | 2;
  /** worktree 组：分支名；detached 为 null。 */
  branch?: string | null;
  /** worktree 组：是不是主 worktree。 */
  main?: boolean;
  node: string;
  children: TreeGroup[];
  rows: SessionRow[];
}

export type FlatEntry = { kind: "group"; group: TreeGroup } | { kind: "row"; row: SessionRow; ordinal: number; hidden: boolean };

export const OTHER_LABEL = "其它目录";

type Project = NonNullable<SessionRow["project"]>;

function projectOf(row: SessionRow): Project | null {
  const p = row.project;
  if (!p || typeof p !== "object") return null;
  if (typeof p.repo !== "string" || typeof p.worktree !== "string") return null;
  return p;
}

function createdAt(row: SessionRow): string {
  return typeof row.created_at === "string" ? row.created_at : "";
}

/** 行序：`created_at` 升序（ISO 文本可直接比），同值按 id；再同（不可能）保持传入顺序。不看状态。 */
export function sortRows(rows: SessionRow[]): SessionRow[] {
  return rows
    .map((row, i) => ({ row, i }))
    .sort((a, b) => {
      const c = createdAt(a.row).localeCompare(createdAt(b.row));
      if (c !== 0) return c;
      const id = a.row.id.localeCompare(b.row.id);
      if (id !== 0) return id;
      return a.i - b.i;
    })
    .map((x) => x.row);
}

function basename(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

/** 一行所属的组键，从外到里：`[node:…, repo:…|other:…, wt:…]`（其它目录只有两层）。组头的需要关注计数按它算。 */
export function rowGroupKeys(row: SessionRow): string[] {
  const p = projectOf(row);
  if (!p) return [`node:${row.node}`, `other:${row.node}`];
  return [`node:${row.node}`, `repo:${row.node}:${p.repo}`, `wt:${row.node}:${p.worktree}`];
}

/** 节点顺序：本机第一，然后按 `nodes` 的顺序，剩下的按名字。 */
function orderNodes(names: Iterable<string>, nodes: NodeStatus[] | undefined, localNode: string | undefined): string[] {
  const rank = new Map<string, number>();
  if (localNode !== undefined) rank.set(localNode, -1);
  (nodes ?? []).forEach((n, i) => {
    if (!rank.has(n.name)) rank.set(n.name, i);
  });
  return [...names].sort((a, b) => {
    const ra = rank.get(a) ?? Number.MAX_SAFE_INTEGER;
    const rb = rank.get(b) ?? Number.MAX_SAFE_INTEGER;
    if (ra !== rb) return ra - rb;
    return a.localeCompare(b);
  });
}

export function buildTree(rows: SessionRow[], nodes: NodeStatus[] | undefined, localNode: string | undefined): TreeGroup[] {
  const byNode = new Map<string, SessionRow[]>();
  for (const r of rows) {
    const list = byNode.get(r.node) ?? [];
    list.push(r);
    byNode.set(r.node, list);
  }
  return orderNodes(byNode.keys(), nodes, localNode).map((node) => ({
    key: `node:${node}`,
    kind: "node" as const,
    label: node,
    title: node,
    depth: 0 as const,
    node,
    children: buildNode(node, byNode.get(node) ?? []),
    rows: [],
  }));
}

function buildNode(node: string, rows: SessionRow[]): TreeGroup[] {
  const repos = new Map<string, { name: string; worktrees: Map<string, { p: Project; rows: SessionRow[] }> }>();
  const other: SessionRow[] = [];
  for (const r of rows) {
    const p = projectOf(r);
    if (!p) {
      other.push(r);
      continue;
    }
    const repo = repos.get(p.repo) ?? { name: p.name, worktrees: new Map() };
    repos.set(p.repo, repo);
    const wt = repo.worktrees.get(p.worktree) ?? { p, rows: [] };
    wt.rows.push(r);
    repo.worktrees.set(p.worktree, wt);
  }
  const groups: TreeGroup[] = [...repos.entries()]
    .sort(([ra, a], [rb, b]) => a.name.localeCompare(b.name) || ra.localeCompare(rb))
    .map(([repo, { name, worktrees }]) => ({
      key: `repo:${node}:${repo}`,
      kind: "repo" as const,
      label: name,
      title: repo,
      depth: 1 as const,
      node,
      children: [...worktrees.entries()]
        .sort(([wa, a], [wb, b]) => {
          // 主 worktree 永远第一；其余按目录最后一段，同名按完整路径。
          if (a.p.main !== b.p.main) return a.p.main ? -1 : 1;
          return basename(wa).localeCompare(basename(wb)) || wa.localeCompare(wb);
        })
        .map(([worktree, { p, rows: wtRows }]) => ({
          key: `wt:${node}:${worktree}`,
          kind: "worktree" as const,
          label: basename(worktree),
          title: worktree,
          depth: 2 as const,
          branch: typeof p.branch === "string" ? p.branch : null,
          main: p.main === true,
          node,
          children: [],
          rows: sortRows(wtRows),
        })),
      rows: [],
    }));
  if (other.length > 0) {
    groups.push({
      key: `other:${node}`,
      kind: "other",
      label: OTHER_LABEL,
      title: "不在 git 仓库里的会话",
      depth: 1,
      node,
      children: [],
      rows: sortRows(other),
    });
  }
  return groups;
}

/**
 * DFS 平铺：组头永远出现（折叠只是把它下面的东西藏起来）；行带 DFS 序号，折叠组里的行 `hidden: true` 但
 * 序号照数（与 A46 Finished 区同一条规则：折叠只是不画不是不数，Alt/Option+N 的第 N 条永远是同一条）。
 */
export function flattenTree(tree: TreeGroup[], collapsed: ReadonlySet<string>): FlatEntry[] {
  const out: FlatEntry[] = [];
  let ordinal = 0;
  const walk = (g: TreeGroup, hidden: boolean) => {
    out.push({ kind: "group", group: g });
    const below = hidden || collapsed.has(g.key);
    for (const c of g.children) walk(c, below);
    for (const row of g.rows) out.push({ kind: "row", row, ordinal: ++ordinal, hidden: below });
  };
  for (const g of tree) walk(g, false);
  return out;
}

const NONE: ReadonlySet<string> = new Set();

/** 树的 DFS 里会话行的顺序——`visibleOrder` 的 tree 分支、Alt/Option+N 与序号都用它。不读状态。 */
export function treeOrder(rows: SessionRow[], nodes: NodeStatus[] | undefined, localNode: string | undefined): SessionRow[] {
  return flattenTree(buildTree(rows, nodes, localNode), NONE).flatMap((e) => (e.kind === "row" ? [e.row] : []));
}

/**
 * 折叠记忆（localStorage，键是组 key 的集合；agora-uvd.3）。按组 key 而不按节点整体记（取舍）。读写都包
 * try/catch，照 sidebarMode.ts 的 loadMode / storeMode。
 */
export const COLLAPSED_STORAGE_KEY = "agora.sidebar-tree-collapsed";

export function loadCollapsed(storage: Pick<Storage, "getItem"> | null = safeStorage()): Set<string> {
  try {
    const raw = storage?.getItem(COLLAPSED_STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return new Set(Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function storeCollapsed(collapsed: ReadonlySet<string>, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(COLLAPSED_STORAGE_KEY, JSON.stringify([...collapsed]));
  } catch {
    // 存不下就算了：下次打开全部展开。
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
