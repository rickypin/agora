// @vitest-environment jsdom
/**
 * 键盘优先的行为守卫（agora-xqa.14；键位表 docs/spec/ux.md）。
 *
 * 三条硬的：① 终端聚焦时 Ctrl+C/D/Z/R/A/E 一路到底，全局层连 preventDefault 都不许调；
 * ② Alt/Option+N 跳的必须是**过滤后的显示顺序**；③ 命令面板既能切会话也能起会话。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import type { SessionRow, SocketLike } from "./events";
import { TERMINAL_CTRL_KEYS } from "./keys";
import { SessionStore } from "./store";
import { Workspace } from "./Workspace";

const mounted: string[] = [];
vi.mock("./TerminalView", async () => {
  const React = await import("react");
  return {
    TerminalView: ({ sessionId }: { sessionId: string }) => {
      React.useEffect(() => {
        mounted.push(sessionId);
      }, [sessionId]);
      // 终端本体在 jsdom 里开不了；这里只要一个能拿到焦点的落点。
      return React.createElement("textarea", { "data-testid": `term-${sessionId}` });
    },
  };
});

class FakeSocket implements SocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  close(): void {}
  send(events: unknown[]): void {
    this.onmessage?.({ data: JSON.stringify(events) });
  }
}

function row(id: string, over: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    node: "mac",
    status: "running",
    alive: true,
    display_name: id.slice(4),
    agent_type: "claude",
    reason: null,
    ...over,
  };
}

function setup(rows: SessionRow[]) {
  const sock = new FakeSocket();
  const store = new SessionStore({
    connect: () => sock,
    fetchSnapshot: async () => ({ sessions: rows, unregistered: [] }),
    coalesceMs: 0,
  });
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    if (url.startsWith("/api/projects/worktrees")) return json({ worktrees: [] });
    if (url.startsWith("/api/projects"))
      return json({
        projects: [
          { path: "/code/agora", name: "agora", last_used_at: null },
          { path: "/code/sglog", name: "sglog", last_used_at: null },
        ],
      });
    if (url.startsWith("/api/agents"))
      return json({ agents: [{ name: "claude", command: "claude" }, { name: "codex", command: "codex" }] });
    if (url.startsWith("/api/system")) return json({ node: "mac" });
    if (url === "/api/sessions" && method === "POST") return json({ id: "mac:new" }, 201);
    return json({});
  };
  const ui = render(<Workspace store={store} api={sessionApi(f)} catalog={catalogApi(f)} />);
  return { ui, store, sock, requests };
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function online(t: ReturnType<typeof setup>) {
  await act(async () => {
    t.sock.onopen?.({});
  });
  await flush();
}

/** 在 window 上发一个 keydown，返回它有没有被 preventDefault。 */
function press(init: KeyboardEventInit & { key: string }, target: EventTarget = window): boolean {
  const ev = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  act(() => {
    target.dispatchEvent(ev);
  });
  return ev.defaultPrevented;
}

afterEach(() => {
  cleanup();
  mounted.length = 0;
});

describe("不吞终端的 Ctrl 键（MISSION §6.5 硬约束）", () => {
  it.each([...TERMINAL_CTRL_KEYS])("终端聚焦时 Ctrl+%s 不被全局层碰一下", async (k) => {
    const t = setup([row("mac:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-mac:a"));
    const term = screen.getByTestId("term-mac:a");
    term.focus();
    expect(press({ key: k, ctrlKey: true }, term)).toBe(false);
    // 也不能顺手干别的：Tab 没变、没发过任何写请求。
    expect(screen.getByTestId("tab-mac:a")).toBeTruthy();
    await flush();
    expect(t.requests.filter((r) => r.method !== "GET")).toEqual([]);
  });
});

describe("侧栏过滤与序号跳转", () => {
  it("Cmd+F 聚焦过滤框，Enter 打开第一条", async () => {
    const t = setup([row("mac:a"), row("mac:b", { agent_type: "codex" })]);
    await online(t);
    expect(press({ key: "f", metaKey: true })).toBe(true);
    const box = screen.getByTestId("sidebar-filter") as HTMLInputElement;
    expect(document.activeElement).toBe(box);
    fireEvent.change(box, { target: { value: "codex" } });
    expect(screen.queryByTestId("row-mac:a")).toBeNull(); // 不匹配的藏起来
    fireEvent.keyDown(box, { key: "Enter" });
    expect(mounted).toEqual(["mac:b"]);
  });

  it("Alt/Option+N 跳的是过滤后的顺序，不是原始顺序", async () => {
    const t = setup([row("mac:a"), row("mac:b"), row("mac:c")]);
    await online(t);
    // 不过滤：1 = a。
    expect(press({ key: "1", code: "Digit1", altKey: true })).toBe(true);
    expect(mounted).toEqual(["mac:a"]);
    // 过滤掉 a 之后，1 必须是 b——照原始顺序的实现这里会开 a，红。
    fireEvent.change(screen.getByTestId("sidebar-filter"), { target: { value: "b" } });
    press({ key: "1", code: "Digit1", altKey: true });
    expect(mounted).toEqual(["mac:a", "mac:b"]);
    // 过滤后只剩一条：2 无处可跳，什么都不该发生。
    press({ key: "2", code: "Digit2", altKey: true });
    expect(mounted).toEqual(["mac:a", "mac:b"]);
  });

  it("Cmd+Shift+] / [ 在显示顺序里前后走，走到头绕回来", async () => {
    const t = setup([row("mac:a"), row("mac:b"), row("mac:c")]);
    await online(t);
    press({ key: "1", code: "Digit1", altKey: true });
    expect(press({ key: "}", code: "BracketRight", metaKey: true, shiftKey: true })).toBe(true);
    press({ key: "}", code: "BracketRight", metaKey: true, shiftKey: true });
    expect(mounted).toEqual(["mac:a", "mac:b", "mac:c"]);
    press({ key: "}", code: "BracketRight", metaKey: true, shiftKey: true }); // 绕回第一条
    press({ key: "{", code: "BracketLeft", metaKey: true, shiftKey: true }); // 往回 = 最后一条
    expect(mounted).toEqual(["mac:a", "mac:b", "mac:c", "mac:a", "mac:c"]);
  });
});

describe("Command Palette", () => {
  it("Cmd+K 打开，fuzzy 选中会话就切过去", async () => {
    const t = setup([row("mac:alpha"), row("mac:beta")]);
    await online(t);
    press({ key: "k", metaKey: true });
    await flush();
    const box = screen.getByTestId("palette-input");
    fireEvent.change(box, { target: { value: "beta" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await flush();
    expect(mounted).toEqual(["mac:beta"]);
    expect(screen.queryByTestId("palette-input")).toBeNull(); // 选完就关
  });

  it("`New <agent> in <project>` 直接起会话，字段与对话框同一个端点", async () => {
    const t = setup([]);
    await online(t);
    press({ key: "k", metaKey: true });
    await flush();
    fireEvent.change(screen.getByTestId("palette-input"), { target: { value: "new codex sglog" } });
    fireEvent.keyDown(screen.getByTestId("palette-input"), { key: "Enter" });
    await flush();
    const post = t.requests.find((r) => r.method === "POST" && r.url === "/api/sessions");
    expect(post?.body && JSON.parse(post.body)).toMatchObject({
      display_name: "sglog",
      agent_type: "codex",
      working_directory: "/code/sglog",
    });
    // 和 New Agent 一样：会话进了列表才开 Tab（不然会被 prune 立刻关掉）。
    expect(screen.queryByTestId("tab-mac:new")).toBeNull();
    await act(async () => {
      t.sock.send([{ type: "session_created", id: "mac:new", session: row("mac:new") }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("tab-mac:new")).toBeTruthy();
  });

  it("面板开着时全局快捷键让位给它", async () => {
    const t = setup([row("mac:a")]);
    await online(t);
    press({ key: "k", metaKey: true });
    await flush();
    // Alt+1 这时不该在背后偷偷切会话。
    expect(press({ key: "1", code: "Digit1", altKey: true })).toBe(false);
    expect(mounted).toEqual([]);
    fireEvent.keyDown(screen.getByTestId("palette-input"), { key: "Escape" });
    await flush();
    expect(screen.queryByTestId("palette-input")).toBeNull();
  });
});

describe("窄屏（手机没有键盘）", () => {
  it("桌面断点以下不装全局快捷键", async () => {
    const wide = window.innerWidth;
    Object.defineProperty(window, "innerWidth", { value: 375, configurable: true });
    try {
      const t = setup([row("mac:a")]);
      await online(t);
      expect(press({ key: "k", metaKey: true })).toBe(false);
      expect(screen.queryByTestId("palette-input")).toBeNull();
    } finally {
      Object.defineProperty(window, "innerWidth", { value: wide, configurable: true });
    }
  });
});
