import { describe, expect, it } from "vitest";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import type { NodeStatus } from "./Header";
import { rowHaystack } from "./SessionRow";
import { buildTree, COLLAPSED_STORAGE_KEY, flattenTree, foldSuperseded, historyKey, loadCollapsed, rowGroupKeys, storeCollapsed, treeOrder, type TreeGroup } from "./sidebarTreeModel";

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
    // 同一 worktree 里 a 变 waiting、b 变 failed：按状态排（字母序或 attention 分数）都会把两行对调。
    const statuses = ["failed", "finished", "waiting", "turn_done", "idle", "unknown"];
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
    // 折叠组自己的组头不藏，它下面两个 worktree 的组头藏。
    expect(folded.flatMap((e) => (e.kind === "group" && e.hidden ? [e.group.key] : []))).toEqual([`wt:mac:${AGORA}`, `wt:mac:${WT3}`]);
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

  // superseded 折叠（agora-5gg.19，决策 agora-5gg.8 选 A）：身份仍是对话，呈现层一进程一行。
  // 判据是 `end_cause` 枚举（5gg.6）而不是 reason 那句人话。
  const superseded = (id: string, created: string, extra: Record<string, unknown> = {}): SessionRow =>
    row(id, "mac", created, main, { status: "finished", alive: false, process: "gone", origin: "external", pid: 4242, working_directory: AGORA, end_cause: { kind: "superseded" }, ...extra });

  it("superseded rows of the same process fold under the current row and are not top level (agora-5gg.19)", () => {
    const now = row("mac:now", "mac", "2026-09-18T12:00:03Z", main, { pid: 4242, working_directory: AGORA, origin: "external" });
    const other = row("mac:other", "mac", "2026-09-18T12:00:04Z", main, { pid: 9999, working_directory: AGORA });
    const rows = [superseded("mac:old1", "2026-09-18T12:00:01Z"), superseded("mac:old2", "2026-09-18T12:00:02Z"), now, other];
    const tree = buildTree(rows, undefined, "mac");
    const wt = tree[0]!.children[0]!.children[0]!;
    // 顶层只剩当前行与另一个进程的行；两行旧对话挂在 mac:now 下面，按创建序。
    expect(wt.rows.map((r) => r.id)).toEqual(["mac:now", "mac:other"]);
    expect(wt.history.get("mac:now")!.map((r) => r.id)).toEqual(["mac:old1", "mac:old2"]);
    expect(wt.history.get("mac:other")).toBeUndefined();
    // 树的骨架里根本不会出现旧行（`outline` 只走 `rows`）。
    expect(outline(tree)).toEqual(["node:mac", `  repo:mac:${AGORA}`, `    wt:mac:${AGORA}`, "      mac:now", "      mac:other"]);
    // 计数：宿主行带 2，别人带 0；组件拿它决定画不画「历史对话 N」。
    const counts = (entries: ReturnType<typeof flattenTree>) => entries.flatMap((e) => (e.kind === "row" ? [[e.row.id, e.ordinal, e.hidden, e.historyCount ?? 0, e.hostId ?? "-"] as const] : []));
    expect(counts(flattenTree(tree, new Set()))).toEqual([
      ["mac:now", 1, false, 2, "-"],
      // 默认收起，但序号照数（与折叠组、与 A46 Finished 区同一条规则：不画不是不数）。
      ["mac:old1", 2, true, 0, "mac:now"],
      ["mac:old2", 3, true, 0, "mac:now"],
      ["mac:other", 4, false, 0, "-"],
    ]);
    // 展开那一枚：旧行不再 hidden，别的行的序号一个字不动。
    expect(counts(flattenTree(tree, new Set(), new Set([historyKey("mac:now")])))).toEqual([
      ["mac:now", 1, false, 2, "-"],
      ["mac:old1", 2, false, 0, "mac:now"],
      ["mac:old2", 3, false, 0, "mac:now"],
      ["mac:other", 4, false, 0, "-"],
    ]);
    // treeOrder 把历史行算在内（它们仍可被 Alt/Option+N 选中，选中会自动展开那枚折叠）。
    expect(treeOrder(rows, undefined, "mac").map((r) => r.id)).toEqual(["mac:now", "mac:old1", "mac:old2", "mac:other"]);
  });

  it("superseded rows without a current row stay ordinary finished rows (agora-5gg.19)", () => {
    // 进程退了而桶里全是 superseded：新对话没登记出来（或被 external_finished_ttl 删了）。这些旧行是
    // 仅存的记录，折起来就没有了——按普通 FINISHED 画在顶层。
    const rows = [superseded("mac:old1", "2026-09-18T12:00:01Z"), superseded("mac:old2", "2026-09-18T12:00:02Z")];
    const tree = buildTree(rows, undefined, "mac");
    const wt = tree[0]!.children[0]!.children[0]!;
    expect(wt.rows.map((r) => r.id)).toEqual(["mac:old1", "mac:old2"]);
    expect([...wt.history.keys()]).toEqual([]);
    expect(flattenTree(tree, new Set()).flatMap((e) => (e.kind === "row" && !e.hidden ? [e.row.id] : []))).toEqual(["mac:old1", "mac:old2"]);
    // 过滤同理：旧行单独命中时它回到顶层，不会为了找不着的当前行而消失。
    const kept = fuzzyFilter(treeOrder([...rows, row("mac:now", "mac", "2026-09-18T12:00:03Z", main, { pid: 4242, working_directory: AGORA })], undefined, "mac"), "old1", (r) => r.id);
    expect(outline(buildTree(kept, undefined, "mac"))).toEqual(["node:mac", `  repo:mac:${AGORA}`, `    wt:mac:${AGORA}`, "      mac:old1"]);
  });

  it("the fold bucket is (node, pid) first and (node, working_directory) when there is no pid (agora-5gg.19)", () => {
    const current = (id: string, extra: Record<string, unknown> = {}): SessionRow => row(id, "mac", "2026-09-18T12:00:09Z", main, { origin: "external", ...extra });
    // 拿不到进程号（Codex Desktop 那一类：pid null、process unknown）：退到同一目录。
    const byCwd = foldSuperseded([
      superseded("mac:old", "2026-09-18T12:00:01Z", { pid: null }),
      current("mac:now", { pid: null, working_directory: AGORA }),
    ]);
    expect(byCwd.rows.map((r) => r.id)).toEqual(["mac:now"]);
    expect(byCwd.history.get("mac:now")!.map((r) => r.id)).toEqual(["mac:old"]);
    // pid 优先于目录：同进程的两行是一条工作线，同目录只是同名（决策原文里 notes×8 那种计数）。
    const older = superseded("mac:old", "2026-09-18T12:00:01Z", { pid: 1 });
    const samePid = current("mac:pid1", { pid: 1, working_directory: "/tmp/a" });
    const newerDir = current("mac:pid2", { pid: 2, working_directory: AGORA });
    expect(foldSuperseded([older, newerDir, samePid]).history.get("mac:pid1")!.map((r) => r.id)).toEqual(["mac:old"]);
    // 跨节点不折：pid 在两台机器上各自为政，4242 在 mac 与在 zuan 不是同一个进程。
    const folded = foldSuperseded([superseded("mac:old", "1"), { ...current("zuan:now", { pid: 4242, working_directory: AGORA }), node: "zuan" }]);
    expect(folded.rows.map((r) => r.id)).toEqual(["mac:old", "zuan:now"]);
    expect([...folded.history.keys()]).toEqual([]);
    // pid 与 cwd 都拿不到（老库里的行）：没有人可折。
    expect(foldSuperseded([superseded("mac:old", "1", { pid: null, working_directory: null })]).rows.map((r) => r.id)).toEqual(["mac:old"]);
  });

  it("only end_cause superseded folds, and a chain lands flat under the newest row (agora-5gg.19)", () => {
    // 别的结束原因一律不折：人在终端里自己结束的、探活发现的、运行时会话没了的、按过 Kill 的，
    // 都不是“被新对话换掉”，它们各自还是一条看得见的会话记录。
    const causes: Record<string, unknown> = {
      host_session_end: { kind: "host_session_end", value: "exit" },
      process_gone: { kind: "process_gone" },
      runtime_gone: { kind: "runtime_gone", value: "session" },
      killed_by_user: { kind: "killed_by_user" },
      // 老节点 / 5gg.6 之前写下的检查点：这一格是空的，空不等于 superseded，也不等于「没结束」。
      null_cause: null,
      absent: undefined,
    };
    const stayed: Record<string, string[]> = {};
    for (const [label, cause] of Object.entries(causes)) {
      const rows = [superseded("mac:x", "2026-09-18T12:00:01Z", { end_cause: cause }), row("mac:now", "mac", "2026-09-18T12:00:02Z", main, { pid: 4242, working_directory: AGORA })];
      const folded = foldSuperseded(rows);
      stayed[label] = folded.rows.map((r) => r.id);
      expect([...folded.history.keys()]).toEqual([]);
    }
    // 每一档都留在顶层（同 pid、同目录，本该折得起来——只差 end_cause 不是 superseded）。
    expect(stayed).toEqual({
      host_session_end: ["mac:x", "mac:now"],
      process_gone: ["mac:x", "mac:now"],
      runtime_gone: ["mac:x", "mac:now"],
      killed_by_user: ["mac:x", "mac:now"],
      null_cause: ["mac:x", "mac:now"],
      absent: ["mac:x", "mac:now"],
    });
    // 有运行时句柄的行（origin = agora / adopted）就算带着 superseded 也不折：supersede 只发生在无句柄的
    // 行上（`src/session/manager.rs` 的 supersede_handleless_rows），真出现这一行说明我们对身份的理解错了；
    // 把人正在用的那条 pane 藏起来比多画一行严重得多。headless（无句柄的另一档）照折。
    const handled = foldSuperseded([
      superseded("mac:x", "2026-09-18T12:00:01Z", { origin: "agora" }),
      superseded("mac:h", "2026-09-18T12:00:02Z", { origin: "headless" }),
      row("mac:now", "mac", "2026-09-18T12:00:03Z", main, { pid: 4242, working_directory: AGORA }),
    ]);
    expect(handled.rows.map((r) => r.id)).toEqual(["mac:x", "mac:now"]);
    expect(handled.history.get("mac:h")).toBeUndefined();
    expect(handled.history.get("mac:now")!.map((r) => r.id)).toEqual(["mac:h"]);
    // 词表外的 kind（对端比页面新）同样不折：不认识的枚举当「没说」，不能拿它做隐藏行的决定。
    expect(foldSuperseded([superseded("mac:x", "1", { end_cause: { kind: "replaced_by_desktop" } })]).rows.map((r) => r.id)).toEqual(["mac:x"]);
    // 一条链 A→B→C：B 自己也被 C 换掉了，两行都并进 C——一进程一行，不套两层折叠。
    const chain = [
      superseded("mac:a", "2026-09-18T12:00:01Z"),
      superseded("mac:b", "2026-09-18T12:00:02Z"),
      row("mac:c", "mac", "2026-09-18T12:00:03Z", main, { pid: 4242, working_directory: AGORA, origin: "external" }),
    ];
    const folded = foldSuperseded(chain);
    expect(folded.rows.map((r) => r.id)).toEqual(["mac:c"]);
    expect(folded.history.get("mac:c")!.map((r) => r.id)).toEqual(["mac:a", "mac:b"]);
    // 当前行不因为折叠换位置：它还在自己 created_at 那一格（行的位置只随创建 / 删除变）。
    const withNeighbour = [...chain, row("mac:z", "mac", "2026-09-18T12:00:04Z", sglog) as SessionRow];
    const tree = buildTree(withNeighbour, undefined, "mac");
    expect(outline(tree)).toEqual([
      "node:mac",
      `  repo:mac:${AGORA}`,
      `    wt:mac:${AGORA}`,
      "      mac:c",
      `  repo:mac:/Users/ricky/code/sglog`,
      `    wt:mac:/Users/ricky/code/sglog`,
      "      mac:z",
    ]);
  });

  it("folds a row shaped exactly like GET /api/sessions output (agora-5gg.19)", () => {
    // 上面几笔都是对象字面量，拼错一个键只会让断言变松。这一份是服务端原样发回来的形态（`src/events.rs`
    // 的 to_value(view)：SessionRecord flatten + pid + assessment flatten），键名钉在这里。
    const snapshot = JSON.parse(
      `[{"id":"mac:old","node":"mac","runtime_ref":null,"display_name":"notes","agent_type":"grok",
         "working_directory":"/Users/ricky/notes","origin":"external","created_at":"2026-09-18T12:00:01Z",
         "status":"finished","source":"hook","confidence":0.9,"reason":"superseded: 新对话 9b2e1c",
         "end_cause":{"kind":"superseded"},"unknown_cause":null,"process":"gone","alive":false,"pid":31817,
         "project":{"repo":"/Users/ricky/notes","name":"notes","worktree":"/Users/ricky/notes","branch":null,"main":true}},
        {"id":"mac:now","node":"mac","runtime_ref":null,"display_name":"notes","agent_type":"grok",
         "working_directory":"/Users/ricky/notes","origin":"external","created_at":"2026-09-18T12:00:09Z",
         "status":"running","source":"hook","confidence":0.9,"reason":"prompt submitted",
         "end_cause":null,"unknown_cause":null,"process":"alive","alive":true,"pid":31817,
         "project":{"repo":"/Users/ricky/notes","name":"notes","worktree":"/Users/ricky/notes","branch":null,"main":true}}]`,
    ) as SessionRow[];
    const tree = buildTree(snapshot, undefined, "mac");
    const wt = tree[0]!.children[0]!.children[0]!;
    // 同一个 grok 进程（pid 31817）的两行：顶层只剩当前那一行，旧的那行是它的历史。
    expect(wt.rows.map((r) => r.id)).toEqual(["mac:now"]);
    expect(wt.history.get("mac:now")!.map((r) => r.id)).toEqual(["mac:old"]);
    expect(treeOrder(snapshot, undefined, "mac").map((r) => r.id)).toEqual(["mac:now", "mac:old"]);
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
