/**
 * service worker 的推送处理（agora-thc.7）：push 展示通知的形态与 tag、notificationclick 的
 * 导航与页面消息。
 *
 * sw.js 是 classic script、不是模块：这里先造一个假 `self`（capture addEventListener），再动态
 * import——测的就是浏览器里跑的那份字节，不是抄出来的一份重写。
 */
import { beforeAll, describe, expect, it, vi } from "vitest";

type Handler = (event: unknown) => void;

const handlers = new Map<string, Handler>();
const shown: Array<{ title: string; opts: { tag?: string; data?: { session: string | null } } }> = [];
const focused: string[] = [];
const openWindowed: string[] = [];
const posted: Array<{ url: string; message: unknown }> = [];
let windowClients: Array<{ url: string; focus: () => Promise<void>; postMessage: (m: unknown) => void }> = [];

const fakeSelf = {
  location: { origin: "https://zuan.example:7681" },
  addEventListener: (type: string, fn: Handler) => handlers.set(type, fn),
  skipWaiting: () => {},
  clients: {
    claim: async () => {},
    matchAll: async () => windowClients,
    openWindow: async (url: string) => {
      openWindowed.push(url);
    },
  },
  registration: {
    showNotification: async (title: string, opts: { tag?: string; data?: { session: string | null } }) => {
      shown.push({ title, opts });
    },
  },
};

beforeAll(async () => {
  vi.stubGlobal("self", fakeSelf);
  // sw.js 是 classic script（不是模块、没有 .d.ts）：类型检查在这里让步，跑的是真文件。
  // @ts-expect-error 未声明模块
  await import("../public/sw.js");
});

/** 跑一个 SW 事件：收集 waitUntil 的 promise 并等它们落定。 */
async function fire(type: string, event: Record<string, unknown>) {
  const pending: Promise<unknown>[] = [];
  const handler = handlers.get(type);
  expect(handler, `sw.js 应注册 ${type}`).toBeTruthy();
  handler!({ ...event, waitUntil: (p: Promise<unknown>) => pending.push(p) });
  await Promise.all(pending);
}

function reset() {
  shown.length = 0;
  focused.length = 0;
  openWindowed.length = 0;
  posted.length = 0;
  windowClients = [];
}

describe("service worker push", () => {
  it("shows the server title with tag = session and the session in data", async () => {
    reset();
    await fire("push", { data: { json: () => ({ title: "Claude / 修 CI @ zuan needs input", session: "zuan:abc" }) } });
    expect(shown).toHaveLength(1);
    expect(shown[0].title).toContain("needs input");
    expect(shown[0].opts.tag).toBe("zuan:abc");
    expect(shown[0].opts.data).toEqual({ session: "zuan:abc" });
  });

  it("survives a bad payload with a safe fallback", async () => {
    reset();
    await fire("push", { data: { json: () => { throw new Error("not json"); } } });
    expect(shown[0].title).toBe("agora");
    expect(shown[0].opts.tag).toBe("agora");
  });

  it("notificationclick focuses an open /m client and posts the session", async () => {
    reset();
    windowClients = [
      { url: "https://zuan.example:7681/", focus: async () => {}, postMessage: () => {} },
      {
        url: "https://zuan.example:7681/m",
        focus: async () => {
          focused.push("/m");
        },
        postMessage: (m) => posted.push({ url: "/m", message: m }),
      },
    ];
    await fire("notificationclick", {
      notification: { close: () => {}, data: { session: "zuan:abc" } },
    });
    expect(focused).toEqual(["/m"]);
    expect(posted).toEqual([{ url: "/m", message: { type: "agora-open-session", session: "zuan:abc" } }]);
    expect(openWindowed).toEqual([]);
  });

  it("notificationclick opens /m?session=… when nothing is open", async () => {
    reset();
    await fire("notificationclick", {
      notification: { close: () => {}, data: { session: "zuan:abc" } },
    });
    expect(openWindowed).toEqual(["/m?session=zuan%3Aabc"]);
  });

  it("notificationclick without a session opens plain /m", async () => {
    reset();
    await fire("notificationclick", { notification: { close: () => {}, data: null } });
    expect(openWindowed).toEqual(["/m"]);
  });
});
