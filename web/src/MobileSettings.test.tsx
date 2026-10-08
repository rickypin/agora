// @vitest-environment jsdom
/**
 * /m 设置屏（agora-thc.7）：推送开关、不可达降级提示、加主屏引导、吊销本设备。
 */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MobileSettings } from "./MobileSettings";
import { DEFAULT_MOBILE_TEXT_SIZE, MOBILE_TEXT_SIZES, MOBILE_TEXT_STORAGE_KEY } from "./mobileText";
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

/**
 * 防回归（2026-10-07 iPhone 实测）：设置页不得因为 effect 依赖每次渲染都新建的对象而死循环。
 * `browserPushEnv()` 每次调用返回新对象；如果没有 useMemo，点「设置」会冻结页面（桌面复现 CDP
 * 超时），用户看到的就是"点了没反应"。这里不注入 env，数 browserPushEnv 被调用几次。
 */
describe("前端构建号与硬刷新（agora-xu12）", () => {
  it("设置页底部显示构建号，并有重新加载（清缓存 + 换地址导航）", async () => {
    // iOS 主屏 PWA 会把 start_url 钉在缓存里、从任务切换器回来还可能只是"恢复旧页面"，
    // 升级后手机上跑的是哪一版光看行为猜不出来——构建号是唯一判据，重新加载是唯一的自救。
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => {}}
        env={env()}
        probe={async () => ({ apple: null, reason: null })}
      />,
    );
    const build = await screen.findByTestId("mobile-build");
    expect(build.textContent).toContain("前端");
    expect(screen.getByTestId("mobile-reload")).toBeTruthy();
  });
});

describe("布局诊断（agora-xu12）", () => {
  it("显示边框可开关，复制布局报告退到 textarea 时内容含行与可疑元素", async () => {
    // 手机是唯一跑真 Safari 的机器：现场取证靠这两颗按钮——描边看出框/被裁，报告给我盒子数字。
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => {}}
        env={env()}
        probe={async () => ({ apple: null, reason: null })}
      />,
    );
    await screen.findByTestId("mobile-push-state");
    const outline = screen.getByTestId("mobile-diag-outline");
    expect(outline.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(outline);
    expect(outline.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(outline);
    expect(outline.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(screen.getByTestId("mobile-diag-report"));
    // jsdom 没有 clipboard → copyText 返回 false → 一定退到 textarea（真机上多一步"已复制"提示）。
    const text = await screen.findByTestId("mobile-diag-text");
    const value = (text as HTMLTextAreaElement).value;
    expect(value).toContain('"safeArea"');
    expect(value).toContain('"rows"');
    expect(value).toContain('"suspects"');
    expect(value).toContain('"viewport"');
  });
});

describe("手机端字号档位（agora-x70t.xsgz）", () => {
  it("四档都在、当前档有 aria-pressed、点了立刻持久化并回调", async () => {
    // iOS Safari 不把系统"文字大小"暴露给网页，所以这个旋钮得自己给；默认 17px（Apple HIG body，
    // 也顺带让输入框不进 iOS 的"聚焦自动放大"区）。存 localStorage，刷新后还在。
    const picked: string[] = [];
    render(
      <MobileSettings
        onClose={() => {}}
        onRevoked={() => {}}
        env={env()}
        probe={async () => ({ apple: null, reason: null })}
        onTextSize={(s) => picked.push(s)}
      />,
    );
    await screen.findByTestId("mobile-push-state");
    for (const s of MOBILE_TEXT_SIZES) {
      const b = screen.getByTestId(`mobile-text-${s.key}`) as HTMLButtonElement;
      expect(b.textContent).toBe(s.label);
      expect(b.getAttribute("aria-pressed")).toBe(String(s.key === DEFAULT_MOBILE_TEXT_SIZE));
    }
    fireEvent.click(screen.getByTestId("mobile-text-l"));
    expect(picked).toEqual(["l"]);
    expect(localStorage.getItem(MOBILE_TEXT_STORAGE_KEY)).toBe("l");
    expect(screen.getByTestId("mobile-text-l").getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByTestId("mobile-text-m").getAttribute("aria-pressed")).toBe("false");
  });
});

describe("MobileSettings render stability", () => {
  it("calls browserPushEnv once, not on every render", async () => {
    const push = await import("./push");
    const spy = vi.spyOn(push, "browserPushEnv");
    try {
      render(
        <MobileSettings
          onClose={() => {}}
          onRevoked={() => {}}
          probe={async () => ({ apple: null, reason: null })}
        />,
      );
      // 等渲染与自查落定（若是死循环，这里的 microtask 会一直转、调用数会爆掉）。
      await screen.findByTestId("mobile-push-toggle");
      await new Promise((r) => setTimeout(r, 50));
      expect(spy.mock.calls.length).toBeLessThanOrEqual(2);
    } finally {
      spy.mockRestore();
    }
  });
});
