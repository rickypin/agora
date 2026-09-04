// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import type { SessionRow, SocketLike, UnregisteredRow } from "./events";
import { KILL_BODY } from "./SessionSettings";
import { SessionStore } from "./store";
import { Workspace } from "./Workspace";

// 终端本体在 jsdom 里开不了（canvas / ResizeObserver）；这里只关心它的挂载 / 卸载。
const mounted: string[] = [];
const unmounted: string[] = [];
vi.mock("./TerminalView", async () => {
  const React = await import("react");
  return {
    TerminalView: ({ sessionId }: { sessionId: string }) => {
      React.useEffect(() => {
        mounted.push(sessionId);
        return () => {
          unmounted.push(sessionId);
        };
      }, [sessionId]);
      return React.createElement("div", { "data-testid": `term-${sessionId}` }, sessionId);
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

function row(id: string, status = "running"): SessionRow {
  return { id, node: "n", status, alive: true, display_name: id.slice(2), agent_type: "claude", reason: null };
}

function setup(rows: SessionRow[], unregistered: UnregisteredRow[] = []) {
  const sock = new FakeSocket();
  const store = new SessionStore({
    connect: () => sock,
    fetchSnapshot: async () => ({ sessions: rows, unregistered }),
    coalesceMs: 0,
  });
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  let killResponse: () => { status: number; body: unknown } = () => ({ status: 200, body: {} });
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    // New Agent 对话框的数据源；killResponse 只管会话的写端点。
    if (url.startsWith("/api/projects/worktrees")) return json({ worktrees: [] });
    if (url.startsWith("/api/projects")) return json({ projects: [{ path: "/p", name: "p", last_used_at: null }] });
    if (url.startsWith("/api/agents")) return json({ agents: [{ name: "a1", command: "a1" }] });
    if (url.startsWith("/api/system")) return json({ node: "n" });
    if (url === "/api/sessions" && method === "POST") return json({ id: "n:new" }, 201);
    if (url === "/api/sessions/adopt") return json({ id: "n:adopted" }, 201);
    const r = killResponse();
    return json(r.body, r.status);
  };
  const renders: string[] = [];
  const ui = render(
    <Workspace store={store} api={sessionApi(f)} catalog={catalogApi(f)} onRowRender={(id) => renders.push(id)} />,
  );
  return { ui, store, sock, requests, renders, setKill: (fn: typeof killResponse) => (killResponse = fn) };
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** 模拟 WS 连上：EventsClient 在 onopen 里拉全量快照。 */
async function online(t: ReturnType<typeof setup>) {
  await act(async () => {
    t.sock.onopen?.({});
  });
  await flush();
}

afterEach(() => {
  cleanup();
  mounted.length = 0;
  unmounted.length = 0;
});

describe("Workspace", () => {
  it("opening / switching / closing tabs only mounts and unmounts terminals, never writes to the API (A20)", async () => {
    const t = setup([row("n:a"), row("n:b")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    fireEvent.click(screen.getByTestId("row-n:b"));
    expect(mounted).toEqual(["n:a", "n:b"]);
    expect(unmounted).toEqual(["n:a"]); // 切 Tab：旧终端 detach
    fireEvent.click(screen.getByTestId("tab-n:a"));
    fireEvent.click(screen.getByTestId("close-n:a"));
    expect(screen.queryByTestId("tab-n:a")).toBeNull();
    expect(screen.getByTestId("term-n:b")).toBeTruthy();
    await flush();
    expect(t.requests.filter((r) => r.method !== "GET")).toEqual([]);
  });

  it("a status_changed event re-renders only the affected sidebar row (no full refresh)", async () => {
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    t.renders.length = 0;
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(t.renders).toEqual(["n:b"]);
    expect(t.store.changes).toBe(2); // 快照 + 这一次
    // 内容相等的事件：不重渲染。
    t.renders.length = 0;
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(t.renders).toEqual([]);
  });

  it("Kill asks for confirmation only when the node says so, with the spec copy (A21)", async () => {
    const t = setup([row("n:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    fireEvent.click(screen.getByText("Settings"));
    t.setKill(() => ({ status: 409, body: { error: "needs_confirmation", message: "会杀" } }));
    fireEvent.click(screen.getByTestId("kill"));
    await flush();
    expect(screen.getByRole("dialog").textContent).toContain(KILL_BODY);
    t.setKill(() => ({ status: 200, body: {} }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Kill" }));
    await flush();
    expect(t.requests.map((r) => [r.method, r.url, r.body])).toEqual([
      ["POST", "/api/sessions/n%3Aa/kill", "{}"],
      ["POST", "/api/sessions/n%3Aa/kill", JSON.stringify({ confirmed: true })],
    ]);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("New Agent opens the created session as a tab only once it is in the list", async () => {
    // POST 的 201 先于 `session_created` 到达：那时开 Tab 会被 prune 立刻关掉
    // （列表里还没有这一行），用户看到的是"创建了但没打开"。
    const t = setup([]);
    await online(t);
    fireEvent.click(screen.getByText("+ New Agent"));
    await flush();
    fireEvent.click(screen.getByTestId("create"));
    await flush();
    expect(t.requests.some((r) => r.method === "POST" && r.url === "/api/sessions")).toBe(true);
    expect(screen.queryByTestId("tab-n:new")).toBeNull();

    await act(async () => {
      t.sock.send([{ type: "session_created", id: "n:new", session: row("n:new") }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("tab-n:new")).toBeTruthy();
    expect(mounted).toEqual(["n:new"]);
  });

  it("Rename sends PATCH even with the same name; Delete metadata is a DELETE, not a kill", async () => {
    const t = setup([row("n:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    fireEvent.click(screen.getByText("Settings"));
    fireEvent.click(screen.getByText("Rename"));
    await flush();
    fireEvent.click(screen.getByText("Delete metadata"));
    await flush();
    expect(t.requests.map((r) => [r.method, r.url, r.body])).toEqual([
      ["PATCH", "/api/sessions/n%3Aa", JSON.stringify({ display_name: "a" })],
      ["DELETE", "/api/sessions/n%3Aa", undefined],
    ]);
  });

  it("unregistered runtime sessions show as Unknown Agent and adopt with the user's choices (7cu)", async () => {
    const t = setup([row("n:a")], [
      {
        runtime_ref: "tmux:default:manual",
        name: "manual",
        title: "",
        alive: true,
        managed: false,
        working_directory: "/p",
        agent_hint: "claude",
        node: "n",
      },
    ]);
    await online(t);
    const snapshotsBefore = t.store.client.snapshots;
    expect(screen.getByText("Unknown Agent（像 claude）")).toBeTruthy();
    fireEvent.click(screen.getByTestId("unreg-tmux:default:manual"));
    const form = screen.getByTestId("adopt-tmux:default:manual");
    // hint 只是默认值：用户改成 codex 就发 codex。
    fireEvent.change(within(form).getByLabelText("采纳：名字"), { target: { value: "手动起的" } });
    fireEvent.change(within(form).getByLabelText("采纳：agent 类型"), { target: { value: "codex" } });
    fireEvent.submit(form);
    await flush();
    const adopt = t.requests.find((r) => r.url === "/api/sessions/adopt");
    expect(adopt?.method).toBe("POST");
    expect(JSON.parse(adopt!.body!)).toEqual({
      runtime_ref: "tmux:default:manual",
      display_name: "手动起的",
      project: "/p",
      agent_type: "codex",
    });
    // 未登记列表不走事件流：采纳后主动重拉一次快照。
    expect(t.store.client.snapshots).toBe(snapshotsBefore + 1);
    expect(screen.queryByTestId("adopt-tmux:default:manual")).toBeNull();
  });

  it("an external session is tagged in the sidebar and opens without a terminal (A16)", async () => {
    const t = setup([{ ...row("n:x"), origin: "external", agent_type: "claude" }]);
    await online(t);
    expect(screen.getByText("external")).toBeTruthy();
    fireEvent.click(screen.getByTestId("row-n:x"));
    expect(screen.getByTestId("no-terminal")).toBeTruthy();
    expect(mounted).toEqual([]);
  });
});
