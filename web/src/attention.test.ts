import { describe, expect, it } from "vitest";
import {
  countByStatus,
  formatAgo,
  needsAttention,
  partitionByAttention,
  sortByAttention,
  statusLine,
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
    expect(rows.filter(needsAttention).map((r) => r.id)).toEqual(["fin", "wait", "done", "fail"]);
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
