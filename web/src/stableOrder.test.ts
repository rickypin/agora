import { describe, expect, it } from "vitest";
import type { Section } from "./attention";
import { stableOrder, stableSections } from "./stableOrder";

interface Row {
  id: string;
  status: string;
}

const rows = (...spec: string[]): Row[] => spec.map((s) => ({ id: s, status: "running" }));
const ids = <T extends { id: string }>(list: readonly T[]): string[] => list.map((r) => r.id);

describe("stableOrder（A51，agora-4yr.4）", () => {
  it("unfrozen returns next and reports rows whose relative position changed", () => {
    const next = rows("c", "a", "b");
    const r = stableOrder(["a", "b", "c"], next, false);
    // 不冻结就是原样交出 next（连元素引用都不换）。
    expect(ids(r.order)).toEqual(["c", "a", "b"]);
    expect(r.order[0]).toBe(next[0]);
    // c 从末尾跑到最前，a / b 跟着往后挪了一格：三行的下标都变了，三行都算动过（描述里的定义
    // 就是「与 prev 过滤后的序列逐位比较，位置不同即 moved」，不做最长公共子序列那种最小集）。
    expect([...r.moved].sort()).toEqual(["a", "b", "c"]);
    // 第一次渲染（prev === null）谁都不算动过——不然一进页面满屏高亮。
    expect(stableOrder(null, next, false).moved.size).toBe(0);
    // 纯粹的增删不算换位：只删（过滤）与只插（新会话）都不点亮任何一行。
    expect(stableOrder(["a", "b", "c"], rows("a", "c"), false).moved.size).toBe(0);
    expect(stableOrder(["a", "b"], rows("a", "x", "b"), false).moved.size).toBe(0);
  });

  it("frozen keeps prev order, drops removed ids, inserts new ids after their predecessor", () => {
    // prev = a b c；next 里 b 没了、来了 x（在 a 之后）、y（在 c 之后）、z（排在所有老行之前）。
    const r = stableOrder(["a", "b", "c"], rows("z", "a", "x", "c", "y"), true);
    expect(ids(r.order)).toEqual(["z", "a", "x", "c", "y"]);
    // 老行 a、c 的相对先后原样；b 从 next 里消失了就真的没了（不许留幽灵行）。
    expect(ids(r.order).filter((id) => id === "a" || id === "c")).toEqual(["a", "c"]);
    // 一次进来好几个新行时它们之间保持 next 的先后。
    expect(ids(stableOrder(["a"], rows("a", "m", "n"), true).order)).toEqual(["a", "m", "n"]);
    // prev 为 null（还没渲染过）时冻结也无从冻起，直接给 next。
    expect(ids(stableOrder(null, rows("b", "a"), true).order)).toEqual(["b", "a"]);
  });

  it("frozen never reports moved", () => {
    // 冻结期间行本来就没动，报 moved 就会在原地闪一片高亮。
    expect(stableOrder(["a", "b", "c"], rows("c", "b", "a"), true).moved.size).toBe(0);
    expect(stableOrder(["a", "b"], rows("x", "b", "a"), true).moved.size).toBe(0);
  });

  it("objects always come from next（状态是新的）", () => {
    // 冻结的是次序不是内容：符号、预览、waiting 3m 都得跟着事件走，否则用户看不到状态变了。
    const next: Row[] = [
      { id: "a", status: "waiting" },
      { id: "b", status: "finished" },
    ];
    const frozen = stableOrder(["b", "a"], next, true);
    expect(ids(frozen.order)).toEqual(["b", "a"]);
    expect(frozen.order.map((r) => r.status)).toEqual(["finished", "waiting"]);
    expect(frozen.order[0]).toBe(next[1]);
    expect(frozen.order[1]).toBe(next[0]);
  });

  it("frozen freezes the section of every row and keeps the three sections contiguous (A51)", () => {
    // 冻结那一刻：wait / own 在 NEEDS ATTENTION，run 在 RUNNING，ext 在折叠区。
    const prev = new Map<string, Section>([
      ["wait", "attention"],
      ["own", "attention"],
      ["run", "running"],
      ["ext", "finished"],
    ]);
    // 冻结期间 own 被写进 seen（实时算法说它该进折叠区）——它必须留在原位、仍算 attention 段，
    // 否则 Sidebar 会把它从 DOM 里抹掉（那正是「不许在冻结期间隐藏行」）。
    const live = (row: Row): Section => (row.id === "own" || row.id === "ext" ? "finished" : row.id === "run" ? "running" : "attention");
    const held = stableSections(prev, rows("wait", "own", "run", "ext"), live, true);
    expect(ids(held.order)).toEqual(["wait", "own", "run", "ext"]);
    expect(held.sections).toEqual(["attention", "attention", "running", "finished"]);
    // 冻结期间新来的行按实时段落位，并被挪进自己那一段——三段必须连续，Sidebar 的表头与计数全靠这个。
    const withNew = stableSections(prev, rows("wait", "own", "fresh", "run", "ext"), (row) => (row.id === "fresh" ? "running" : live(row)), true);
    expect(ids(withNew.order)).toEqual(["wait", "own", "fresh", "run", "ext"]);
    expect(withNew.sections).toEqual(["attention", "attention", "running", "running", "finished"]);
    expect(contiguous(withNew.sections)).toBe(true);
    // 不冻结就是实时分段，一行不动。
    const liveResult = stableSections(prev, rows("wait", "own", "run", "ext"), live, false);
    expect(liveResult.sections).toEqual(["attention", "finished", "running", "finished"]);
  });
});

/** 三段各自成块、且顺序是 attention → running → finished。 */
function contiguous(sections: Section[]): boolean {
  const rank: Record<Section, number> = { attention: 0, running: 1, finished: 2 };
  return sections.every((s, i) => i === 0 || rank[sections[i - 1]] <= rank[s]);
}
