// @vitest-environment jsdom
/**
 * 设备管理与配对新设备（agora-thc.1）：列表、吊销（两步确认）、QR。
 *
 * 前端唯一的 fetch 在 net.ts，这里 mock `./net` 的 apiFetch（与 App.test 同一纪律）。
 */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DeviceDialog } from "./DeviceDialog";

const net = vi.hoisted(() => ({
  deletes: [] as string[],
  devices: [] as Array<Record<string, string | null>>,
}));

vi.mock("./net", () => ({
  API_PREFIX: "/api/",
  apiFetch: async (path: string, init: RequestInit = {}): Promise<Response> => {
    const json = (status: number, body: unknown): Response =>
      ({ ok: status >= 200 && status < 300, status, json: async () => body }) as unknown as Response;
    if (path === "/api/auth/devices") return json(200, net.devices);
    if (path.startsWith("/api/auth/devices/") && init.method === "DELETE") {
      const id = decodeURIComponent(path.split("/").pop()!);
      net.deletes.push(id);
      net.devices = net.devices.filter((d) => d.id !== id);
      return json(204, null);
    }
    if (path === "/api/auth/pair/new" && init.method === "POST") {
      return json(200, {
        url: "https://zuan.example:7681/#pair=tok",
        qr: { size: 2, rows: ["10", "01"] },
      });
    }
    return json(404, {});
  },
}));

describe("DeviceDialog", () => {
  beforeEach(() => {
    net.deletes.length = 0;
    net.devices = [
      {
        id: "d1",
        name: "iPhone",
        paired_via: "session",
        created_at: "2026-09-22T00:00:00Z",
        last_seen_at: "2026-09-22T01:00:00Z",
        revoked_at: null,
      },
    ];
  });
  afterEach(cleanup);

  it("lists devices, mints a QR pair link, and revokes with confirmation", async () => {
    render(<DeviceDialog onClose={() => {}} />);
    expect(await screen.findByText("iPhone")).toBeTruthy();

    fireEvent.click(screen.getByTestId("pair-new"));
    expect(await screen.findByTestId("pair-qr")).toBeTruthy();
    expect(screen.getByTestId("pair-url").textContent).toContain("#pair=tok");

    fireEvent.click(screen.getByTestId("revoke-d1"));
    // 两步：先弹确认框，确认后才真的 DELETE。
    expect(screen.getByText("吊销这台设备？")).toBeTruthy();
    expect(net.deletes).toEqual([]);
    fireEvent.click(screen.getByText("吊销", { selector: ".dialog-actions .danger" }));
    await waitFor(() => expect(net.deletes).toEqual(["d1"]));
    // 刷新后的列表不再有这行（吊销即时生效）。
    await waitFor(() => expect(screen.queryByText("iPhone")).toBeNull());
  });
});
