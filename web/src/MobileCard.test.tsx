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
  it("explains an empty thread instead of leaving the card blank", () => {
    // 现场（2026-10-07，agora-sd0b）：pi 的 external 行 reload 后只有登记——没有 ❯ 也没有 ↳，
    // 手机打开就是一张空卡。无句柄行的说明现在由 mobile-terminal-only 统一承担
    // （agora-71p2：空 thread、有内容、在跑都用这一条），有句柄的才是 mobile-empty-thread。
    setup(row("zuan:aa2462", { agent_type: "pi", origin: "external", status: "idle" }));
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("桌面终端");
    expect(screen.queryByTestId("mobile-bubble-user")).toBeNull();
    expect(screen.queryByTestId("mobile-bubble-agent")).toBeNull();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    cleanup();

    // 有句柄的行同样空 thread（idle）：说"回一条就会出现在这里"，不让它误以为要去桌面。
    setup(row("n:b", { status: "idle" }));
    const handled = screen.getByTestId("mobile-empty-thread").textContent ?? "";
    expect(handled).not.toContain("桌面终端");
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    expect(screen.getByTestId("mobile-next-input")).toBeTruthy();
  });

  it("an external row with content still says where to reply", () => {
    // MISSION §5.5：external 没有终端与文本输入。以前只有空 thread 才说话（agora-sd0b），
    // 有内容时看着像"输入框没画出来"（agora-71p2）。
    setup(row("zuan:x", { agent_type: "pi", origin: "external", status: "turn_done", prompt: "问", detail: "答" }));
    expect(screen.getByTestId("mobile-bubble-agent").textContent).toContain("答");
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("桌面终端");
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();

    // 在跑也一样说清（且不再说"等它停下来再发"——手机永远发不了）。
    cleanup();
    setup(row("zuan:y", { agent_type: "pi", origin: "external", status: "running", detail: "bash" }));
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("在跑");
    expect(screen.queryByTestId("mobile-running-note")).toBeNull();
  });

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

  it("host text channel: a handleless pi row can be answered from the phone", async () => {
    // ADR-002 D11 / agora-t5kf.3：无句柄但宿主收文本（pi 的扩展自报 input_channel → text_via=host）
    // → 手机能发，走 daemon 的 input/ 队列；只有 text_via=none 的才说"到桌面"。
    const { requests } = setup(
      row("zuan:aa2462", { agent_type: "pi", origin: "external", status: "turn_done", text_via: "host", prompt: "问", detail: "答" }),
    );
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "从手机发一条" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "从手机发一条\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发送");
  });

  it("host + running: the composer stays open and the note says queued until this run ends", () => {
    // agora-shze：pi 的扩展在会话非空闲时走 `deliverAs: "followUp"`，语义是**等当前这一轮跑完
    // （不再有工具调用）再交进去**，不是"做完当前工具批次"（那是 pi 的 `steer`）。文案必须与这个
    // 事实一致：说"跑完这一轮"；issue 里原本要求的"这一步"是 steer 的时机，写上去就是假话
    // （ADR-002 D11 的实测记录：两次真 TUI 实测 + pi 1.0.4 自己的 SDK 注释）。
    setup(row("zuan:bb", { agent_type: "pi", origin: "external", status: "running", text_via: "host", detail: "bash" }));
    expect(screen.getByTestId("mobile-next-input")).toBeTruthy();
    const note = screen.getByTestId("mobile-host-running-note").textContent ?? "";
    expect(note).toContain("排队");
    expect(note).toContain("跑完这一轮");
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    expect(screen.queryByTestId("mobile-running-note")).toBeNull();
  });

  it("runtime + running keeps the old gate: no composer, wait for it to stop", () => {
    // runtime（PTY）没有队列：文本直接落进 TUI 的输入区，各家对"跑着的时候打字"解释不同
    // （Claude 排队、别的可能当快捷键），不值得赌——门与文案都不动（agora-shze，issue 的 ①）。
    setup(row("n:rt", { status: "running", runtime_ref: "tmux:agora:n-rt", command: "claude", detail: "在跑" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.getByTestId("mobile-running-note").textContent).toContain("等它停下来");
    expect(screen.queryByTestId("mobile-host-running-note")).toBeNull();
  });

  it("host + running: sending still lands on 已发送", async () => {
    // 发送链路不变：POST /api/sessions/:id/input（daemon 对 host 通道 200 + 排队），
    // 卡片与小节里 host-idle 路径同一套乐观态（agora-shze）。
    const { requests } = setup(
      row("zuan:bb", { agent_type: "pi", origin: "external", status: "running", text_via: "host", detail: "bash" }),
    );
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "做完这个之后顺手跑一下测试" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("发送中…");
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "做完这个之后顺手跑一下测试\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发送");
  });

  it("聚焦 composer 时把输入框滚进可见区（iOS 键盘不遮它）", () => {
    // agora-x70t.ten0：Safari 不认 interactive-widget，粘性底部的 composer 只能主动滚一下；
    // 等 300ms 是让键盘动画先走起来（jsdom 里 scrollIntoView 未必存在，代码必须可选调用）。
    vi.useFakeTimers();
    const calls: ScrollIntoViewOptions[] = [];
    const proto = Element.prototype as unknown as { scrollIntoView?: (o?: ScrollIntoViewOptions) => void };
    const orig = proto.scrollIntoView;
    proto.scrollIntoView = (o) => calls.push(o ?? {});
    try {
      setup(row("n:a"));
      fireEvent.focus(screen.getByTestId("mobile-next-input"));
      expect(calls).toEqual([]);
      act(() => vi.advanceTimersByTime(400));
      expect(calls).toEqual([{ block: "nearest" }]);
    } finally {
      if (orig) proto.scrollIntoView = orig;
      else delete proto.scrollIntoView;
      vi.useRealTimers();
    }
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
    expect(onSeen).toHaveBeenCalledWith(seenKey({ id: "n:a", status: "turn_done", status_since: 500 }));
  });
});
