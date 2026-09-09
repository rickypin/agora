// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import type { SessionRow } from "./events";
import { RespondPanel } from "./RespondPanel";

afterEach(cleanup);

function setup(row: Partial<SessionRow>, status = 200, body: unknown = {}) {
  const requests: { url: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    requests.push({ url, body: init.body as string | undefined });
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  };
  const onOpenTerminal = vi.fn();
  const full: SessionRow = { id: "mac:s1", node: "mac", status: "waiting", alive: true, pending_decision: { request_id: "request-1", summary: "Bash", epoch: 1 }, ...row };
  render(<RespondPanel row={full} api={sessionApi(f)} onOpenTerminal={onOpenTerminal} />);
  return { requests, onOpenTerminal };
}

it("permission via hook: allow goes to the input endpoint, no terminal involved", async () => {
  // A15：不打开终端回答 PermissionRequest。
  const { requests, onOpenTerminal } = setup({
    reason: "permission",
    respond_via: "hook",
    detail: "Bash",
  });
  expect(screen.getByText("Bash")).toBeTruthy();
  fireEvent.click(screen.getByTestId("allow"));
  await waitFor(() => expect(requests.length).toBe(1));
  expect(requests[0].url).toBe("/api/sessions/mac%3As1/input");
  expect(JSON.parse(requests[0].body!)).toEqual({ kind: "decision", decision: "allow", request_id: "request-1" });
  expect(onOpenTerminal).not.toHaveBeenCalled();
});

it("a question or a host that cannot decide via hook only offers the terminal", () => {
  // ADR-002 D5：AskUserQuestion 的选项在 TUI 里；Grok 的 allow 不能替用户批准。
  setup({ reason: "question", respond_via: "hook", detail: "which?" });
  expect(screen.getByText("which?")).toBeTruthy();
  expect(screen.queryByTestId("allow")).toBeNull();
  cleanup();
  const { onOpenTerminal } = setup({ reason: "permission", respond_via: "terminal", detail: "Bash" });
  expect(screen.queryByTestId("allow")).toBeNull();
  fireEvent.click(screen.getByTestId("open-terminal"));
  expect(onOpenTerminal).toHaveBeenCalledWith("mac:s1");
});

it("a short hold (Codex) says how long the dashboard has before the terminal takes over", () => {
  // ADR-002 附录 A（Codex 0.152.1 实测）：挂起期间 TUI 不弹审批提示，上限只有几十秒。
  setup({ reason: "permission", respond_via: "hook", respond_within_secs: 20, detail: "Bash" });
  expect(screen.getByTestId("respond-within").textContent).toContain("20 秒");
  expect(screen.getByTestId("allow")).toBeTruthy();
  cleanup();
  // Claude 的 55 min 并存模型：不啰嗦。
  setup({ reason: "permission", respond_via: "hook", respond_within_secs: 3300, detail: "Bash" });
  expect(screen.queryByTestId("respond-within")).toBeNull();
});

it("terminal answered first: no_pending_decision is shown, not thrown", async () => {
  const { requests } = setup({ reason: "permission", respond_via: "hook" }, 409, {
    error: "no_pending_decision",
    message: "x",
  });
  fireEvent.click(screen.getByTestId("deny"));
  await waitFor(() => expect(requests.length).toBe(1));
  await waitFor(() => expect(screen.getByText(/已在终端回答/)).toBeTruthy());
});

it("turn_done: the next instruction is sent as text with a trailing newline", async () => {
  const { requests } = setup({ status: "turn_done", detail: "Two files." });
  expect(screen.getByText(/Two files\./)).toBeTruthy();
  const input = screen.getByTestId("next-input") as HTMLInputElement;
  expect((screen.getByTestId("next-send") as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(input, { target: { value: "now delete them" } });
  fireEvent.click(screen.getByTestId("next-send"));
  await waitFor(() => expect(requests.length).toBe(1));
  expect(JSON.parse(requests[0].body!)).toEqual({ kind: "text", data: "now delete them\n" });
  await waitFor(() => expect(input.value).toBe(""));
});

it("running rows have nothing to answer", () => {
  setup({ status: "running" });
  expect(screen.queryByTestId("respond-panel-mac:s1")).toBeNull();
});


it("binds the visible summary to its request id when parallel requests change", async () => {
  const input = vi.fn().mockResolvedValue({ ok: true, value: {} });
  const api = { ...sessionApi(), input };
  const row: SessionRow = { id: "mac:s1", node: "mac", status: "waiting", alive: true, reason: "permission", respond_via: "hook", detail: "old detail", pending_decision: { request_id: "write-2", summary: "Write file", epoch: 1 } };
  const { rerender } = render(<RespondPanel row={row} api={api} onOpenTerminal={vi.fn()} />);
  expect(screen.getByText("Write file")).toBeTruthy();
  fireEvent.click(screen.getByTestId("allow"));
  await waitFor(() => expect(input).toHaveBeenCalledWith("mac:s1", { kind: "decision", decision: "allow", request_id: "write-2" }));
  await waitFor(() => expect((screen.getByTestId("allow") as HTMLButtonElement).disabled).toBe(false));
  rerender(<RespondPanel row={{ ...row, pending_decision: { request_id: "bash-1", summary: "Bash command", epoch: 1 } }} api={api} onOpenTerminal={vi.fn()} />);
  expect(screen.getByText("Bash command")).toBeTruthy();
  fireEvent.click(screen.getByTestId("deny"));
  await waitFor(() => expect(input).toHaveBeenLastCalledWith("mac:s1", { kind: "decision", decision: "deny", request_id: "bash-1" }));
});

it("a restored or expired permission has no live approval button", () => {
  setup({ reason: "permission", respond_via: "hook", detail: "Write", pending_decision: null });
  expect(screen.getByText("Write")).toBeTruthy();
  expect(screen.queryByTestId("allow")).toBeNull();
  expect(screen.getByTestId("open-terminal")).toBeTruthy();
});

it("the next-command input renders above the last reply (A50)", () => {
  // agora-03k：几百行的回复原样塞在窄侧栏里、输入框在全文最底下，人要先滚到底才能给下一条指令。
  // 搬进主区之后输入框排在最上面（用户明确要求），回复排在它下面、由 .respond-last 限高滚动。
  // 「限高」是布局事实（jsdom 不做布局），归代检；这里钉的是 DOM 顺序。
  setup({ status: "turn_done", detail: "结论先说：可以做局部性能优化" });
  const input = screen.getByTestId("next-input");
  const last = screen.getByTestId("respond-last");
  expect(input.compareDocumentPosition(last) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(last.textContent).toContain("结论先说");
});

it("the panel renders nothing for running / finished rows", () => {
  // 主区里"不占位"比在侧栏里更要紧：面板挤在 crumb 与终端之间，没有可回答的东西就一格都不该占。
  for (const status of ["running", "finished", "failed", "idle", "starting", "unknown"]) {
    setup({ status });
    expect(screen.queryByTestId("respond-panel-mac:s1"), `status=${status} 时面板不该占位`).toBeNull();
    cleanup();
  }
});

it("Escape in the input returns focus to the terminal callback", () => {
  // 终端焦点规则（agora-p29 / agora-vcc）不变：面板抢到焦点之后，Escape 是把键盘还给终端的那条路。
  const { onOpenTerminal } = setup({ status: "turn_done", detail: "done" });
  fireEvent.keyDown(screen.getByTestId("next-input"), { key: "Escape" });
  expect(onOpenTerminal).toHaveBeenCalledWith("mac:s1");
});

it("an external row has no terminal to hand the focus to, so it offers no such button", () => {
  // MISSION §5.5：external 会话没有运行时句柄，主区那一格是"没有终端"的说明文字。
  // 经 hook 的回答照旧（Allow / Deny 在），"打开终端"不画。
  setup({ origin: "external", reason: "permission", respond_via: "hook", detail: "Bash" });
  expect(screen.getByTestId("allow")).toBeTruthy();
  expect(screen.queryByTestId("open-terminal")).toBeNull();
});
