/**
 * SessionStore 的节点钟基准（agora-au5）。
 *
 * `GET /api/sessions` 响应顶层带报告方（节点）自己那只钟的读数 `now`（docs/spec/api.md「peer
 * 视图」）；页面算时长要拿它当基准，所以 store 在每次快照到达时把 `now` 与收到它的页面钟时刻
 * 配成一对存下来。老节点 / 测试桩不发 `now`：存 null，调用方退回页面钟（行为与以前一致）。
 */
import { describe, expect, it } from "vitest";
import type { Snapshot } from "./events";
import { SessionStore } from "./store";

function snapshot(extra: Record<string, unknown> = {}): Snapshot {
  return { sessions: [], unregistered: [], ...extra } as Snapshot;
}

describe("SessionStore · 节点钟基准 (agora-au5)", () => {
  it("captures the snapshot's top-level now with the page clock reading at receipt", async () => {
    const store = new SessionStore({ fetchSnapshot: async () => snapshot({ now: 42 }) });
    expect(store.clockSnapshot()).toBeNull();
    await store.client.refresh();
    const clock = store.clockSnapshot();
    expect(clock?.now).toBe(42);
    // `at` 是收到那份快照时页面钟的读数（秒），与 now 配对算节点钟的"现在"。
    expect(clock?.at).toBe(Math.floor(Date.now() / 1000));
  });

  it("falls back to null for old nodes that do not send now, and for bogus values", async () => {
    const store = new SessionStore({ fetchSnapshot: async () => snapshot() });
    await store.client.refresh();
    expect(store.clockSnapshot()).toBeNull();
    const bogus = new SessionStore({ fetchSnapshot: async () => snapshot({ now: "42" }) });
    await bogus.client.refresh();
    expect(bogus.clockSnapshot()).toBeNull();
    const nan = new SessionStore({ fetchSnapshot: async () => snapshot({ now: Number.NaN }) });
    await nan.client.refresh();
    expect(nan.clockSnapshot()).toBeNull();
  });

  it("replaces the anchor on every snapshot so the elapsed time restarts from the newest now", async () => {
    let now = 100;
    const store = new SessionStore({ fetchSnapshot: async () => snapshot({ now }) });
    await store.client.refresh();
    expect(store.clockSnapshot()?.now).toBe(100);
    now = 200;
    await store.client.refresh();
    expect(store.clockSnapshot()?.now).toBe(200);
  });
});
