/**
 * Web Push 客户端（agora-thc.7）：订阅 / 取消 / 打开自查重订 / 状态归类。
 *
 * 与 notify.ts 同一纪律：`PushManager` 与 `fetch` 经 PushEnv 注入，测试用假实现钉交互顺序
 * （订阅前必须先有公钥、取消先删服务端再退订、自查在权限 granted 时才重订）。
 */
import { describe, expect, it } from "vitest";
import {
  disablePush,
  enablePush,
  pushState,
  selfCheckPush,
  urlBase64ToUint8Array,
  type PushEnv,
  type PushSubscriptionJson,
  type SubscriptionLike,
} from "./push";

function fakeEnv(patch: Partial<PushEnv> = {}) {
  const calls: string[] = [];
  const posted: PushSubscriptionJson[] = [];
  const deleted: string[] = [];
  const sub: SubscriptionLike = {
    endpoint: "https://web.push.apple.com/A",
    toJSON: () => ({ endpoint: "https://web.push.apple.com/A", keys: { p256dh: "k", auth: "a" } }),
    unsubscribe: async () => {
      calls.push("unsubscribe");
      return true;
    },
  };
  const env: PushEnv = {
    secure: true,
    supported: true,
    ios: false,
    standalone: true,
    permission: () => "granted",
    requestPermission: async () => {
      calls.push("request");
      return "granted";
    },
    getSubscription: async () => null,
    subscribe: async (key) => {
      calls.push(`subscribe:${key.length}`);
      return sub;
    },
    fetchVapidKey: async () => {
      calls.push("vapid");
      return "B".repeat(87);
    },
    postSubscription: async (s) => {
      calls.push("post");
      posted.push(s);
      return true;
    },
    deleteSubscription: async (endpoint) => {
      calls.push("delete");
      deleted.push(endpoint);
      return true;
    },
    ...patch,
  };
  return { env, calls, posted, deleted, sub };
}

describe("push subscription", () => {
  it("enables: permission then key then subscribe then register, in that order", async () => {
    const { env, calls, posted } = fakeEnv({ permission: () => "default" });
    const report = await enablePush(env);
    expect(report.state).toBe("on");
    expect(calls).toEqual(["request", "vapid", "subscribe:65", "post"]);
    expect(posted[0].endpoint).toBe("https://web.push.apple.com/A");
  });

  it("reuses an existing subscription instead of subscribing again", async () => {
    const { env, calls, sub } = fakeEnv({ getSubscription: async () => sub });
    expect((await enablePush(env)).state).toBe("on");
    expect(calls).toEqual(["vapid", "post"]);
  });

  it("disables: delete on the node first, then unsubscribe locally", async () => {
    const { env, calls, deleted, sub } = fakeEnv({ getSubscription: async () => sub });
    expect((await disablePush(env)).state).toBe("off");
    expect(deleted).toEqual(["https://web.push.apple.com/A"]);
    expect(calls).toEqual(["delete", "unsubscribe"]);
  });

  it("self-check re-registers an existing subscription and resubscribes when it vanished", async () => {
    const existing = fakeEnv({ getSubscription: async () => fakeEnv().sub });
    expect((await selfCheckPush(existing.env)).state).toBe("on");
    expect(existing.calls).toEqual(["post"]);

    const gone = fakeEnv();
    expect((await selfCheckPush(gone.env)).state).toBe("on");
    expect(gone.calls).toEqual(["vapid", "subscribe:65", "post"]);
  });

  it("never touches permissions when the environment cannot push", async () => {
    const { env, calls } = fakeEnv({ supported: false, permission: () => "default" });
    expect((await enablePush(env)).state).toBe("unsupported");
    expect(calls).toEqual([]);
    const insecure = fakeEnv({ secure: false });
    expect((await enablePush(insecure.env)).state).toBe("insecure");
    expect(insecure.calls).toEqual([]);
  });

  it("reports each blocked reason as its own state", async () => {
    expect((await pushState(fakeEnv({ permission: () => "denied" }).env)).state).toBe("denied");
    expect((await pushState(fakeEnv({ permission: () => "default" }).env)).state).toBe("off");
    expect((await pushState(fakeEnv({ getSubscription: async () => fakeEnv().sub }).env)).state).toBe("on");
    expect((await enablePush(fakeEnv({ fetchVapidKey: async () => null }).env)).state).toBe("no-key");
    expect((await enablePush(fakeEnv({ postSubscription: async () => false }).env)).state).toBe("failed");
  });

  it("ios browser tab says install first", async () => {
    const { env } = fakeEnv({ ios: true, standalone: false, supported: false });
    const report = await enablePush(env);
    expect(report.state).toBe("unsupported");
    expect(report.detail).toContain("主屏幕");
  });
});

describe("urlBase64ToUint8Array", () => {
  it("decodes URL-safe base64 with padding", () => {
    expect([...urlBase64ToUint8Array("AQID")]).toEqual([1, 2, 3]);
    expect([...urlBase64ToUint8Array("-_8")]).toEqual([251, 255]);
  });
});
