import { describe, expect, it } from "vitest";
import { partitionByAttention, sortByAttention } from "./attention";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import { rowHaystack } from "./Sidebar";
import { loadMode, MODE_STORAGE_KEY, storeMode, visibleOrder } from "./sidebarMode";

function row(id: string, status: string, extra: Record<string, unknown> = {}): SessionRow {
  return { id, node: "n", status, alive: true, ...extra };
}

describe("sidebarMode", () => {
  it("loadMode falls back to attention on missing, garbage, and throwing storage", () => {
    const mem = new Map<string, string>();
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
    expect(loadMode(storage)).toBe("attention");
    expect(loadMode(null)).toBe("attention");
    mem.set(MODE_STORAGE_KEY, "garbage");
    expect(loadMode(storage)).toBe("attention");
    mem.set(MODE_STORAGE_KEY, "tree");
    expect(loadMode(storage)).toBe("tree");
    mem.set(MODE_STORAGE_KEY, "attention");
    expect(loadMode(storage)).toBe("attention");
    const throwing = {
      getItem: (): string | null => {
        throw new Error("denied");
      },
    };
    expect(loadMode(throwing)).toBe("attention");
  });

  it("storeMode round-trips", () => {
    const mem = new Map<string, string>();
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
    storeMode("tree", storage);
    expect(mem.get(MODE_STORAGE_KEY)).toBe("tree");
    expect(loadMode(storage)).toBe("tree");
    storeMode("attention", storage);
    expect(loadMode(storage)).toBe("attention");
    const throwing = {
      getItem: (): string | null => null,
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(() => storeMode("tree", throwing)).not.toThrow();
  });

  it("visibleOrder in attention mode equals partitionByAttention(fuzzyFilter(sortByAttention))", () => {
    const rows = [
      row("run", "running", { status_since: 5 }),
      row("ext-fin", "finished", { origin: "external", status_since: 1, display_name: "alpha" }),
      row("wait", "waiting", { display_name: "alpha" }),
      row("idle", "idle"),
      row("fail", "failed", { display_name: "alpha" }),
    ];
    const seen = new Set(["run@5"]);
    const filter = "alpha";
    const got = visibleOrder("attention", rows, filter, seen);
    const want = partitionByAttention(fuzzyFilter(sortByAttention(rows), filter, rowHaystack), seen);
    expect(got).toEqual(want);
    expect(got.map((r) => r.id)).toEqual(["fail", "wait", "ext-fin"]);
  });

  it("visibleOrder in tree mode is creation order and filter only removes rows", () => {
    const rows = [
      row("c", "waiting", { created_at: "2026-09-08T12:00:02Z", display_name: "keep" }),
      row("b", "failed", { created_at: "2026-09-08T12:00:01Z", display_name: "keep" }),
      row("a", "running", { created_at: "2026-09-08T12:00:01Z", display_name: "drop" }),
      row("d", "idle", { created_at: "2026-09-08T12:00:03Z", display_name: "keep" }),
    ];
    const none = new Set<string>();
    expect(visibleOrder("tree", rows, "", none).map((r) => r.id)).toEqual(["a", "b", "c", "d"]);
    // 过滤只删不换序：同秒 a/b 里 a 被滤掉，b 仍在 c 前。
    expect(visibleOrder("tree", rows, "keep", none).map((r) => r.id)).toEqual(["b", "c", "d"]);
  });
});
