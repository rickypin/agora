/**
 * 「按项目」视图的树（A48，agora-uvd.3）：节点组 → 仓库组 → worktree 组 → 会话行；没有仓库的行归该节点的
 * 「其它目录」组。纯函数、可测；组件在 SidebarTree.tsx。文件名带 Model 后缀：叫 sidebarTree.ts 会与 SidebarTree.tsx 在 macOS 的大小写不敏感文件系统上撞名，裸导入 ./SidebarTree 解析到 .ts（2026-09-09 实测 tsc TS1149）。
 *
 * 唯一的硬规则（用户 2026-09-08 的第一条反馈：行随状态自己换位置，人跟不上）：**行的位置只随创建 / 删除变，
 * 不随状态变**——本文件不读 `status` / `status_since` 排序（守卫 sidebarTreeModel.test.ts「order never changes
 * when status or status_since changes」）。FINISHED 行不搬家、不折叠，只由组件淡显。
 *
 * 那条规则只有一条例外（agora-5gg.19，决策 agora-5gg.8 选 A）：`end_cause = superseded` 的旧行从顶层
 * 拿掉、挂到同一进程（拿不到进程号时同一目录）的当前行下面。它读的是**结束原因**而不是状态，且只搬
 * 已被换掉的那一行：一个 agent 进程在侧栏占一行，换过的对话是它的历史（见 [`foldSuperseded`]）。
 * 数据模型一个字不动，身份仍是 `(host, agent_session_id)`（ADR-002 D7）。
 *
 * 分组键（也是折叠记忆的键，MISSION §6.5 折叠不改变序号）：
 * - `node:<node>`：本机排第一，其余按 `nodes`（Header 那一排）的顺序，`nodes` 里没有的按名字；
 * - `repo:<node>:<project.repo>`：组内按 `project.name` 字母序；
 * - `wt:<node>:<project.worktree>`：主 worktree 排第一，其余按目录最后一段字母序；
 * - `other:<node>`：`project == null` 的行，排在该节点所有仓库组之后，组内平铺不再分层（取舍：不按目录再分）；
 * - 行按 `created_at` 升序、同值按 id。
 * 层级固定三层 + 其它目录，不做「只有一个仓库就省略仓库层」之类的自适应（规则越少越好记）。
 */
import { isHandleless } from "./attention";
import { rowEndCause, type SessionRow } from "./events";
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
  /**
   * worktree 组：它所属仓库的路径（= 父仓库组的 `title`）。组头的「在此起 agent」要把 Project 一起
   * 预填，而平铺之后拿不到父组（agora-uvd.4）——在建树时就带上，别让画的人回头找爸爸。
   */
  repo?: string;
  node: string;
  children: TreeGroup[];
  /** 顶层行：当前行与没被折起来的行。superseded 旧行不在这里，它们在 [`TreeGroup.history`] 里。 */
  rows: SessionRow[];
  /**
   * 当前行 id → 折进它下面的 superseded 旧行（agora-5gg.19，按 created_at 升序）。只有叶子组
   * （worktree / 其它目录）会有非空值：折叠不跨组（取舍见 [`foldSuperseded`] 的注释）。
   */
  history: Map<string, SessionRow[]>;
}

/**
 * `hidden`：某个祖先组折叠了，或这一行是别人名下的历史而那个折叠没展开——行不画、序号照数；
 * 组头同样不画（只有折叠的那个组头自己留着）。
 * `historyCount`：这一行名下有几个 superseded 旧行（0 / undefined = 没有，组件据此画不画「历史对话」按钮）。
 * `hostId`：这一行是谁的历史（顶层行没有这个键）。
 */
export type FlatEntry =
  | { kind: "group"; group: TreeGroup; hidden: boolean }
  | { kind: "row"; row: SessionRow; ordinal: number; hidden: boolean; hostId?: string; historyCount?: number };

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

/** 行序：`created_at` 升序（ISO 文本可直接比），同值按 id；再同（不可能）保持传入顺序。不看状态、不看结束原因。 */
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

/**
 * 这一行是不是「被同一进程的新对话换掉了」。两个判据：
 * - `end_cause` 枚举而不是 `reason` 那句人话（agora-5gg.6 / MISSION §2.3 规则 10：措辞可以改，枚举不行）。
 *   `end_cause` 为 null 的老行一律不折：宁可多画一行，也不能拿「没有原因」当「被换掉了」。
 * - 还得是无运行时句柄的那两种来源（external / headless，`isHandleless`）：supersede 只发生在它们身上
 *   （`src/session/manager.rs` 的 `supersede_handleless_rows`）。有 `runtime_ref` 的行身份是 pane，同一
 *   进程再发 SessionStart 落回同一行（那是真会话的 resume，ADR-002 D7 尾段），把它折起来就是藏掉一条
 *   人正在用的行——本任务唯一不可接受的错。当前行那一侧不问来源。
 */
export function isSuperseded(row: SessionRow): boolean {
  return isHandleless(row) && rowEndCause(row)?.kind === "superseded";
}

const HISTORY_PREFIX = "hist:";

/** 历史折叠的键：`hist:<当前行 id>`。组 key 以 `node:` / `repo:` / `wt:` / `other:` 开头，撞不上。 */
export function historyKey(hostRowId: string): string {
  return `${HISTORY_PREFIX}${hostRowId}`;
}

/** 这个折叠 key 是历史折叠还是组折叠（两套默认值相反：组默认开、历史默认收，见 [`flattenTree`]）。 */
export function isHistoryKey(key: string): boolean {
  return key.startsWith(HISTORY_PREFIX);
}

/** 进程号只在「探得到」时非空（agora-5gg.5：external 行探不到就 null）；别的节点的键可能是任何形状。 */
function pidOf(row: SessionRow): number | null {
  return typeof row.pid === "number" && Number.isInteger(row.pid) && row.pid > 0 ? row.pid : null;
}

function cwdOf(row: SessionRow): string | null {
  return typeof row.working_directory === "string" && row.working_directory !== "" ? row.working_directory : null;
}

/** 同一节点上的一个 agent 进程：pid 拿不到时退到它的 cwd（Codex Desktop 那一类，`process = unknown`）。 */
function pidKey(row: SessionRow): string | null {
  const pid = pidOf(row);
  return pid === null ? null : `pid:${row.node}:${pid}`;
}

function cwdKey(row: SessionRow): string | null {
  const cwd = cwdOf(row);
  return cwd === null ? null : `cwd:${row.node}:${cwd}`;
}

export interface SupersededFold {
  /** 顶层行（直接当作 `TreeGroup.rows` 用），保持传入顺序：当前行 + 折不掉的 superseded 行。 */
  rows: SessionRow[];
  /** 当前行 id → 它名下的旧行，保持传入顺序（= created_at 升序）。 */
  history: Map<string, SessionRow[]>;
}

/**
 * 把 superseded 旧行折到当前行下面（agora-5gg.19；决策 agora-5gg.8 选 A：身份仍是对话，只改呈现）。
 *
 * 认「当前那一行」：同一个桶（同节点 + 同 pid，pid 拿不到时同 working_directory）里**最后一个不是
 * superseded** 的行。传入必须已按 `created_at` 升序（[`sortRows`]），所以「最后一个」就是最新那一代对话；
 * 一条链 A→B→C（B 也被 C 换掉）时 A、B 都并进 C，一进程一行而不是套两层折叠。
 *
 * 三条边界：
 * - **没有当前行就不折**：进程退了而桶里全是 superseded（新对话没登记出来就被 `external_finished_ttl`
 *   删了、或换到了别的 pid 上），这些旧行按普通 FINISHED 画在顶层——它们是仅存的记录，藏起来就没有了；
 * - **pid 优先于 cwd**：同进程的两行才是同一条工作线，同目录只是同名（决策原文那两份计数）；
 * - **不跨组搬行**：组是 worktree，把一行的历史挂到另一个 worktree 组的行上会让「位置只随创建 / 删除变」
 *   变得没法解释（还会留下空的 worktree 组）。同进程换了目录的旧行按普通行画（取舍）。
 */
export function foldSuperseded(rows: SessionRow[]): SupersededFold {
  const currentByPid = new Map<string, SessionRow>();
  const currentByCwd = new Map<string, SessionRow>();
  for (const row of rows) {
    if (isSuperseded(row)) continue;
    // 后面的覆盖前面的：留下的就是桶里最新的那个当前行。
    const pk = pidKey(row);
    if (pk !== null) currentByPid.set(pk, row);
    const ck = cwdKey(row);
    if (ck !== null) currentByCwd.set(ck, row);
  }
  const top: SessionRow[] = [];
  const history = new Map<string, SessionRow[]>();
  for (const row of rows) {
    let host: SessionRow | undefined;
    if (isSuperseded(row)) {
      const pk = pidKey(row);
      const ck = cwdKey(row);
      // 两张表里只存非 superseded 的行，所以拿到的必是别人；两个键都拿不到（无 pid 又无 cwd）就是没有人。
      host = (pk === null ? undefined : currentByPid.get(pk)) ?? (ck === null ? undefined : currentByCwd.get(ck));
    }
    if (host === undefined) {
      top.push(row);
      continue;
    }
    const list = history.get(host.id);
    if (list === undefined) history.set(host.id, [row]);
    else list.push(row);
  }
  return { rows: top, history };
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
    history: new Map(),
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
          repo,
          node,
          children: [],
          ...foldSuperseded(sortRows(wtRows)),
        })),
      rows: [],
      history: new Map(),
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
      ...foldSuperseded(sortRows(other)),
    });
  }
  return groups;
}

const NONE: ReadonlySet<string> = new Set();

/**
 * DFS 平铺：组头永远出现（折叠只是把它下面的东西藏起来）；行带 DFS 序号，折叠组里的行 `hidden: true` 但
 * 序号照数（与 A46 Finished 区同一条规则：折叠只是不画不是不数，Alt/Option+N 的第 N 条永远是同一条）。
 *
 * 当前行之后紧跟它名下的 superseded 旧行（agora-5gg.19）：它们同样占序号，但**默认收起**——`openHistory`
 * 是显式展开的那些 `historyKey(...)`（组的默认是展开、历史的默认是收起，所以两个集合不能合成一个）。
 * 一条 superseded 行被折进来时总行数不变，所以别的行的序号不因为它漂移。
 */
export function flattenTree(tree: TreeGroup[], collapsed: ReadonlySet<string>, openHistory: ReadonlySet<string> = NONE): FlatEntry[] {
  const out: FlatEntry[] = [];
  let ordinal = 0;
  const walk = (g: TreeGroup, hidden: boolean) => {
    out.push({ kind: "group", group: g, hidden });
    const below = hidden || collapsed.has(g.key);
    for (const c of g.children) walk(c, below);
    for (const row of g.rows) {
      const older = g.history.get(row.id);
      out.push({ kind: "row", row, ordinal: ++ordinal, hidden: below, historyCount: older?.length ?? 0 });
      if (older === undefined) continue;
      const foldHidden = below || !openHistory.has(historyKey(row.id));
      for (const h of older) out.push({ kind: "row", row: h, ordinal: ++ordinal, hidden: foldHidden, hostId: row.id });
    }
  };
  for (const g of tree) walk(g, false);
  return out;
}

/**
 * 树的 DFS 里会话行的顺序——`visibleOrder` 的 tree 分支、Alt/Option+N 与序号都用它。不读状态；
 * 当前行之后紧跟它名下的 superseded 旧行（行的集合不变，只是位置：agora-5gg.19）。
 */
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
