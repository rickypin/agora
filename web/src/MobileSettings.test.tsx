// @vitest-environment jsdom
/**
 * /m 设置屏（agora-thc.7）：推送开关、不可达降级提示、加主屏引导、吊销本设备。
 */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MobileSettings } from "./MobileSettings";
import type { PushEnv, SubscriptionLike } from "./push";

const net = vi.hoisted(() => ({ logout: 0 }));

vi.mock("./net", () => ({
  API_PREFIX: "/api/",
  apiFetch: async (path: string): Promise<Response> => {
    const json = (status: number, body: unknown): Response =>
      ({ ok: status >= 200 && status < 300, status, json: async () => body }) as unknown as Response;
    if (path === "/api/auth/logout") {
      net.logout += 1;
      return json(204, null);
    }
    return json(404, {});
  },
}));

const sub: SubscriptionLike = {
  endpoint: "https://web.push.apple.com/A",
  toJSON: () => ({ endpoint: "https://web.push.apple.com/A", keys: { p256dh: "k", auth: "a" } }),
  unsubscribe: async () => true,
};

function env(patch: Partial<PushEnv> = {}): PushEnv {
  return {
    secure: true,
    supported: true,
    ios: false,
    standalone: true,
    permission: () => "granted",
    requestPermission: async () => "granted",
    getSubscription: async () => sub,
    subscribe: async () => sub,
    fetchVapidKey: async () => "B".repeat(87),
    postSubscription: async () => true,
    deleteSubscription: async () => true,
    ...patch,
  };
}

afterEach(() => {
  cleanup();
  net.logout = 0;
});

describe("MobileSettings", () => {
  it("self-checks on open, toggles off through the node, and shows the unreachable hint", async () => {
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => {}}
        env={env()}
        probe={async () => ({ apple: false, reason: "连接超时" })}
      />,
    );
    // 自查：本地有订阅 → 重新登记 → on。
    expect(await screen.findByText("已开启")).toBeTruthy();
    expect(await screen.findByTestId("mobile-push-unreachable").then((el) => el.textContent)).toContain("连接超时");

    const deleted: string[] = [];
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => {}}
        env={env({
          deleteSubscription: async (endpoint) => {
            deleted.push(endpoint);
            return true;
          },
        })}
        probe={async () => ({ apple: null, reason: null })}
      />,
    );
    const toggles = await screen.findAllByTestId("mobile-push-toggle");
    fireEvent.click(toggles[toggles.length - 1]);
    await waitFor(() => expect(deleted).toEqual(["https://web.push.apple.com/A"]));
    expect((await screen.findAllByText("未开启")).length).toBeGreaterThan(0);
  });

  it("guides iOS browser tabs to install first", async () => {
    const ua = Object.getOwnPropertyDescriptor(navigator, "userAgent");
    Object.defineProperty(navigator, "userAgent", { value: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", configurable: true });
    try {
      render(
        <MobileSettings
          onClose={() => {}}
          onRevoked={() => {}}
          env={env({ supported: false, ios: true, standalone: false })}
          probe={async () => ({ apple: null, reason: null })}
        />,
      );
      expect(await screen.findByTestId("install-hint")).toBeTruthy();
      expect(screen.getByTestId("mobile-push-state").textContent).toContain("不支持");
    } finally {
      if (ua) Object.defineProperty(navigator, "userAgent", ua);
    }
  });

  it("revoking the device logs out after confirmation", async () => {
    const revoked: boolean[] = [];
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => revoked.push(true)}
        env={env()}
        probe={async () => ({ apple: null, reason: null })}
      />,
    );
    fireEvent.click(await screen.findByTestId("mobile-revoke-self"));
    expect(screen.getByText("吊销本设备？")).toBeTruthy();
    expect(net.logout).toBe(0);
    fireEvent.click(screen.getByText("吊销", { selector: ".dialog-actions .danger" }));
    await waitFor(() => expect(net.logout).toBe(1));
    await waitFor(() => expect(revoked).toEqual([true]));
  });
});
