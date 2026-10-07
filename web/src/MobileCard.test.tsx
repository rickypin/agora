// @vitest-environment jsdom
/**
 * /m 会话卡（agora-thc.10）：IM 语法（两气泡 / 底部 composer / 乐观发送）、composer 状态门、
 * WAITING 三种分支、Kill / Restart 的两步确认、A52 的 DOM 预算。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import type { SessionRow } from "./events";
import { FOLD_LINES, MobileCard } from "./MobileCard";
import { seenKey } from "./attention";

function row(id: string, patch: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    node: "zuan",
    status: "turn_done",
    alive: true,
    display_name: id,
    agent_type: "claude",
    reason: null,
    respond_via: "hook",
    ...patch,
  };
}

function setup(r: SessionRow, killRequiresConfirm = true, onSeen = vi.fn()) {
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    if (url.endsWith("/kill") && killRequiresConfirm && init.body === "{}") {
      return json({ error: "needs_confirmation", message: "会杀掉运行中的进程" }, 409);
    }
    return json({});
  };
  const ui = render(<MobileCard row={r} api={sessionApi(f)} now={1000} onBack={vi.fn()} onSeen={onSeen} />);
  return { ui, requests, onSeen };
}

afterEach(cleanup);

describe("thread", () => {
  it("renders the last turn as two bubbles and folds the reply to 6 lines with one expand", () => {
    const reply = Array.from({ length: 10 }, (_, i) => `第 ${i + 1} 行`).join("\n");
    setup(row("n:a", { prompt: "加一个菜单", detail: reply }));

    expect(screen.getByTestId("mobile-bubble-user").textContent).toContain("加一个菜单");
    const agent = screen.getByTestId("mobile-bubble-agent");
    expect(agent.textContent).toContain(`第 ${FOLD_LINES} 行`);
    expect(agent.textContent).not.toContain(`第 ${FOLD_LINES + 1} 行`);

    fireEvent.click(screen.getByTestId("mobile-expand"));
    expect(screen.getByTestId("mobile-bubble-agent").textContent).toContain("第 10 行");
    expect(screen.queryByTestId("mobile-expand")).toBeNull();
    // 只允许展开最后一条：没有更早消息与翻页的出口。
    expect(screen.queryByTestId(/mobile-history|mobile-stream|mobile-load-more/)).toBeNull();
  });
});

describe("composer", () => {
  it("sends optimistically and settles to 已发送", async () => {
    const { requests } = setup(row("n:a"));
    const input = screen.getByTestId("mobile-next-input");
    fireEvent.change(input, { target: { value: "把菜单加到侧栏" } });
    fireEvent.click(screen.getByTestId("mobile-send"));

    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("发送中…");
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "把菜单加到侧栏\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发送");
  });

  it("gates on status: running is disabled with a note, handleless has no composer", () => {
    setup(row("n:a", { status: "running", detail: "在跑" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.getByTestId("mobile-running-note")).toBeTruthy();
    cleanup();

    setup(row("n:b", { status: "turn_done", origin: "external" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
  });
});

describe("waiting", () => {
  it("decides via the hook with the pending request_id", async () => {
    const r = row("n:a", {
      status: "waiting",
      reason: "permission",
      respond_via: "hook",
      pending_decision: { request_id: "req-1", summary: "Bash: git push origin main", epoch: 1 },
    });
    const { requests } = setup(r);
    expect(screen.getByTestId("mobile-decision-text").textContent).toBe("Bash: git push origin main");
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();

    fireEvent.click(screen.getByTestId("mobile-allow"));
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "decision", decision: "allow", request_id: "req-1" }));
  });

  it("shows 需要到桌面 for terminal-only decisions instead of a composer", () => {
    setup(row("n:a", { status: "waiting", reason: "permission", respond_via: "terminal", detail: "Bash: rm -rf build" }));
    expect(screen.getByTestId("mobile-decision-text").textContent).toBe("Bash: rm -rf build");
    expect(screen.getByTestId("mobile-needs-desktop")).toBeTruthy();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.queryByTestId("mobile-allow")).toBeNull();
  });
});

describe("kill / restart", () => {
  it("confirms through the node's 409 before sending confirmed: true", async () => {
    const { requests } = setup(row("n:a", { status: "running", command: "claude" }));
    fireEvent.click(screen.getByTestId("mobile-more"));
    fireEvent.click(screen.getByTestId("mobile-kill"));
    await act(async () => {});
    expect(requests[0].body).toBe("{}");

    const dialogs = screen.getAllByRole("dialog");
    expect(dialogs.length).toBe(1);
    fireEvent.click(screen.getByText("Kill", { selector: ".dialog-actions button" }));
    await act(async () => {});
    expect(requests[1].body).toBe(JSON.stringify({ confirmed: true }));
  });

  it("hides actions for handleless rows", () => {
    setup(row("n:a", { origin: "headless" }));
    expect(screen.queryByTestId("mobile-more")).toBeNull();
  });
});

describe("information budget (A52)", () => {
  it("never renders terminal / creation / diff / history surfaces", () => {
    const { ui } = setup(row("n:a", { prompt: "q", detail: "a" }));
    const forbidden = [
      '[data-testid^="term"]',
      '[data-testid="create"]',
      '[data-testid^="diff-"]',
      '[data-testid^="acceptance-"]',
      '[data-testid^="changes-"]',
      '[data-testid^="respond-"]',
      '[data-testid^="mobile-stream"]',
      '[data-testid="mobile-load-more"]',
      ".xterm",
      ".terminal",
      ".mobile-stream",
      ".mobile-history",
    ];
    for (const selector of forbidden) expect(ui.container.querySelectorAll(selector), selector).toHaveLength(0);
  });

  it("marks a turn_done row as seen when the card opens", () => {
    const onSeen = vi.fn();
    setup(row("n:a", { status: "turn_done", status_since: 500 }), true, onSeen);
    expect(onSeen).toHaveBeenCalledWith(seenKey({ id: "n:a", status_since: 500 }));
  });
});
