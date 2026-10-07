// @vitest-environment jsdom
/**
 * /m 手机收件箱（agora-thc.5；MISSION §6.9；A37 / A52）。
 *
 * 钉住的四件事：四段与 attention 同源（headless 收进已完成、不虚增「在跑」）、A52 的 DOM 预算
 * （没有终端 / 创建 / diff / 验收 / 改动列表 / 消息流与历史翻页）、推送深链定位（落在收起的
 * 已完成区也要先展开再选中）、行上的时长与摘要口径。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionRow, SocketLike } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { MobileApp, mobileSummary } from "./MobileApp";
import { SessionStore } from "./store";

class FakeSocket implements SocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  close(): void {}
}

function row(id: string, patch: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    node: "zuan",
    status: "running",
    alive: true,
    display_name: id,
    agent_type: "claude",
    reason: null,
    respond_via: "hook",
    ...patch,
  };
}

function setup(rows: SessionRow[]) {
  const sock = new FakeSocket();
  const store = new SessionStore({
    connect: () => sock,
    fetchSnapshot: async () => ({ sessions: rows, unregistered: [] }),
    coalesceMs: 0,
  });
  const health = new HealthWatcher({ fetchHealth: async () => ({ status: "ok" }) });
  const version = new VersionWatcher({ fetchSystem: async () => ({ node: "zuan", api_version: { major: 1, minor: 9 } }) });
  const ui = render(<MobileApp store={store} health={health} version={version} now={1000} />);
  return { ui, store, sock };
}

/** WS 连上：EventsClient 在 onopen 里拉全量，VersionWatcher 也在那一刻比版本。 */
async function online(t: ReturnType<typeof setup>) {
  await act(async () => {
    t.sock.onopen?.({});
  });
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  window.history.replaceState(null, "", "/");
});

describe("mobile inbox sections", () => {
  it("keeps attention's four-section order and never counts headless as working", async () => {
    const t = setup([
      row("n:a", { status: "waiting", status_since: 820 }),
      row("n:b", { status: "unknown" }),
      row("n:c", { status: "running" }),
      row("n:d", { status: "running", origin: "headless" }),
      row("n:e", { status: "finished", origin: "external" }),
    ]);
    await online(t);

    const order = [...t.ui.container.querySelectorAll('[data-testid^="mobile-section-"]')].map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(order).toEqual([
      "mobile-section-attention",
      "mobile-section-unclear",
      "mobile-section-working",
      "mobile-section-finished",
    ]);
    expect(screen.getByTestId("mobile-section-attention").textContent).toContain("n:a");
    expect(screen.getByTestId("mobile-section-unclear").textContent).toContain("n:b");
    expect(screen.getByTestId("mobile-section-working").textContent).toContain("n:c");
    // headless 在跑的行收进已完成（与桌面 finishedCollapsed 同源），不虚增「在跑」。
    expect(screen.getByTestId("mobile-section-working").textContent).not.toContain("n:d");
    expect(screen.getByTestId("mobile-section-finished").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("mobile-finished-toggle"));
    expect(screen.getByTestId("mobile-row-n:d")).toBeTruthy();
    expect(screen.getByTestId("mobile-row-n:e")).toBeTruthy();
  });

  it("shows duration, the peer ≥ bound, agent badge and node chips", async () => {
    const t = setup([
      row("n:a", { status: "waiting", status_since: 820 }),
      row("n:p", { node: "mac", status: "waiting", status_since: 820, stale: false }),
    ]);
    await online(t);

    const local = screen.getByTestId("mobile-row-n:a");
    const peer = screen.getByTestId("mobile-row-n:p");
    expect(local.textContent).toContain("waiting 3m");
    expect(peer.textContent).toContain("waiting ≥3m");
    expect(local.textContent).toContain("Claude");
    expect(screen.getByTestId("mobile-node-n:a").className).toContain("local");
    const peerChip = screen.getByTestId("mobile-node-n:p");
    expect(peerChip.className).toContain("peer");
    expect(peerChip.getAttribute("data-node")).toBe("mac");
  });

  it("locates a deep-linked row by opening its card", async () => {
    window.history.replaceState(null, "", "/m?session=zuan:n:e");
    const t = setup([row("zuan:n:a", { status: "waiting" }), row("zuan:n:e", { status: "finished", origin: "external" })]);
    await online(t);

    expect(screen.getByTestId("mobile-card-zuan:n:e")).toBeTruthy();
    fireEvent.click(screen.getByTestId("mobile-back"));
    // 回到收件箱：这一行是 external finished，在收起的已完成区里；展开后能看到它。
    fireEvent.click(screen.getByTestId("mobile-finished-toggle"));
    expect(screen.getByTestId("mobile-row-zuan:n:e")).toBeTruthy();
  });

  it("opens the card from a row tap and goes back to the inbox", async () => {
    const t = setup([row("n:a", { status: "waiting" })]);
    await online(t);

    fireEvent.click(screen.getByTestId("mobile-row-n:a"));
    expect(screen.getByTestId("mobile-card-n:a")).toBeTruthy();
    fireEvent.click(screen.getByTestId("mobile-back"));
    expect(screen.getByTestId("mobile-inbox").textContent).toContain("需要我");
  });
});

describe("mobile information budget (A52)", () => {
  it("never renders desktop or message-stream surfaces", async () => {
    const t = setup([row("n:a", { status: "waiting" })]);
    await online(t);

    const forbidden = [
      '[data-testid^="term"]',
      '[data-testid="create"]',
      '[data-testid^="diff-"]',
      '[data-testid^="acceptance-"]',
      '[data-testid^="changes-"]',
      '[data-testid^="respond-"]',
      '[data-testid^="new-agent"]',
      '[data-testid^="mobile-stream"]',
      '[data-testid="mobile-load-more"]',
      ".xterm",
      ".terminal",
      ".mobile-stream",
      ".mobile-history",
    ];
    for (const selector of forbidden) {
      expect(t.ui.container.querySelectorAll(selector), selector).toHaveLength(0);
    }
  });

  it("mobileSummary takes the first non-empty line and caps at 80 chars", () => {
    expect(mobileSummary(row("x", { progress: "\n第一行\n第二行" }))).toBe("第一行");
    expect(mobileSummary(row("x", { reason: "运行时会话没了" }))).toBe("运行时会话没了");
    const s = mobileSummary(row("x", { detail: "字".repeat(120) }));
    expect(s.endsWith("…")).toBe(true);
    expect(s.length).toBe(80);
  });
});
