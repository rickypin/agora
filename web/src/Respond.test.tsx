// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import type { SessionRow } from "./events";
import { Respond } from "./Respond";

afterEach(cleanup);

function setup(row: Partial<SessionRow>, status = 200, body: unknown = {}) {
  const requests: { url: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    requests.push({ url, body: init.body as string | undefined });
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  };
  const onOpenTerminal = vi.fn();
  const full: SessionRow = { id: "mac:s1", node: "mac", status: "waiting", alive: true, ...row };
  render(<Respond row={full} api={sessionApi(f)} onOpenTerminal={onOpenTerminal} />);
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
  expect(JSON.parse(requests[0].body!)).toEqual({ kind: "decision", decision: "allow" });
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
  expect(screen.queryByTestId("respond-mac:s1")).toBeNull();
});
