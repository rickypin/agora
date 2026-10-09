// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { DesktopOverview } from "./DesktopOverview";
import { ATTENTION_WINDOW_SECS, seenKey } from "./attention";
import type { SessionRow } from "./events";

const row = (id: string, status: string, extra: Partial<SessionRow> = {}): SessionRow => ({ id, node: "mac", status, alive: true, display_name: id, origin: "agora", status_since: 1900000000, ...extra });
afterEach(() => { cleanup(); vi.useRealTimers(); });
it("概览复用 attention/已读判据，点击只交给唯一的会话选择回调", () => {
  const done = row("done", "turn_done");
  const rows = [row("running", "running"), row("wait", "waiting"), done, row("headless", "waiting", { origin: "headless" }), row("unknown", "unknown")];
  const onOpen = vi.fn();
  render(<DesktopOverview rows={rows} seen={new Set([seenKey(done)])} serverClock={null} onOpen={onOpen} onNewAgent={() => {}} onPalette={() => {}} />);
  expect(screen.getByRole("heading", { name: "1 个会话需要你" })).toBeTruthy();
  expect(screen.queryByTestId("overview-done")).toBeNull();
  expect(screen.queryByTestId("overview-headless")).toBeNull();
  expect(screen.queryByTestId("overview-running")).toBeNull();
  fireEvent.click(screen.getByTestId("overview-wait"));
  expect(onOpen).toHaveBeenCalledExactlyOnceWith("wait");
});
it("无会话时提供创建与命令入口，不卡在无操作的空白页", () => {
  const create = vi.fn(), palette = vi.fn();
  render(<DesktopOverview rows={[]} seen={new Set()} serverClock={null} onOpen={() => {}} onNewAgent={create} onPalette={palette} />);
  fireEvent.click(screen.getByRole("button", { name: "+ 新建会话" }));
  fireEvent.click(screen.getByRole("button", { name: /搜索与命令/ }));
  expect(create).toHaveBeenCalledOnce(); expect(palette).toHaveBeenCalledOnce();
});
it("首页的完成新鲜度随节点时钟推进，不依赖另一次事件刷新", () => {
  vi.useFakeTimers(); vi.setSystemTime(1900000000000);
  const r = row("old-result", "turn_done", { status_since: 1900000000 - ATTENTION_WINDOW_SECS + 10 });
  render(<DesktopOverview rows={[r]} seen={new Set()} serverClock={{ now: 1900000000, at: 1900000000 }} onOpen={() => {}} onNewAgent={() => {}} onPalette={() => {}} />);
  expect(screen.getByTestId("overview-old-result")).toBeTruthy();
  act(() => { vi.advanceTimersByTime(30000); });
  expect(screen.queryByTestId("overview-old-result")).toBeNull();
});
