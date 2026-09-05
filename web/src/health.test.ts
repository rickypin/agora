import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HealthWatcher, isHealthy, runtimeDegraded } from "./health";

describe("isHealthy", () => {
  it("accepts the public subset", () => {
    expect(isHealthy({ status: "ok" })).toBe(true);
  });
  it("rejects anything else", () => {
    expect(isHealthy({ status: "degraded" })).toBe(false);
    expect(isHealthy(null)).toBe(false);
    expect(isHealthy("ok")).toBe(false);
  });
});

describe("runtimeDegraded", () => {
  it("returns the runtime's own reason verbatim when it reports degraded", () => {
    const reason = "运行时 server 不可用: protocol version mismatch (client 8, server 7)";
    expect(runtimeDegraded({ status: "ok", runtime: { status: "degraded", reason, path_source: "shell" } })).toBe(reason);
  });
  it("is null for a healthy runtime and for the unauthenticated public subset (ADR-003 D1)", () => {
    expect(runtimeDegraded({ status: "ok", runtime: { status: "ok", reason: null, path_source: "shell" } })).toBeNull();
    // 公开子集没有 runtime 段：不是 degraded，也不许据此显示任何节点配置。
    expect(runtimeDegraded({ status: "ok" })).toBeNull();
    expect(runtimeDegraded(null)).toBeNull();
  });
  it("never shows an empty banner: a degraded report without a reason gets a placeholder", () => {
    expect(runtimeDegraded({ status: "ok", runtime: { status: "degraded", reason: "  " } })).toBe("原因未知");
  });
});

describe("HealthWatcher", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  async function tick(ms: number) {
    await vi.advanceTimersByTimeAsync(ms);
  }

  it("polls slowly while ok, every 10 s while degraded, and clears once the runtime recovers (agora-bgr)", async () => {
    let report: unknown = { status: "ok", runtime: { status: "ok", reason: null } };
    const w = new HealthWatcher({ fetchHealth: async () => report, okMs: 60_000, degradedMs: 10_000 });
    const seen: (string | null)[] = [];
    w.subscribe(() => seen.push(w.snapshot()));
    w.start();
    await tick(0);
    expect(w.polls).toBe(1);
    expect(w.snapshot()).toBeNull();
    expect(seen).toEqual([]); // 没变就不通知

    // 运行时失明：下一次例行重拉才看得到（60 s），之后 10 s 一次。
    report = { status: "ok", runtime: { status: "degraded", reason: "tmux 3.0 < 3.2" } };
    await tick(59_000);
    expect(w.snapshot()).toBeNull();
    await tick(1_000);
    expect(w.snapshot()).toBe("tmux 3.0 < 3.2");
    expect(seen).toEqual(["tmux 3.0 < 3.2"]);
    await tick(10_000);
    expect(w.polls).toBe(3);
    // 同一原因反复报：不通知。
    expect(seen).toEqual(["tmux 3.0 < 3.2"]);

    // 恢复：10 s 内横幅的数据源转回 null。
    report = { status: "ok", runtime: { status: "ok", reason: null } };
    await tick(10_000);
    expect(w.snapshot()).toBeNull();
    expect(seen).toEqual(["tmux 3.0 < 3.2", null]);

    w.stop();
    const polls = w.polls;
    await tick(120_000);
    expect(w.polls).toBe(polls); // 停了就不再拉
  });

  it("keeps the last verdict when the report cannot be fetched, instead of flapping", async () => {
    let fail = false;
    const w = new HealthWatcher({
      fetchHealth: async () => {
        if (fail) throw new Error("daemon away");
        return { status: "ok", runtime: { status: "degraded", reason: "r" } };
      },
      degradedMs: 10_000,
    });
    w.start();
    await tick(0);
    expect(w.snapshot()).toBe("r");
    fail = true;
    await tick(10_000);
    expect(w.snapshot()).toBe("r");
    w.stop();
  });

  it("refresh() pulls immediately and leaves exactly one timer behind", async () => {
    let report: unknown = { status: "ok", runtime: { status: "degraded", reason: "r" } };
    const w = new HealthWatcher({ fetchHealth: async () => report, okMs: 60_000, degradedMs: 10_000 });
    w.start();
    await tick(0);
    report = { status: "ok", runtime: { status: "ok", reason: null } };
    await w.refresh();
    expect(w.snapshot()).toBeNull();
    expect(w.polls).toBe(2);
    // 只剩 refresh 排的那一个 60 s 定时器：中途没有多出来的 10 s 那一次。
    await tick(59_000);
    expect(w.polls).toBe(2);
    await tick(1_000);
    expect(w.polls).toBe(3);
    w.stop();
  });
});
