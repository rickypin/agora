/**
 * 侧栏两种视图（A47，agora-uvd.2）：「需要我」= 今天的 attention 排序（默认，MISSION §6.3）；
 * 「按项目」= 树（agora-uvd.3 填组件）。本文件是切换骨架与序号规则：显示顺序一条
 * `visibleOrder`，Alt/Option+N / ]/[ / 过滤框 Enter 都走它。模式是浏览器视图状态，
 * 不加服务端字段、不进 URL。
 */
import { partitionByAttention, sortByAttention, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import type { NodeStatus } from "./Header";
import { rowHaystack } from "./SessionRow";
import { treeOrder } from "./sidebarTreeModel";

export type SidebarMode = "attention" | "tree";

export const MODE_STORAGE_KEY = "agora.sidebar-mode";

/**
 * 侧栏显示顺序。attention 分支原样 = 今天的
 * `partitionByAttention(fuzzyFilter(sortByAttention))`；tree 分支是树的 DFS
 * 顺序（`treeOrder`，agora-uvd.3：节点 → 仓库 → worktree → 行按创建序，不读状态）
 * 再过滤。过滤两种模式共用同一个输入框与 `rowHaystack`，只删不换序。
 */
export function visibleOrder(mode: SidebarMode, rows: SessionRow[], filter: string, seen: SeenSet, nodes?: NodeStatus[], localNode?: string): SessionRow[] {
  if (mode === "tree") {
    return fuzzyFilter(treeOrder(rows, nodes, localNode), filter, rowHaystack);
  }
  return partitionByAttention(fuzzyFilter(sortByAttention(rows), filter, rowHaystack), seen);
}

/**
 * 视图模式的持久化（localStorage；agora-uvd.2）：换浏览器 / 清了站点数据就回到默认
 * attention——它只是视图状态。读写都包 try/catch：隐私窗口、被禁的存储访问
 * `localStorage` 本身会抛。照 `attention.ts` 的 loadSeen / storeSeen 写。
 */
export function loadMode(storage: Pick<Storage, "getItem"> | null = safeStorage()): SidebarMode {
  try {
    const raw = storage?.getItem(MODE_STORAGE_KEY);
    return raw === "tree" || raw === "attention" ? raw : "attention";
  } catch {
    return "attention";
  }
}

export function storeMode(mode: SidebarMode, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(MODE_STORAGE_KEY, mode);
  } catch {
    // 存不下就算了：下次打开回到默认 attention。
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
