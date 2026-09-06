// @vitest-environment jsdom
/**
 * 配对链接在 StrictMode 下只兑换一次（agora-3w8）。
 *
 * React 19 StrictMode 的开发模式对 effect 同步地 挂载 → 清理 → 再挂载，App 里读 `#pair=<token>`
 * 并 POST /api/auth/pair 的 effect 因此跑两遍、两遍都读到同一个 token。2026-09-06 agora-hhu 代检时
 * daemon 日志同一毫秒两条 POST：第一条 200，第二条 401（token 单次使用，ADR-003 D2），页面据第二条
 * 显示「配对链接无效」，其实 cookie 已发。这里用 `<StrictMode>` 真实地触发双跑，网络出口换成一个
 * 会"用掉"token 的替身（同一 token 第二次 POST 就 401，与 daemon 同构），断言只 POST 一次且页面进入
 * 已配对。Workspace 换成占位：它自己的行为归 Workspace.test。
 *
 * 前端唯一的 `fetch(` 在 net.ts（tests/arch_boundary.rs 守卫，测试文件也在扫描范围内），所以这里
 * mock 的是 `./net` 的 apiFetch，不碰全局 fetch。
 */
import { act, cleanup, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

const net = vi.hoisted(() => ({
  /** 每次 POST /api/auth/pair 的 token，按发出顺序。 */
  pairPosts: [] as string[],
  /** 已经被用掉的 token：再来一次就 401，与 daemon 的单次使用同构。 */
  consumed: new Set<string>(),
  /** `GET /api/auth/devices` 该不该 200（有没有已有 session）。 */
  devicesOk: false,
}));

vi.mock("./Workspace", () => ({
  Workspace: () => <div>workspace-placeholder</div>,
}));

vi.mock("./net", () => ({
  API_PREFIX: "/api/",
  apiFetch: async (path: string, init: RequestInit = {}): Promise<Response> => {
    const json = (status: number, body: unknown): Response =>
      ({ ok: status >= 200 && status < 300, status, json: async () => body }) as unknown as Response;
    if (path === "/api/health") return json(200, { status: "ok" });
    if (path === "/api/auth/devices") return json(net.devicesOk ? 200 : 401, {});
    if (path === "/api/auth/pair" && init.method === "POST") {
      const { token } = JSON.parse(String(init.body)) as { token: string };
      net.pairPosts.push(token);
      if (net.consumed.has(token)) return json(401, { error: "unauthorized" });
      net.consumed.add(token);
      return json(200, { device: { id: "d1", name: "test" } });
    }
    return json(404, {});
  },
}));

function renderStrict(hash: string) {
  window.location.hash = hash;
  return render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

describe("App pair gate under StrictMode", () => {
  beforeEach(() => {
    net.pairPosts.length = 0;
    net.devicesOk = false;
  });
  afterEach(() => {
    cleanup();
    window.history.replaceState(null, "", "/");
  });

  it("strict_mode_double_effect_redeems_the_pair_link_once", async () => {
    renderStrict("#pair=abc");
    // 挂载 → 清理 → 再挂载已经同步发生：此刻两次 effect 都读到了 token，POST 只能有一次。
    expect(net.pairPosts).toEqual(["abc"]);
    await act(async () => {});
    expect(await screen.findByText("workspace-placeholder")).toBeTruthy();
    expect(net.pairPosts).toEqual(["abc"]);
    // 兑换落定后 token 从地址栏抹掉（单次使用，留着只会误导）。
    expect(window.location.hash).toBe("");
  });

  it("a_second_token_is_redeemed_separately", async () => {
    // 上一条已经把 "abc" 兑换过并缓存；换一个 token 必须真的再 POST 一次，而不是拿旧 promise。
    renderStrict("#pair=def");
    expect(net.pairPosts).toEqual(["def"]);
    expect(await screen.findByText("workspace-placeholder")).toBeTruthy();
    expect(net.pairPosts).toEqual(["def"]);
  });
});
