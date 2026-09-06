import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { API_VERSION, checkApiVersion, HealthWatcher, isHealthy, isPeerError, localNodeOf, peersOf, runtimeDegraded, versionBlocked, VersionWatcher } from "./health";

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

  it("carries the peers section out of the same poll and only notifies when it changes (agora-7ku.12)", async () => {
    const zuan = { online: true, last_seen: "2026-09-02T23:10:00Z", retrying: false, last_error: null };
    let report: unknown = { status: "ok", runtime: { status: "ok", reason: null }, peers: { zuan } };
    let fail = false;
    const w = new HealthWatcher({
      fetchHealth: async () => {
        if (fail) throw new Error("daemon away");
        return report;
      },
      okMs: 60_000,
      degradedMs: 10_000,
    });
    let notified = 0;
    let bannerNotified = 0;
    w.subscribeNodes(() => (notified += 1));
    w.subscribe(() => (bannerNotified += 1));
    expect(w.nodesSnapshot()).toEqual({ reachable: null, peers: {} });
    w.start();
    await tick(0);
    const first = w.nodesSnapshot();
    expect(first).toEqual({ reachable: true, peers: { zuan } });
    expect(notified).toBe(1);
    expect(w.polls).toBe(1); // 没有第二条轮询

    // 同样的报告：引用不换、不通知（useSyncExternalStore 靠引用判等）。
    await tick(60_000);
    expect(w.nodesSnapshot()).toBe(first);
    expect(notified).toBe(1);

    // peer 掉线：下一次例行重拉带出 stale + 类型化的原因。
    report = { status: "ok", runtime: { status: "ok", reason: null }, peers: { zuan: { ...zuan, online: false, retrying: true, last_error: "fingerprint_mismatch" } } };
    await tick(60_000);
    expect(w.nodesSnapshot().peers.zuan).toMatchObject({ online: false, retrying: true, last_error: "fingerprint_mismatch", last_seen: zuan.last_seen });
    expect(notified).toBe(2);

    // 本机拉不到：reachable 变 false，peer 的最后一眼保留（不变量 8），degraded 的结论不动。
    fail = true;
    await tick(60_000);
    expect(w.nodesSnapshot()).toEqual({ reachable: false, peers: { zuan: { ...zuan, online: false, retrying: true, last_error: "fingerprint_mismatch" } } });
    expect(w.snapshot()).toBeNull();
    expect(notified).toBe(3);
    // 节点上上下下从不惊动横幅的订阅者（agora-bgr 的"没变就不通知"照旧成立）。
    expect(bannerNotified).toBe(0);
    w.stop();
  });
});

describe("isPeerError", () => {
  it("knows exactly the five typed values of last_error (rule 10; agora-41e adds misconfigured)", () => {
    for (const v of ["incompatible_version", "fingerprint_mismatch", "unauthorized", "unreachable", "misconfigured"]) {
      expect(isPeerError(v)).toBe(true);
    }
    expect(isPeerError("something_new")).toBe(false);
    expect(isPeerError(null)).toBe(false);
    expect(isPeerError(42)).toBe(false);
  });
});

describe("peersOf", () => {
  it("normalizes the peers section and never lets one bad entry break the rest", () => {
    expect(peersOf({ status: "ok" })).toEqual({});
    expect(peersOf(null)).toEqual({});
    expect(
      peersOf({
        status: "ok",
        peers: {
          ok: { online: true, last_seen: "2026-09-02T23:10:00Z", retrying: false, last_error: null },
          cfg: { online: false, last_seen: "2026-09-02T23:10:00Z", retrying: false, last_error: "misconfigured" },
          odd: { online: "yes", last_seen: 5, last_error: "something_new" },
          junk: 42,
        },
      }),
    ).toEqual({
      ok: { online: true, last_seen: "2026-09-02T23:10:00Z", retrying: false, last_error: null },
      // 配置错误原样带出：retrying false 是它的语义（节点不重试、定时重读配置），不是缺字段。
      cfg: { online: false, last_seen: "2026-09-02T23:10:00Z", retrying: false, last_error: "misconfigured" },
      // 不认识的 last_error 当 null——前端只认五个类型，宁可少说也不瞎说。
      odd: { online: false, last_seen: null, retrying: false, last_error: null },
    });
  });
});

// docs/spec/api.md「api_version 兼容规则」的浏览器半边（agora-7ku.4）。
describe("checkApiVersion", () => {
  const page = { major: 1, minor: 0 };
  const system = (api_version: unknown) => ({ api_version, version: "0.1.0", node: "mac" });

  it("same major is compatible whatever the minor, in both directions", () => {
    expect(checkApiVersion(system({ major: 1, minor: 0 }), page)).toEqual({ kind: "compatible", node: { major: 1, minor: 0 }, page });
    // 节点 minor 更大：多出来的字段本方忽略；更小：缺的字段按缺省。都不许拒绝对话。
    expect(checkApiVersion(system({ major: 1, minor: 7 }), page).kind).toBe("compatible");
    expect(checkApiVersion(system({ major: 1, minor: 0 }), { major: 1, minor: 7 }).kind).toBe("compatible");
  });

  it("a different major is incompatible, in both directions", () => {
    expect(checkApiVersion(system({ major: 2, minor: 0 }), page)).toEqual({ kind: "major_mismatch", node: { major: 2, minor: 0 }, page });
    expect(checkApiVersion(system({ major: 1, minor: 99 }), { major: 2, minor: 0 }).kind).toBe("major_mismatch");
  });

  it("an api_version it cannot read is unreadable, never guessed as 1.0", () => {
    // 缺失、旧二进制的裸整数、字符串、负数、缺 minor、非整数、根本不是对象。
    for (const bad of [system(undefined), system(1), system("1.0"), system({ major: -1, minor: 0 }), system({ major: 1 }), system({ major: 1.5, minor: 0 }), null, "ok"]) {
      expect(checkApiVersion(bad, page)).toEqual({ kind: "unreadable", page });
    }
  });

  it("the page's own version is the default", () => {
    expect(checkApiVersion(system(API_VERSION)).kind).toBe("compatible");
  });
});

describe("versionBlocked", () => {
  const page = { major: 1, minor: 0 };
  it("is null while compatible or before any verdict", () => {
    expect(versionBlocked(null)).toBeNull();
    expect(versionBlocked({ kind: "compatible", node: { major: 1, minor: 3 }, page })).toBeNull();
  });
  it("tells a stale tab to refresh and an old node to upgrade, by comparing numbers not text", () => {
    expect(versionBlocked({ kind: "major_mismatch", node: { major: 2, minor: 1 }, page })).toBe("节点 API 版本 2.1，页面按 1.0 构建，请刷新页面");
    expect(versionBlocked({ kind: "major_mismatch", node: { major: 1, minor: 0 }, page: { major: 2, minor: 0 } })).toBe(
      "节点 API 版本 1.0，页面按 2.0 构建，请升级节点后刷新页面",
    );
    expect(versionBlocked({ kind: "unreadable", page })).toBe("节点没有报告可识别的 API 版本，页面按 1.0 构建，请升级节点后刷新页面");
  });
});

describe("VersionWatcher", () => {
  it("carries the node's own id out of the same /api/system fetch and keeps it when a later fetch fails (agora-7ku.5)", async () => {
    let report: () => unknown = () => ({ api_version: API_VERSION, version: "x", node: "mac" });
    const w = new VersionWatcher({ fetchSystem: async () => report() });
    let notified = 0;
    w.subscribe(() => (notified += 1));
    expect(w.nodeSnapshot()).toBeNull();
    await w.check();
    expect(w.nodeSnapshot()).toBe("mac");
    expect(notified).toBe(1);
    // 拉不到：沿用；同样的结论：不通知。
    report = () => {
      throw new Error("down");
    };
    await w.check();
    expect(w.nodeSnapshot()).toBe("mac");
    await w.check();
    expect(notified).toBe(1);
    // node 不是非空字符串当没报，沿用上一次。
    report = () => ({ api_version: API_VERSION, version: "x", node: 7 });
    await w.check();
    expect(w.nodeSnapshot()).toBe("mac");
    expect(localNodeOf({ node: "" })).toBeNull();
    expect(localNodeOf(null)).toBeNull();
    expect(localNodeOf({ node: "zuan" })).toBe("zuan");
  });

  it("has no verdict until a fetch succeeds, then keeps the last verdict when the node cannot be reached", async () => {
    let report: () => unknown = () => {
      throw new Error("daemon away");
    };
    const w = new VersionWatcher({ fetchSystem: async () => report(), page: { major: 1, minor: 0 } });
    const seen: unknown[] = [];
    w.subscribe(() => seen.push(w.snapshot()?.kind));
    await w.check();
    expect(w.snapshot()).toBeNull(); // 拉不到 ≠ 不兼容
    expect(seen).toEqual([]);

    report = () => ({ api_version: { major: 2, minor: 0 }, version: "9", node: "n" });
    await w.check();
    expect(w.snapshot()?.kind).toBe("major_mismatch");
    await w.check(); // 同一结论：不通知
    expect(seen).toEqual(["major_mismatch"]);

    // daemon 重启中拉不到：沿用"不兼容"，不闪回正常。
    report = () => {
      throw new Error("restarting");
    };
    await w.check();
    expect(w.snapshot()?.kind).toBe("major_mismatch");

    report = () => ({ api_version: { major: 1, minor: 4 }, version: "1", node: "n" });
    await w.check();
    expect(w.snapshot()?.kind).toBe("compatible");
    expect(seen).toEqual(["major_mismatch", "compatible"]);
    expect(w.checks).toBe(5);
  });
});
