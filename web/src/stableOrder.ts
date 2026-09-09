/**
 * 「需要我」视图的重排稳定（A51，agora-4yr.4）。
 *
 * 侧栏按 attention 分数排序（`attention.ts`），状态一变行就换位——用户 2026-09-08：「agent 根据状态
 * 自己变动位置，人类用户跟不上」。规则一句话：**我在看 / 在操作的时候别动；我走开了再落位；动了要
 * 让我看见**。这里只放纯函数，触发条件（指针进出侧栏、键盘交互与 3 s 定时器）在 `Workspace.tsx`。
 *
 * 冻结的对象是**顺序 + 分段归属**，不只是顺序（2026-09-10 交叉验证；反例见 `stableSections`）：
 * `Sidebar.tsx` 的三段表头是按位置推的，只冻顺序会让表头插到列表中间、计数错、FINISHED 行从 DOM
 * 消失——正好违反本任务自己写的「不许在冻结期间隐藏行」。
 *
 * 不冻的东西同样重要：行对象一律取自 `next`，所以状态符号、预览文案、`waiting 3m` 全是新的，只有
 * 次序沿用旧的；新行照插，不等解冻。
 */
import type { Section } from "./attention";

/** 冻结窗口：指针离开侧栏、或最后一次键盘 / 点击交互之后这么久才落位。写死 3 s，不做配置。 */
export const FREEZE_MS = 3000;

/** 三段的先后，与 `attention.ts` 的 `partitionByAttention` 同一条。 */
export const SECTION_ORDER: readonly Section[] = ["attention", "running", "finished"];

export interface StableResult<T> {
  order: T[];
  /** 相对位置真的变了的行：解冻落位那一帧短暂高亮（`li.moved`）。冻结期间恒为空——没动就没什么可看。 */
  moved: Set<string>;
}

/**
 * 显示顺序：`frozen` 时沿用 `prev` 的次序，否则原样交出 `next` 并报告谁换了位。
 *
 * - `frozen === false` 或 `prev === null`：`order = next`；`moved` = 在 `prev` 里存在、且相对位置变了的 id。
 * - `frozen === true`：`order` = `prev` 的顺序过滤掉 `next` 里没有的 id，再把新 id 按它们在 `next` 里的
 *   位置插进去（插到 `next` 中它前一个「旧顺序里也有」的元素之后；没有前一个就插最前）；`moved` 为空。
 *
 * 元素对象永远取自 `next`：冻结的是次序，不是内容。
 */
export function stableOrder<T extends { id: string }>(prev: readonly string[] | null, next: readonly T[], frozen: boolean): StableResult<T> {
  if (!frozen || prev === null) {
    return { order: [...next], moved: movedIds(prev, next) };
  }
  const byId = new Map(next.map((r) => [r.id, r]));
  const kept = prev.filter((id) => byId.has(id));
  const keptSet = new Set(kept);
  // 新行的锚点：`next` 里它前面最近的一个「旧顺序里也有」的行。锚点之后可能排着好几个新行，
  // 它们之间保持 `next` 的先后。
  const head: T[] = [];
  const after = new Map<string, T[]>();
  let anchor: string | null = null;
  for (const row of next) {
    if (keptSet.has(row.id)) {
      anchor = row.id;
      continue;
    }
    if (anchor === null) head.push(row);
    else {
      const bucket = after.get(anchor);
      if (bucket) bucket.push(row);
      else after.set(anchor, [row]);
    }
  }
  const order: T[] = [...head];
  for (const id of kept) {
    order.push(byId.get(id)!);
    const bucket = after.get(id);
    if (bucket) order.push(...bucket);
  }
  return { order, moved: new Set() };
}

/**
 * 换了位的行：按 `next` 的顺序取 `prev` 中也存在的 id 序列，与 `prev` 过滤后的序列逐位比较，不同即算动过。
 * 只看**相对**位置，所以纯粹的增删（新行插进来、老行消失）不会把满屏行都点亮；过滤只删不换序，
 * 敲过滤框同样不点亮。
 */
function movedIds<T extends { id: string }>(prev: readonly string[] | null, next: readonly T[]): Set<string> {
  const moved = new Set<string>();
  if (prev === null) return moved;
  const inPrev = new Set(prev);
  const byNext = next.filter((r) => inPrev.has(r.id)).map((r) => r.id);
  const inNext = new Set(byNext);
  const byPrev = prev.filter((id) => inNext.has(id));
  for (let i = 0; i < byNext.length; i += 1) {
    if (byNext[i] !== byPrev[i]) moved.add(byNext[i]);
  }
  return moved;
}

export interface SectionedResult<T> {
  order: T[];
  sections: Section[];
}

/**
 * 每行的分段归属：`frozen` 时老行沿用冻结那一刻的段，新行按实时 `sectionOf` 算。
 *
 * 为什么非冻不可（2026-09-10 交叉验证，代码里的反例）：`Sidebar.tsx` 的 `firstRunning` /
 * `firstFinished` / `finishedCount` 全靠 `rows.map(sectionOf)` 三段**连续**，且 finished 段收起时
 * 那些行根本不渲染。只冻顺序的话，行留在旧位置而段用新状态算，一条日常路径就能把列表打烂：逐个
 * 处理时点下一行会把上一行写进 `seen`，`finishedCollapsed` 立刻为真，而 `onOpen` 自己就调
 * `touchSidebar()` 冻着顺序——那一行当场从 DOM 消失，FINISHED 表头插到列表中间，计数把它之后所有
 * 行都算进去。守卫见 `Workspace.test.tsx`「a row that becomes finished during a freeze…」。
 *
 * 冻完还要**稳定重排一次**：老行的段本来就是非降序的（冻结那一刻由 `partitionByAttention` 保证），
 * 三段各自 filter 对它们是恒等变换、一行都不会动；只有冻结期间新插进来的行会被挪进自己那一段，
 * 免得它把三段切断（这是 Sidebar 唯一的前提）。状态符号仍走 `row.status` 实时更新，与分段无关。
 */
export function stableSections<T extends { id: string }>(
  prev: ReadonlyMap<string, Section> | null,
  order: readonly T[],
  sectionOf: (row: T) => Section,
  frozen: boolean,
): SectionedResult<T> {
  if (!frozen || prev === null) {
    return { order: [...order], sections: order.map(sectionOf) };
  }
  const at = (row: T): Section => prev.get(row.id) ?? sectionOf(row);
  const out: T[] = [];
  const sections: Section[] = [];
  for (const section of SECTION_ORDER) {
    for (const row of order) {
      if (at(row) === section) {
        out.push(row);
        sections.push(section);
      }
    }
  }
  return { order: out, sections };
}
