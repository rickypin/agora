import { describe, expect, it } from "vitest";
import {
  countByStatus,
  finishedCollapsed,
  formatAgo,
  loadSeen,
  needsAttention,
  partitionByAttention,
  sectionOf,
  SEEN_STORAGE_KEY,
  sortByAttention,
  statusLine,
  storeSeen,
  taskLabel,
} from "./attention";
import type { SessionRow } from "./events";

function row(id: string, status: string, extra: Record<string, unknown> = {}): SessionRow {
  return { id, node: "n", status, alive: true, ...extra };
}

describe("attention", () => {
  it("puts whatever is stuck on a human above whatever is running (A17)", () => {
    // MISSION §6.3 的分数表：FAILED 100 / WAITING 90 / TURN_DONE 85 / FINISHED 80 / UNKNOWN 40 /
    // IDLE 30 / STARTING 20 / RUNNING 10。
    const rows = [
      row("run", "running"),
      row("idle", "idle"),
      row("fin", "finished"),
      row("wait", "waiting"),
      row("start", "starting"),
      row("unk", "unknown"),
      row("done", "turn_done"),
      row("fail", "failed"),
    ];
    expect(sortByAttention(rows).map((r) => r.id)).toEqual(["fail", "wait", "done", "fin", "unk", "idle", "start", "run"]);
    expect(rows.filter((r) => needsAttention(r)).map((r) => r.id)).toEqual(["fin", "wait", "done", "fail"]);
  });

  // A46（agora-j4w.1）：FINISHED 分来源与看没看过。external 的工作面在别的窗口，人在终端里自己结束了会话
  // （MISSION §4.6 证据 ②），agora 这边没有 pane 也没有 Restart——一律直接收进 Finished 区；agora / adopted
  // 来源的要等人在侧栏里选中展开过一次（证据 ①）。
  it("external FINISHED never needs attention; agora/adopted FINISHED does until seen (A46)", () => {
    const ext = row("ext", "finished", { origin: "external" });
    const own = row("own", "finished", { origin: "agora" });
    const adopted = row("ad", "finished", { origin: "adopted" });
    const none = new Set<string>();
    expect(needsAttention(ext)).toBe(false);
    expect(needsAttention(ext, none)).toBe(false);
    expect(finishedCollapsed(ext)).toBe(true);
    expect(needsAttention(own, none)).toBe(true);
    expect(needsAttention(adopted, none)).toBe(true);
    expect(finishedCollapsed(own, none)).toBe(false);
    const seen = new Set(["own"]);
    expect(needsAttention(own, seen)).toBe(false);
    expect(finishedCollapsed(own, seen)).toBe(true);
    expect(needsAttention(adopted, seen)).toBe(true);
    // 看过只对 FINISHED 有意义：WAITING 行在集合里也照样"等你"，RUNNING 行也不会因此进 Finished 区。
    expect(needsAttention(row("w", "waiting"), new Set(["w"]))).toBe(true);
    expect(finishedCollapsed(row("r", "running", { origin: "external" }))).toBe(false);
    expect(sectionOf(ext)).toBe("finished");
    expect(sectionOf(own, none)).toBe("attention");
    expect(sectionOf(own, seen)).toBe("finished");
    expect(sectionOf(row("r", "running"))).toBe("running");
  });

  it("three-way partition keeps sortByAttention order inside every segment (Alt/Option+N invariant, A46)", () => {
    // 故意乱序传入：sortByAttention 先排，partition 只分段不换序。
    const rows = [
      row("run", "running", { status_since: 5 }),
      row("ext-fin", "finished", { origin: "external", status_since: 1 }),
      row("seen-fin", "finished", { origin: "agora", status_since: 2 }),
      row("wait", "waiting"),
      row("new-fin", "finished", { origin: "adopted", status_since: 3 }),
      row("idle", "idle"),
      row("ext-fin-2", "finished", { origin: "external", status_since: 4 }),
      row("fail", "failed"),
    ];
    const seen = new Set(["seen-fin"]);
    const sorted = sortByAttention(rows);
    const shown = partitionByAttention(sorted, seen);
    // 三段各自等于 sortByAttention 顺序按段过滤的结果，拼起来就是显示顺序。
    const by = (section: string) => sorted.filter((r) => sectionOf(r, seen) === section).map((r) => r.id);
    expect(shown.map((r) => r.id)).toEqual([...by("attention"), ...by("running"), ...by("finished")]);
    expect(shown.map((r) => r.id)).toEqual(["fail", "wait", "new-fin", "idle", "run", "ext-fin", "seen-fin", "ext-fin-2"]);
    // 一行不多一行不少：折叠区的行仍在序列里，序号照数。
    expect(shown.length).toBe(rows.length);
    // 过滤（只删不换序）与分段可交换：过滤后再分段 = 分段后再过滤，Alt/Option+N 在过滤后仍指向眼睛看到的第 N 条。
    const keep = (r: SessionRow) => r.id !== "wait" && r.id !== "ext-fin";
    expect(partitionByAttention(sorted.filter(keep), seen).map((r) => r.id)).toEqual(shown.filter(keep).map((r) => r.id));
    // 不传 seen：agora 来源的 FINISHED 全在 NEEDS ATTENTION，external 的仍收起。
    expect(partitionByAttention(sorted).map((r) => r.id)).toEqual(["fail", "wait", "seen-fin", "new-fin", "idle", "run", "ext-fin", "ext-fin-2"]);
  });

  it("persists the seen set defensively: bad JSON, missing storage and throwing storage all read as empty", () => {
    const mem = new Map<string, string>();
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
    expect(loadSeen(storage).size).toBe(0);
    storeSeen(new Set(["a", "b"]), storage);
    expect(mem.get(SEEN_STORAGE_KEY)).toBe(JSON.stringify(["a", "b"]));
    expect([...loadSeen(storage)]).toEqual(["a", "b"]);
    mem.set(SEEN_STORAGE_KEY, "{not json");
    expect(loadSeen(storage).size).toBe(0);
    mem.set(SEEN_STORAGE_KEY, JSON.stringify(["x", 3, null]));
    expect([...loadSeen(storage)]).toEqual(["x"]);
    expect(loadSeen(null).size).toBe(0);
    const throwing = {
      getItem: (): string | null => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadSeen(throwing).size).toBe(0);
    expect(() => storeSeen(new Set(["a"]), throwing)).not.toThrow();
  });

  it("breaks ties by bd priority, then by how long it has waited, then keeps order (A23)", () => {
    const rows = [
      row("p2-late", "waiting", { status_since: 200 }),
      row("nobd", "waiting", { status_since: 100 }), // 无 bd 视为 P2
      row("p3", "waiting", { task: { id: "x-3", title: "t", priority: 3 }, status_since: 1 }),
      row("p0", "waiting", { task: { id: "x-0", title: "t", priority: 0 }, status_since: 300 }),
      row("p2-early", "waiting", { task: { id: "x-2", title: "t", priority: 2 }, status_since: 50 }),
      row("p2-same", "waiting", { status_since: 100 }),
    ];
    expect(sortByAttention(rows).map((r) => r.id)).toEqual(["p0", "p2-early", "nobd", "p2-same", "p2-late", "p3"]);
  });

  it("partition keeps NEEDS ATTENTION first without reordering inside a group", () => {
    const rows = [row("a", "running"), row("b", "waiting"), row("c", "running"), row("d", "failed")];
    expect(partitionByAttention(rows).map((r) => r.id)).toEqual(["b", "d", "a", "c"]);
  });

  it("labels a row by task: issue id + title > prompt summary > display name", () => {
    expect(taskLabel(row("a", "running", { task: { id: "agora-1", title: "写 MISSION", priority: 1 }, task_ref: "agora-1", name: "s" }))).toBe(
      "agora-1 写 MISSION",
    );
    expect(taskLabel(row("a", "running", { task_ref: "修 migration 回滚", name: "s" }))).toBe("修 migration 回滚");
    expect(taskLabel(row("a", "running", { name: "sglog", display_name: "x" }))).toBe("sglog");
    expect(taskLabel(row("n:abc", "running"))).toBe("n:abc");
  });

  it("status line carries the wait duration from status_since", () => {
    expect(statusLine(row("a", "waiting", { status_since: 1000 }), 1000 + 3 * 60 + 5)).toBe("waiting 3m");
    expect(statusLine(row("a", "turn_done", { status_since: 1000 }), 1000 + 30)).toBe("turn done");
    expect(statusLine(row("a", "running"), 5)).toBe("working");
    expect(formatAgo(59)).toBe("");
    expect(formatAgo(3600 * 5)).toBe("5h");
    expect(formatAgo(3600 * 72)).toBe("3d");
  });

  it("counts by status for the header", () => {
    const c = countByStatus([row("a", "running"), row("b", "starting"), row("c", "waiting"), row("d", "turn_done"), row("e", "weird")]);
    expect(c).toEqual({ running: 2, needsInput: 1, turnDone: 1, finished: 0, failed: 0, idle: 0, unknown: 1 });
  });
});
