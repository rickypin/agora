import { describe, expect, it } from "vitest";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import type { NodeStatus } from "./Header";
import { rowHaystack } from "./SessionRow";
import { buildTree, COLLAPSED_STORAGE_KEY, flattenTree, loadCollapsed, rowGroupKeys, storeCollapsed, treeOrder, type TreeGroup } from "./sidebarTree";

function row(id: string, node: string, created: string, project: SessionRow["project"] = null, extra: Record<string, unknown> = {}): SessionRow {
  return { id, node, status: "running", alive: true, created_at: created, project, display_name: id, ...extra };
}

const AGORA = "/Users/ricky/code/agora";
const WT3 = "/Users/ricky/code/agora-wt/agora-uvd.3";
const main = { repo: AGORA, name: "agora", worktree: AGORA, branch: "main", main: true };
const wt3 = { repo: AGORA, name: "agora", worktree: WT3, branch: "agora-uvd.3", main: false };
const sglog = { repo: "/Users/ricky/code/sglog", name: "sglog", worktree: "/Users/ricky/code/sglog", branch: null, main: true };

const node = (name: string, over: Partial<NodeStatus> = {}): NodeStatus => ({ name, online: true, last_seen: null, retrying: false, last_error: null, ...over });

/** 树的骨架：`key` 逐层缩进 + 行 id，方便整棵比。 */
function outline(tree: TreeGroup[]): string[] {
  const out: string[] = [];
  const walk = (g: TreeGroup) => {
    out.push(`${"  ".repeat(g.depth)}${g.key}`);
    g.children.forEach(walk);
    g.rows.forEach((r) => out.push(`${"  ".repeat(g.depth + 1)}${r.id}`));
  };
  tree.forEach(walk);
  return out;
}

const ROWS = [
  row("mac:b", "mac", "2026-09-08T12:00:02Z", main),
  row("mac:wt", "mac", "2026-09-08T12:00:01Z", wt3),
  row("mac:a", "mac", "2026-09-08T12:00:01Z", main),
  row("mac:sh", "mac", "2026-09-08T11:00:00Z", null, { working_directory: "/tmp" }),
  row("mac:sg", "mac", "2026-09-08T10:00:00Z", sglog, { display_name: "parser" }),
  row("zuan:z", "zuan", "2026-09-08T09:00:00Z", main),
];

describe("sidebarTree", () => {
  it("groups by node, repo, worktree and orders rows by created_at (A48)", () => {
    const tree = buildTree(ROWS, [node("mac"), node("zuan")], "mac");
    expect(outline(tree)).toEqual([
      "node:mac",
      `  repo:mac:${AGORA}`,
      `    wt:mac:${AGORA}`,
      "      mac:a", // 同秒按 id：a 在 b 前
      "      mac:b",
      `    wt:mac:${WT3}`,
      "      mac:wt",
      "  repo:mac:/Users/ricky/code/sglog",
      "    wt:mac:/Users/ricky/code/sglog",
      "      mac:sg",
      "  other:mac",
      "    mac:sh",
      "node:zuan",
      `  repo:zuan:${AGORA}`,
      `    wt:zuan:${AGORA}`,
      "      zuan:z",
    ]);
    const agora = tree[0]!.children[0]!;
    expect(agora.label).toBe("agora");
    expect(agora.title).toBe(AGORA);
    // 主 worktree 第一并带标记；其它 worktree 的 label 是目录最后一段，branch 原样。
    expect(agora.children.map((w) => [w.label, w.branch, w.main])).toEqual([
      ["agora", "main", true],
      ["agora-uvd.3", "agora-uvd.3", false],
    ]);
    // detached 的分支是 null，由组件显示成 ⎇ detached。
    expect(tree[0]!.children[1]!.children[0]!.branch).toBeNull();
    // 行的组键（组头计数按它算）。
    expect(rowGroupKeys(ROWS[1]!)).toEqual(["node:mac", `repo:mac:${AGORA}`, `wt:mac:${WT3}`]);
    expect(rowGroupKeys(ROWS[3]!)).toEqual(["node:mac", "other:mac"]);
  });

  it("rows without a project go to an 其它目录 group after all repos (A48)", () => {
    const rows = [row("mac:t", "mac", "2026-09-08T00:00:00Z", null), row("mac:u", "mac", "2026-09-08T00:00:01Z", null), row("mac:z", "mac", "2026-09-08T00:00:02Z", sglog)];
    const tree = buildTree(rows, undefined, "mac");
    expect(tree[0]!.children.map((g) => [g.kind, g.key])).toEqual([
      ["repo", "repo:mac:/Users/ricky/code/sglog"],
      ["other", "other:mac"],
    ]);
    const other = tree[0]!.children[1]!;
    expect(other.label).toBe("其它目录");
    expect(other.depth).toBe(1);
    expect(other.children).toEqual([]); // 组内平铺，不再按目录分
    expect(other.rows.map((r) => r.id)).toEqual(["mac:t", "mac:u"]);
    // 没有无仓库的行就没有这一组。
    expect(buildTree([rows[2]!], undefined, "mac")[0]!.children.map((g) => g.kind)).toEqual(["repo"]);
  });

  it("the local node comes first, peers follow the nodes order", () => {
    const rows = [row("a:1", "a", "1", main), row("zuan:1", "zuan", "1", main), row("mac:1", "mac", "1", main), row("pi:1", "pi", "1", main)];
    // nodes 顺序 zuan → pi，本机 mac 仍第一；a 不在 nodes 里，按名字排最后。
    expect(buildTree(rows, [node("mac", { local: true }), node("zuan"), node("pi")], "mac").map((g) => g.label)).toEqual(["mac", "zuan", "pi", "a"]);
    // 没有 nodes、没有 localNode：全按名字。
    expect(buildTree(rows, undefined, undefined).map((g) => g.label)).toEqual(["a", "mac", "pi", "zuan"]);
    // 本机不在 nodes 里也排第一。
    expect(buildTree(rows, [node("zuan")], "pi").map((g) => g.label)).toEqual(["pi", "zuan", "a", "mac"]);
  });

  it("order never changes when status or status_since changes (A48)", () => {
    const nodes = [node("mac"), node("zuan")];
    const before = treeOrder(ROWS, nodes, "mac");
    expect(before.map((r) => r.id)).toEqual(["mac:a", "mac:b", "mac:wt", "mac:sg", "mac:sh", "zuan:z"]);
    // 同一批行：每一行都换状态与起点，头一行变 WAITING（attention 视图会把它顶到最上面）。
    const statuses = ["waiting", "finished", "failed", "turn_done", "idle", "unknown"];
    const changed = ROWS.map((r, i) => ({ ...r, status: statuses[i % statuses.length]!, status_since: 1000 - i, alive: i % 2 === 0 }));
    const after = treeOrder(changed, nodes, "mac");
    expect(after.length).toBe(before.length);
    // 逐元素相等：位置只随创建 / 删除变，不随状态变。
    after.forEach((r, i) => expect(r.id).toBe(before[i]!.id));
    // 传入顺序打乱也一样。
    const shuffled = [...changed].reverse();
    treeOrder(shuffled, nodes, "mac").forEach((r, i) => expect(r.id).toBe(before[i]!.id));
  });

  it("collapsed groups hide rows but ordinals keep counting (A47)", () => {
    const tree = buildTree(ROWS, [node("mac"), node("zuan")], "mac");
    const open = flattenTree(tree, new Set());
    const rowsOf = (entries: ReturnType<typeof flattenTree>) => entries.flatMap((e) => (e.kind === "row" ? [[e.row.id, e.ordinal, e.hidden] as const] : []));
    expect(rowsOf(open)).toEqual([
      ["mac:a", 1, false],
      ["mac:b", 2, false],
      ["mac:wt", 3, false],
      ["mac:sg", 4, false],
      ["mac:sh", 5, false],
      ["zuan:z", 6, false],
    ]);
    // 折叠 agora 仓库组：它下面两个 worktree 的行藏起来，序号照数；组头照旧出现。
    const folded = flattenTree(tree, new Set([`repo:mac:${AGORA}`]));
    expect(rowsOf(folded)).toEqual([
      ["mac:a", 1, true],
      ["mac:b", 2, true],
      ["mac:wt", 3, true],
      ["mac:sg", 4, false],
      ["mac:sh", 5, false],
      ["zuan:z", 6, false],
    ]);
    expect(folded.filter((e) => e.kind === "group").length).toBe(open.filter((e) => e.kind === "group").length);
    // 折叠节点组：整个节点下面全藏。
    const node0 = flattenTree(tree, new Set(["node:mac"]));
    expect(rowsOf(node0).map(([id, , hidden]) => [id, hidden])).toEqual([
      ["mac:a", true],
      ["mac:b", true],
      ["mac:wt", true],
      ["mac:sg", true],
      ["mac:sh", true],
      ["zuan:z", false],
    ]);
    // treeOrder 与展开的 DFS 一致。
    expect(treeOrder(ROWS, [node("mac"), node("zuan")], "mac").map((r) => r.id)).toEqual(rowsOf(open).map(([id]) => id));
  });

  it("filter removes empty groups", () => {
    const nodes = [node("mac"), node("zuan")];
    const kept = fuzzyFilter(treeOrder(ROWS, nodes, "mac"), "parser", rowHaystack);
    expect(kept.map((r) => r.id)).toEqual(["mac:sg"]);
    // 过滤后重建的树只剩有行的组：没有 agora 仓库组、没有其它目录、没有 zuan 节点组。
    expect(outline(buildTree(kept, nodes, "mac"))).toEqual(["node:mac", "  repo:mac:/Users/ricky/code/sglog", "    wt:mac:/Users/ricky/code/sglog", "      mac:sg"]);
    // 过滤只删不换序：留下的行相对顺序与不过滤时一致。
    const some = fuzzyFilter(treeOrder(ROWS, nodes, "mac"), "mac:", rowHaystack).map((r) => r.id);
    expect(some).toEqual(["mac:a", "mac:b", "mac:wt", "mac:sh"]);
  });

  it("collapsed set round-trips through storage and survives garbage", () => {
    const mem = new Map<string, string>();
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
    expect(loadCollapsed(storage)).toEqual(new Set());
    storeCollapsed(new Set(["node:zuan", "wt:mac:/x"]), storage);
    expect(JSON.parse(mem.get(COLLAPSED_STORAGE_KEY)!)).toEqual(["node:zuan", "wt:mac:/x"]);
    expect(loadCollapsed(storage)).toEqual(new Set(["node:zuan", "wt:mac:/x"]));
    mem.set(COLLAPSED_STORAGE_KEY, "{garbage");
    expect(loadCollapsed(storage)).toEqual(new Set());
    mem.set(COLLAPSED_STORAGE_KEY, JSON.stringify([1, "ok", null]));
    expect(loadCollapsed(storage)).toEqual(new Set(["ok"]));
    const throwing = {
      getItem: (): string | null => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadCollapsed(throwing)).toEqual(new Set());
    expect(() => storeCollapsed(new Set(["x"]), throwing)).not.toThrow();
  });
});
