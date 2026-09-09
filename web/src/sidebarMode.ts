/**
 * 侧栏两种视图（A47，agora-uvd.2）：「需要我」= 今天的 attention 排序（默认，MISSION §6.3）；
 * 「按项目」= 树（agora-uvd.3 填组件）。本文件是切换骨架与序号规则：显示顺序一条
 * `visibleOrder`，Alt/Option+N / ]/[ / 过滤框 Enter 都走它。模式是浏览器视图状态，
 * 不加服务端字段、不进 URL。
 */
import { partitionByAttention, sortByAttention, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import { rowHaystack } from "./Sidebar";

export type SidebarMode = "attention" | "tree";

export const MODE_STORAGE_KEY = "agora.sidebar-mode";

/**
 * 侧栏显示顺序。attention 分支原样 = 今天的
 * `partitionByAttention(fuzzyFilter(sortByAttention))`；tree 分支本任务先按
 * `created_at` 升序（同秒按 id）再过滤——uvd.3 换成 DFS。过滤两种模式共用
 * 同一个输入框与 `rowHaystack`。
 */
export function visibleOrder(mode: SidebarMode, rows: SessionRow[], filter: string, seen: SeenSet): SessionRow[] {
  if (mode === "tree") {
    return fuzzyFilter(sortByCreatedAt(rows), filter, rowHaystack);
  }
  return partitionByAttention(fuzzyFilter(sortByAttention(rows), filter, rowHaystack), seen);
}

/** 创建序平铺（uvd.3 替换成 DFS）：`created_at` 升序，同秒（字符串相等）按 id。 */
function sortByCreatedAt(rows: SessionRow[]): SessionRow[] {
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

function createdAt(row: SessionRow): string {
  return typeof row.created_at === "string" ? row.created_at : "";
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
