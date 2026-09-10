// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import type { SessionRow, SocketLike, UnregisteredRow } from "./events";
import { API_VERSION, HealthWatcher, VersionWatcher } from "./health";
import type { NotificationLike, NotifierDeps, Permission } from "./notify";
import { KILL_BODY } from "./SessionSettings";
import { FREEZE_MS } from "./stableOrder";
import { SessionStore } from "./store";
import { Workspace } from "./Workspace";

// 终端本体在 jsdom 里开不了（canvas / ResizeObserver）；这里只关心它的挂载 / 卸载。
const mounted: string[] = [];
const unmounted: string[] = [];
// 每次挂载用的 socket：`rw:default`（会话终端，connect 未注入）/ `ro:defaultDiffSocket`（只读 diff，agora-h1k.5）。
const sockets: string[] = [];
vi.mock("./TerminalView", async () => {
  const React = await import("react");
  return {
    TerminalView: ({ sessionId, connect, readOnly }: { sessionId: string; connect?: { name: string }; readOnly?: boolean }) => {
      React.useEffect(() => {
        mounted.push(sessionId);
        sockets.push(`${readOnly ? "ro" : "rw"}:${connect?.name ?? "default"}`);
        return () => {
          unmounted.push(sessionId);
        };
      }, [sessionId]);
      return React.createElement("div", { "data-testid": `term-${readOnly ? "diff-" : ""}${sessionId}` }, sessionId);
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
  return { id, node: "n", status, alive: true, display_name: id.slice(2), agent_type: "claude", reason: null, respond_via: "hook" };
}

/** 假的 Notification API：记下弹了什么，点击靠测试自己触发 onclick。 */
function fakeNotify(permission: Permission) {
  const created: { title: string; body: string; tag: string; note: NotificationLike }[] = [];
  const deps: NotifierDeps = {
    permission: () => permission,
    request: async () => (permission = "granted"),
    create: (title, opts) => {
      const note: NotificationLike = { onclick: null, close() {} };
      created.push({ title, body: opts.body, tag: opts.tag, note });
      return note;
    },
    focus: () => {},
  };
  return { deps, created };
}

function setup(rows: SessionRow[], unregistered: UnregisteredRow[] = [], notify?: NotifierDeps, health?: HealthWatcher, version?: VersionWatcher, strict = false) {
  const sock = new FakeSocket();
  const store = new SessionStore({
    connect: () => sock,
    fetchSnapshot: async () => ({ sessions: rows, unregistered }),
    coalesceMs: 0,
  });
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  let killResponse: () => { status: number; body: unknown } | Promise<{ status: number; body: unknown }> = () => ({
    status: 200,
    body: {},
  });
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
    // 展开区的改动列表（agora-h1k.5）：一律一个改了的文件。
    if (url.endsWith("/changes")) return json({ files: [{ path: "a.txt", status: "modified" }], branch: "main", reason: null });
    if (url === "/api/sessions" && method === "POST") return json({ id: "n:new" }, 201);
    if (url === "/api/sessions/adopt") return json({ id: "n:adopted" }, 201);
    const r = await killResponse();
    return json(r.body, r.status);
  };
  const renders: string[] = [];
  const tree = (
    <Workspace
      store={store}
      api={sessionApi(f)}
      catalog={catalogApi(f)}
      onRowRender={(id) => renders.push(id)}
      notifyDeps={notify ?? fakeNotify("denied").deps}
      // 缺省一个健康的运行时；不给的话 Workspace 会去真的 fetch /api/health。
      health={health ?? new HealthWatcher({ fetchHealth: async () => ({ status: "ok", runtime: { status: "ok", reason: null } }) })}
      // 缺省一个同版本的节点；不给的话 Workspace 会去真的 fetch /api/system。
      version={version ?? new VersionWatcher({ fetchSystem: async () => ({ api_version: API_VERSION, version: "test", node: "n" }) })}
    />
  );
  // strict：main.tsx 真的把 App 包在 <StrictMode> 里，dev 下每次渲染跑两遍、effect 挂载 → 清理 → 再挂载。
  // 只有开着它才测得出「ref 在 render 阶段写」这类 bug（agora-3w8 的教训，见下面 moved 那一条）。
  const ui = render(strict ? <StrictMode>{tree}</StrictMode> : tree);
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

/** 侧栏行的显示顺序（不含 RowIdentity 的 row-node- / row-stale- 那些 testid）。 */
function rowOrder(): string[] {
  return screen.getAllByTestId(/^row-n:/).map((el) => el.getAttribute("data-testid")!.slice("row-".length));
}

/** 侧栏行的 `<li>`：moved 类与 data-ordinal 都挂在它身上。 */
function rowLi(id: string): HTMLElement {
  return screen.getByTestId(`row-${id}`).closest("li")!;
}

/**
 * 指针进 / 出侧栏。派的是 pointerover / pointerout 而不是 pointerenter / pointerleave：React 的
 * onPointerEnter / onPointerLeave 是 EnterLeaveEventPlugin 由 over / out 合成出来的，直接派
 * pointerenter 它一个字都收不到（2026-09-10 实测：先写成 fireEvent.pointerEnter，用例里顺序照旧重排）。
 */
function pointerIntoSidebar(): void {
  fireEvent.pointerOver(document.querySelector("aside.sidebar")!);
}
function pointerOutOfSidebar(): void {
  fireEvent.pointerOut(document.querySelector("aside.sidebar")!);
}

/**
 * 冻结窗口（A51，agora-4yr.4）走完、行落位。点一行本身就是"我在操作侧栏"，顺序会冻住 3 s——
 * 「点开 own 再点别的行、own 掉进折叠区」这类断言看的是**落位之后**的三段。
 *
 * fake timer 开 `shouldAdvanceTime`：用例里 `await new Promise(r => setTimeout(r, 5))` 等事件合并窗
 * 的那些真定时器照常按真实时间走，只有这 3 s 是跳过去的；要在冻结定时器被 arm **之前**就切到
 * fake timer，否则 arm 出去的是真定时器，advance 假时钟碰不到它（2026-09-10 实测）。
 */
async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(FREEZE_MS + 1);
  });
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  // 「看过」集合存在 localStorage（agora-j4w.1）：别让一个用例的记号漏到下一个。
  localStorage.clear();
  mounted.length = 0;
  unmounted.length = 0;
  sockets.length = 0;
});

describe("Workspace", () => {
  it("任务标签退回首条 prompt 摘要、与 ❯ 行一字不差时 ❯ 行省略（agora-k9r）", async () => {
    const same = { ...row("n:a"), task_ref: "把 config 迁到 yaml 并推上去", prompt: "把 config 迁到 yaml 并推上去", progress: "Edit x" };
    const differ = { ...row("n:b"), task_ref: "把 config 迁到 yaml 并推上去", prompt: "推上去了吗" };
    const t = setup([same, differ]);
    await online(t);
    expect(screen.getByTestId("label-n:a").textContent).toBe("把 config 迁到 yaml 并推上去");
    expect(screen.queryByTestId("prompt-n:a")).toBeNull();
    expect(screen.getByTestId("progress-n:a").textContent).toContain("Edit x");
    // 后来的 prompt 不同：两行都在。
    expect(screen.getByTestId("label-n:b").textContent).toBe("把 config 迁到 yaml 并推上去");
    expect(screen.getByTestId("prompt-n:b").textContent).toContain("推上去了吗");
  });

  it("a notification event pops a browser notification whose click lands on the row's in-place respond (A18)", async () => {
    const n = fakeNotify("granted");
    const t = setup([row("n:a"), row("n:b")], [], n.deps);
    await online(t);
    expect(screen.queryByTestId("notify-ask")).toBeNull(); // 已答过：不再问
    // 服务端先改行再通知（同一帧）：行变 WAITING，通知带 status。
    await act(async () => {
      t.sock.send([
        { type: "session_updated", id: "n:b", session: { ...row("n:b"), status: "waiting", source: "hook", reason: "permission", alive: true, detail: "Bash: rm -rf x", pending_decision: { request_id: "request-b", summary: "Bash: rm -rf x", epoch: 1 } } },
        { type: "notification", id: "n:b", title: "Claude / b @ n needs input", body: "Bash: rm -rf x", status: "waiting" },
      ]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(n.created.map((c) => [c.title, c.body, c.tag])).toEqual([["Claude / b @ n needs input", "Bash: rm -rf x", "n:b#1"]]);
    expect(screen.queryByTestId("respond-panel-n:b")).toBeNull(); // 还没点：不抢焦点
    await act(async () => {
      n.created[0]!.note.onclick?.({});
    });
    // 点击：该行成为 active，主区 crumb 之下画出它的回答面板（不是终端的事）。
    expect(screen.getByTestId("respond-panel-n:b")).toBeTruthy();
    expect(screen.getByTestId("allow")).toBeTruthy();
    expect(screen.getByTestId("term-n:b")).toBeTruthy();
  });

  it("clicking a turn_done row shows the respond panel in the main area between crumb and pane, and the sidebar row has no respond- testid (A50; agora-03k)", async () => {
    // agora-03k：回答区长在 260 px 的侧栏列里，几百行的 TURN_DONE 回复不可读。搬进主区后
    // 它永远在 crumb 与终端之间——「这一行的回答」与「这一行的终端」是同一对象的两面。
    const t = setup([{ ...row("n:a", "turn_done"), detail: "结论先说：可以做局部性能优化" }]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    const main = t.ui.container.querySelector("section.main")!;
    // 2026-09-10（agora-4yr.3）多了一层 .panels：回答面板与看结果面板合计限高 50vh，终端至少留一半。
    expect(Array.from(main.children).map((el) => el.className)).toEqual(["crumb", "panels", "pane"]);
    expect(screen.getByTestId("respond-panel-n:a")).toBe(main.querySelector(".respond-panel"));
    expect(main.querySelector(".panels")!.firstElementChild).toBe(screen.getByTestId("respond-panel-n:a"));
    // 输入框在最后一条回复之上（用户明确要求；限高 40vh 是布局事实，归代检）。
    const input = screen.getByTestId("next-input");
    const last = screen.getByTestId("respond-last");
    expect(input.compareDocumentPosition(last) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // 侧栏那一行只剩行本身：respond- 一个都没有。
    expect(t.ui.container.querySelector('.sidebar [data-testid^="respond-"]')).toBeNull();
  });

  it("the diff view hides the respond panel, keeps the result panel and 看 diff still switches to it", async () => {
    // 看 diff 的那一格在看结果，不在回答：回答面板不画（它会把 diff 挤窄，而且「打开终端」在那儿
    // 没有落点）。看结果面板**留着**——验收标准 / 改动列表与 diff 并排对照本来就是 MISSION §6.3
    // 要的；而且 Changes 的拉取挂在 mount 的 useEffect、卸载即丢 state，把它一起藏掉就等于
    // 「关掉 diff」必然重发一次 GET /changes（2026-09-06 代检时见过的那个形态，2026-09-10 定夺
    // agora-4yr.3 时复核后沿用）。下面数的就是这个计数。
    const t = setup([{ ...row("n:a", "turn_done"), detail: "done" }]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    const changes = () => t.requests.filter((r) => r.url === "/api/sessions/n%3Aa/changes").length;
    expect(screen.getByTestId("respond-panel-n:a")).toBeTruthy();
    expect(screen.getByTestId("result-panel-n:a")).toBeTruthy();
    expect(changes()).toBe(1);
    // 「看 diff」这个按钮就长在看结果面板里：它还能把主区切过去，本身也说明面板没被藏。
    fireEvent.click(screen.getByTestId("diff-n:a"));
    await flush();
    expect(screen.getByTestId("crumb-diff")).toBeTruthy();
    expect(screen.queryByTestId("respond-panel-n:a")).toBeNull();
    expect(screen.getByTestId("result-panel-n:a")).toBeTruthy();
    expect(changes()).toBe(1);
    fireEvent.click(screen.getByTestId("close-diff"));
    await flush();
    expect(screen.getByTestId("respond-panel-n:a")).toBeTruthy();
    expect(screen.getByTestId("result-panel-n:a")).toBeTruthy();
    expect(changes(), "关掉 diff 不许重拉 /changes：面板全程没卸载过").toBe(1);
  });

  it("a turn_done row with a task shows respond, acceptance and changes panels in that order above the terminal (A50)", async () => {
    // agora-03k 的最后一块：回答 → 验收标准 → 改动列表，三段都在主区、都在终端之上。
    // 顺序是 agora-h1k.3 定的：回答问题 / 给下一条指令是先做的事，对照验收看结果是后做的事。
    const task = { id: "agora-03k", title: "侧栏行展开区可读性差", priority: 2, acceptance: "回答 / 验收 / 改动三段都在主区，侧栏行里一个都没有。" };
    const t = setup([{ ...row("n:a", "turn_done"), detail: "结论先说：可以做局部性能优化", task }]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    const main = t.ui.container.querySelector("section.main")!;
    expect(Array.from(main.children).map((el) => el.className)).toEqual(["crumb", "panels", "pane"]);
    expect(Array.from(main.querySelector(".panels")!.children).map((el) => el.className)).toEqual(["respond-panel", "result-panel"]);
    // 面板内也按这个顺序，且三段都排在终端之前。
    const seq = [
      screen.getByTestId("respond-panel-n:a"),
      screen.getByTestId("acceptance-n:a"),
      screen.getByTestId("changes-n:a"),
      main.querySelector(".pane")!,
    ];
    for (let i = 0; i + 1 < seq.length; i++) {
      expect(seq[i].compareDocumentPosition(seq[i + 1]) & Node.DOCUMENT_POSITION_FOLLOWING, `第 ${i} 段排在第 ${i + 1} 段之前`).toBeTruthy();
    }
    // 文本就是数据本身：验收标准是 bd 的 acceptance_criteria 全文，改动列表是 GET /changes 的应答。
    expect(screen.getByTestId("acceptance-body-n:a").textContent).toBe(task.acceptance);
    expect(screen.getByTestId("acceptance-toggle-n:a").textContent).toContain("agora-03k");
    expect(screen.getByTestId("changes-list-n:a").textContent).toContain("M a.txt");
    // 侧栏那一行只剩行本身（守卫另有 Sidebar.test.tsx 一条；这里是整页装配后的同一件事）。
    expect(t.ui.container.querySelectorAll('aside.sidebar [data-testid^="respond-"],aside.sidebar [data-testid^="acceptance-"],aside.sidebar [data-testid^="changes-"]').length).toBe(0);
  });

  it("panels are absent for waiting rows except the respond panel", async () => {
    // WAITING 不在 Changes 的状态集合里——此刻该做的是回答问题，不是看结果。没有任务的 WAITING 行
    // 于是整个 result-panel 一格都不占（空面板会在终端上方白留一条边线）；有任务的仍然看得到验收
    // 标准（"做完算什么"与状态无关），改动列表照旧不占位、也不发请求。
    const task = { id: "agora-x", title: "有验收的等待行", priority: 2, acceptance: "验收标准与状态无关" };
    const t = setup([row("n:a", "waiting"), { ...row("n:b", "waiting"), task }]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    expect(screen.getByTestId("respond-panel-n:a")).toBeTruthy();
    expect(screen.queryByTestId("result-panel-n:a")).toBeNull();
    expect(screen.queryByTestId("changes-n:a")).toBeNull();
    fireEvent.click(screen.getByTestId("row-n:b"));
    await flush();
    expect(screen.getByTestId("respond-panel-n:b")).toBeTruthy();
    expect(screen.getByTestId("acceptance-body-n:b").textContent).toBe(task.acceptance);
    expect(screen.queryByTestId("changes-n:b")).toBeNull();
    expect(t.requests.filter((r) => r.url.endsWith("/changes"))).toEqual([]);
  });

  it("a notification click focuses the panel input", async () => {
    // MISSION §6.6「点击落到就地回答区」：点通知落到该行之后，人可以直接打字，不用再点一下输入框。
    const n = fakeNotify("granted");
    const t = setup([row("n:a"), row("n:b")], [], n.deps);
    await online(t);
    await act(async () => {
      t.sock.send([
        { type: "session_updated", id: "n:b", session: { ...row("n:b"), status: "turn_done", alive: true, detail: "Two files." } },
        { type: "notification", id: "n:b", title: "Claude / b @ n is done", body: "Two files.", status: "turn_done" },
      ]);
      await new Promise((r) => setTimeout(r, 5));
    });
    await act(async () => {
      n.created[0]!.note.onclick?.({});
    });
    // 聚焦推迟了一个宏任务（面板要赢过 TerminalView 挂载时的 term.focus()，见 RespondPanel 注释）。
    await flush();
    expect(document.activeElement).toBe(screen.getByTestId("next-input"));
  });

  it("Alt/Option+R focuses the panel input when present and does nothing otherwise", async () => {
    const t = setup([
      { ...row("n:a", "turn_done"), detail: "Two files." },
      row("n:b"),
      { ...row("n:w", "waiting"), reason: "permission", respond_via: "hook", detail: "Bash", pending_decision: { request_id: "req-1", summary: "Bash", epoch: 1 } },
    ]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    // 点行的焦点归终端（agora-p29 / agora-vcc 不变）：面板不抢，Alt/Option+R 才抢。
    expect(document.activeElement).not.toBe(screen.getByTestId("next-input"));
    fireEvent.keyDown(window, { code: "KeyR", altKey: true });
    await flush(); // 聚焦推迟一个宏任务，见 RespondPanel 里那段注释
    expect(document.activeElement).toBe(screen.getByTestId("next-input"));
    // WAITING 行落在第一个按钮上（Allow）：那一刻要按的就是它。
    fireEvent.click(screen.getByTestId("row-n:w"));
    fireEvent.keyDown(window, { code: "KeyR", altKey: true });
    await flush();
    expect(document.activeElement).toBe(screen.getByTestId("allow"));
    // RUNNING 行没有面板：什么都不做，焦点原地不动。
    fireEvent.click(screen.getByTestId("row-n:b"));
    const before = document.activeElement;
    fireEvent.keyDown(window, { code: "KeyR", altKey: true });
    await flush();
    expect(screen.queryByTestId("next-input")).toBeNull();
    expect(document.activeElement).toBe(before);
  });

  it("asks for notification permission once, in a banner that goes away after the answer", async () => {
    const n = fakeNotify("default");
    const t = setup([row("n:a")], [], n.deps);
    await online(t);
    // 没权限：通知静默丢掉，不抛。
    await act(async () => {
      t.sock.send([{ type: "notification", id: "n:a", title: "t", body: "", status: "failed" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(n.created).toEqual([]);
    fireEvent.click(within(screen.getByTestId("notify-ask")).getByText("允许通知"));
    await flush();
    expect(screen.queryByTestId("notify-ask")).toBeNull();
    await act(async () => {
      t.sock.send([{ type: "notification", id: "n:a", title: "t2", body: "", status: "failed" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(n.created.map((c) => c.title)).toEqual(["t2"]);
  });

  it("switching rows and closing the terminal view only mounts and unmounts terminals, never writes to the API (A20)", async () => {
    // 主区只跟侧栏 active 行，没有顶栏标签页（agora-a46）：切行 = 旧终端 detach、新终端挂载；
    // crumb 的「关闭」= Detach：主区清空。全程零写请求（MISSION §4.6；不变量 4）。
    const t = setup([row("n:a"), row("n:b")]);
    await online(t);
    expect(screen.queryByRole("tablist")).toBeNull();
    fireEvent.click(screen.getByTestId("row-n:a"));
    expect(screen.getByTestId("crumb").textContent).toBe("a / claude @ n");
    fireEvent.click(screen.getByTestId("row-n:b"));
    expect(mounted).toEqual(["n:a", "n:b"]);
    expect(unmounted).toEqual(["n:a"]); // 切行：旧终端 detach
    expect(screen.getByTestId("crumb").textContent).toBe("b / claude @ n");
    expect(screen.queryByTestId("term-n:a")).toBeNull();
    fireEvent.click(screen.getByTestId("close-view"));
    expect(screen.queryByTestId("term-n:b")).toBeNull();
    expect(screen.queryByTestId("crumb")).toBeNull();
    expect(screen.getByText("从左侧选一个 agent。")).toBeTruthy();
    expect(unmounted).toEqual(["n:a", "n:b"]);
    // 侧栏行都还在：关掉的只是视图。
    expect(screen.getAllByTestId(/^row-n:/).length).toBe(2);
    await flush();
    expect(t.requests.filter((r) => r.method !== "GET")).toEqual([]);
  });

  it("the terminal view clears when its session leaves the list (agora-a46)", async () => {
    const rows = [row("n:a"), row("n:b")];
    const t = setup(rows);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    expect(screen.getByTestId("term-n:a")).toBeTruthy();
    rows.splice(0, 1);
    await act(async () => {
      t.sock.send([{ type: "session_removed", id: "n:a" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.queryByTestId("term-n:a")).toBeNull();
    expect(screen.getByText("从左侧选一个 agent。")).toBeTruthy();
    expect(unmounted).toEqual(["n:a"]);
  });

  it("a status_changed event re-renders only the affected sidebar row (no full refresh)", async () => {
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    t.renders.length = 0;
    // 不改分数的变化（RUNNING 里换个 activity）：行不挪位，别的行连序号都没变，一行都不重渲染。
    // 挪位的变化（RUNNING → WAITING 提到最上）会让序号变了的行跟着重渲染，那是真的变了。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "running", source: "hook", reason: "activity", alive: true, progress: "Edit x" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(t.renders).toEqual(["n:b"]);
    expect(t.store.changes).toBe(2); // 快照 + 这一次
    // 内容相等的事件：不重渲染。
    t.renders.length = 0;
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "running", source: "hook", reason: "activity", alive: true, progress: "Edit x" }]);
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
    // 确认后节点要等 TERM → 5 s 宽限（ADR-001 D2），这几秒里要有字说明在结束（agora-9nv）。
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    t.setKill(() => gate.then(() => ({ status: 200, body: {} })));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Kill" }));
    await flush();
    expect(screen.getByTestId("ending-note").textContent).toContain("正在结束");
    expect(screen.getByTestId("kill")).toHaveProperty("disabled", true);
    release();
    await flush();
    expect(screen.queryByTestId("ending-note")).toBeNull();
    // 展开的行还会 GET 一次 /changes（agora-h1k.5，只读）；这里只看写请求。
    expect(t.requests.filter((r) => !r.url.endsWith("/changes")).map((r) => [r.method, r.url, r.body])).toEqual([
      ["POST", "/api/sessions/n%3Aa/kill", "{}"],
      ["POST", "/api/sessions/n%3Aa/kill", JSON.stringify({ confirmed: true })],
    ]);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("the ending note stays while the row says killed_at && alive, and clears when the process is gone (agora-284)", async () => {
    // 节点只同步等 1 s：进程吃掉 TERM 时 kill 立即 200 返回仍 alive 的行，宽限在后台走。这时请求
    // 已经不在飞了，"正在结束"只能看行本身（killed_at 已写而 alive 仍 true），直到行推成 FINISHED。
    const t = setup([row("n:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    fireEvent.click(screen.getByText("Settings"));
    t.setKill(() => ({ status: 409, body: { error: "needs_confirmation", message: "会杀" } }));
    fireEvent.click(screen.getByTestId("kill"));
    await flush();
    const killedRow = { ...row("n:a"), killed_at: "2026-09-07T10:00:00Z", alive: true };
    t.setKill(() => ({ status: 200, body: killedRow }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Kill" }));
    await flush();
    // 请求已经回来了，但行还没变：没有 killed_at 就没有提示（旧行为），推来带 killed_at 的行才有。
    expect(screen.queryByTestId("ending-note")).toBeNull();
    await act(async () => {
      t.sock.send([{ type: "session_updated", id: "n:a", session: killedRow }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("ending-note").textContent).toContain("正在结束");
    // 宽限满、进程退了：轮询推 status_changed（alive:false），提示消失。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:a", status: "finished", source: "process", reason: "killed by user (signal KILL)", alive: false }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.queryByTestId("ending-note")).toBeNull();
  });

  it("Restart is disabled for adopted sessions that never recorded a command (agora-vto)", async () => {
    const t = setup([{ ...row("n:a"), origin: "adopted", command: null }]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    fireEvent.click(screen.getByText("Settings"));
    expect(screen.getByRole("button", { name: "Restart" })).toHaveProperty("disabled", true);
    expect(screen.getByTestId("kill")).toHaveProperty("disabled", false);
  });

  it("New Agent selects the created session only once it is in the list", async () => {
    // POST 的 201 先于 `session_created` 到达：那时选中会被"行没了就清空"立刻清掉
    // （列表里还没有这一行），用户看到的是"创建了但没打开"。
    const t = setup([]);
    await online(t);
    fireEvent.click(screen.getByText("+ New Agent"));
    await flush();
    fireEvent.click(screen.getByTestId("create"));
    await flush();
    expect(t.requests.some((r) => r.method === "POST" && r.url === "/api/sessions")).toBe(true);
    expect(screen.queryByTestId("term-n:new")).toBeNull();

    await act(async () => {
      t.sock.send([{ type: "session_created", id: "n:new", session: row("n:new") }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("term-n:new")).toBeTruthy();
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
    expect(t.requests.filter((r) => !r.url.endsWith("/changes")).map((r) => [r.method, r.url, r.body])).toEqual([
      ["PATCH", "/api/sessions/n%3Aa", JSON.stringify({ display_name: "a" })],
      ["DELETE", "/api/sessions/n%3Aa", undefined],
    ]);
  });

  it("clearing the Finished section from the counts line sends one DELETE per folded row and nothing for an unseen agora FINISHED row (agora-j4w.2)", async () => {
    const t = setup([
      row("n:wait", "waiting"),
      { ...row("n:ext", "finished"), origin: "external" },
      { ...row("n:own", "finished"), origin: "agora" },
    ]);
    await online(t);
    // own 没看过：折叠区只有 ext 一行。
    expect(screen.getByTestId("clear-finished").textContent).toBe("Finished 2");
    fireEvent.click(screen.getByTestId("clear-finished"));
    fireEvent.click(screen.getByText("Delete 1"));
    await flush();
    expect(t.requests.filter((r) => r.method === "DELETE").map((r) => r.url)).toEqual(["/api/sessions/n%3Aext"]);
    expect(screen.getByTestId("clear-finished-note").textContent).toBe("已清理 1 行");
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

  it("sorts by attention with NEEDS ATTENTION above RUNNING and ordinals following the display order (A17 A23)", async () => {
    const t = setup([
      row("n:run"),
      { ...row("n:p3", "waiting"), task: { id: "x-3", title: "低", priority: 3 } },
      row("n:fail", "failed"),
      { ...row("n:p1", "waiting"), task: { id: "x-1", title: "高", priority: 1 } },
      row("n:done", "turn_done"),
    ]);
    await online(t);
    const rows = screen.getAllByTestId(/^row-n:/).map((el) => el.getAttribute("data-testid"));
    expect(rows).toEqual(["row-n:fail", "row-n:p1", "row-n:p3", "row-n:done", "row-n:run"]);
    // 分区标题：NEEDS ATTENTION 在最上，RUNNING 在第一条 running 之前。
    const list = screen.getByTestId("section-attention").parentElement!;
    const order = Array.from(list.querySelectorAll("[data-testid]"))
      .map((el) => el.getAttribute("data-testid")!)
      .filter((id) => id.startsWith("section-") || (id.startsWith("row-") && !id.startsWith("row-node-") && !id.startsWith("row-stale-")));
    expect(order).toEqual(["section-attention", "row-n:fail", "row-n:p1", "row-n:p3", "row-n:done", "section-running", "row-n:run"]);
    // 第一列是任务：issue id + 标题。
    expect(screen.getByTestId("label-n:p1").textContent).toBe("x-1 高");
    expect(screen.getByTestId("label-n:run").textContent).toBe("run");
    expect(screen.getByTestId("counts").textContent).toBe("Running 1 · Needs Input 2 · Turn Done 1 · Failed 1");
    // Alt/Option+N 跳的是显示顺序里的第 N 条：第 2 条是 p1。
    fireEvent.keyDown(window, { code: "Digit2", altKey: true });
    expect(screen.getByTestId("term-n:p1")).toBeTruthy();
    expect(screen.getByTestId("crumb").textContent).toContain("@ n");
  });

  it("an agora FINISHED row stays in NEEDS ATTENTION until it has been opened and left; external ones start collapsed (A46)", async () => {
    // MISSION §4.6「看过」证据 ①：选中展开过一次。记在离开那一行的时刻：选中期间它留在原位。
    // 点行会冻住顺序 3 s（A51，agora-4yr.4），所以每次"点了之后该重排"的断言前先 settle()。
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const t = setup([
      { ...row("n:ext", "finished"), origin: "external" },
      { ...row("n:own", "finished"), origin: "agora" },
      row("n:run"),
      row("n:wait", "waiting"),
    ]);
    await online(t);
    const order = () =>
      Array.from(screen.getByTestId("section-attention").parentElement!.querySelectorAll("[data-testid]"))
        .map((el) => el.getAttribute("data-testid")!)
        .filter((id) => id.startsWith("section-") || (id.startsWith("row-") && !id.startsWith("row-node-") && !id.startsWith("row-stale-")));
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished"]);
    expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 1");
    // 点开 own：它还在 NEEDS ATTENTION（主区开着它的终端，侧栏不能让它消失进折叠区）。
    fireEvent.click(screen.getByTestId("row-n:own"));
    expect(screen.getByTestId("term-n:own")).toBeTruthy();
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished"]);
    // 再点别的行：own 进折叠区，计数 +1；localStorage 记下了。冻结期间它先留在原位（A51 的守卫在下面
    // 「a row that becomes finished during a freeze…」那条），3 s 落位后才是这里断言的三段。
    fireEvent.click(screen.getByTestId("row-n:wait"));
    await settle();
    expect(order()).toEqual(["section-attention", "row-n:wait", "section-running", "row-n:run", "section-finished"]);
    expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 2");
    expect(JSON.parse(localStorage.getItem("agora.seen-finished") ?? "[]")).toEqual(["n:own@"]);
    // Alt/Option+N 的序号跟着三段拼接走：折叠区收着时第 3 条仍是 ext（wait, run, ext, own）。
    fireEvent.keyDown(window, { code: "Digit3", altKey: true });
    expect(screen.getByTestId("no-terminal")).toBeTruthy();
    expect(screen.getByTestId("crumb").textContent).toContain("ext");
    // 选中的行落在收起的折叠区里：折叠区自动展开，侧栏能看到它高亮（agora-4nk）。
    expect(screen.getByTestId("row-n:ext").closest("li")!.classList.contains("selected")).toBe(true);
    expect(screen.getByTestId("section-finished").getAttribute("aria-expanded")).toBe("true");
    // 手动收回去，下面继续看收起态的分区。
    fireEvent.click(screen.getByTestId("section-finished"));
    // 选中 running 行再离开不算看过；它之后 FINISHED 时回到 NEEDS ATTENTION。
    fireEvent.click(screen.getByTestId("row-n:run"));
    fireEvent.click(screen.getByTestId("row-n:wait"));
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:run", status: "finished", source: "process", reason: "exited", alive: false }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    await settle();
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:run", "section-finished"]);
    // 看过的行又跑起来（Restart）：记号作废，下一次 FINISHED 是新结果。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:own", status: "running", source: "process", reason: "restarted", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(JSON.parse(localStorage.getItem("agora.seen-finished") ?? "[]")).toEqual([]);
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:own", status: "finished", source: "process", reason: "exited", alive: false }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    await settle();
    // 两条都是 FINISHED、都没有 status_since：稳定排序保持快照里的原顺序（own 在 run 前）。
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:own", "row-n:run", "section-finished"]);
  });

  it("a seen mark dies with its completion: running+finished in one batch, or a resync with a newer status_since, put the row back in NEEDS ATTENTION (agora-23h)", async () => {
    // fetchSnapshot 每次都读这个数组：改它再发 resync 就是"重连后拿到的全量"。点行会冻住顺序 3 s（A51），
    // 断言"进了折叠区"之前先 settle()。
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const rows: SessionRow[] = [row("n:wait", "waiting"), { ...row("n:own", "finished"), origin: "agora", status_since: 100 }];
    const t = setup(rows);
    await online(t);
    const order = () =>
      Array.from(screen.getByTestId("section-attention").parentElement!.querySelectorAll("[data-testid]"))
        .map((el) => el.getAttribute("data-testid")!)
        .filter((id) => id.startsWith("section-") || (id.startsWith("row-") && !id.startsWith("row-node-") && !id.startsWith("row-stale-")));
    // 看过 own：进折叠区，记号带着这一次完成的 status_since。
    fireEvent.click(screen.getByTestId("row-n:own"));
    fireEvent.click(screen.getByTestId("row-n:wait"));
    await settle();
    expect(order()).toEqual(["section-attention", "row-n:wait", "section-finished"]);
    expect(JSON.parse(localStorage.getItem("agora.seen-finished") ?? "[]")).toEqual(["n:own@100"]);
    // Restart 后 running 与新的 finished 在同一批到达（合并窗）：中间态从没进过 byId，靠 status_since 认出是新结果。
    await act(async () => {
      t.sock.send([
        { type: "status_changed", id: "n:own", status: "running", source: "process", reason: "restarted", alive: true, status_since: 150 },
        { type: "status_changed", id: "n:own", status: "finished", source: "process", reason: "exited", alive: false, status_since: 200 },
      ]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:own"]);
    expect(JSON.parse(localStorage.getItem("agora.seen-finished") ?? "[]")).toEqual([]);
    // 再看一次，然后断线重连 resync 直接拿到又一次完成（status_since 更新）：同样回到 NEEDS ATTENTION。
    fireEvent.click(screen.getByTestId("row-n:own"));
    fireEvent.click(screen.getByTestId("row-n:wait"));
    await settle();
    expect(order()).toEqual(["section-attention", "row-n:wait", "section-finished"]);
    rows[1] = { ...row("n:own", "finished"), origin: "agora", status_since: 300 };
    await act(async () => {
      t.sock.send([{ type: "resync" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(order()).toEqual(["section-attention", "row-n:wait", "row-n:own"]);
  });

  it("the sidebar mode survives a remount via localStorage (A47)", async () => {
    const mem = new Map<string, string>();
    const fake: Storage = {
      get length() {
        return mem.size;
      },
      clear() {
        mem.clear();
      },
      getItem(k) {
        return mem.get(k) ?? null;
      },
      key(i) {
        return [...mem.keys()][i] ?? null;
      },
      removeItem(k) {
        mem.delete(k);
      },
      setItem(k, v) {
        mem.set(k, v);
      },
    };
    vi.stubGlobal("localStorage", fake);
    try {
      const t = setup([row("n:a")]);
      await online(t);
      expect(screen.getByTestId("sidebar-mode-attention").getAttribute("aria-pressed")).toBe("true");
      fireEvent.click(screen.getByTestId("sidebar-mode-tree"));
      expect(screen.getByTestId("sidebar-mode-tree").getAttribute("aria-pressed")).toBe("true");
      expect(fake.getItem("agora.sidebar-mode")).toBe("tree");
      t.ui.unmount();
      const t2 = setup([row("n:a")]);
      await online(t2);
      expect(screen.getByTestId("sidebar-mode-tree").getAttribute("aria-pressed")).toBe("true");
      expect(screen.getByTestId("sidebar-mode-attention").getAttribute("aria-pressed")).toBe("false");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("shows the two hook lines, or one pane preview line when the session has no hooks", async () => {
    const t = setup([
      { ...row("n:h", "waiting"), prompt: "把 sidebar 换掉", progress: "Edit Sidebar.tsx", status_since: Math.floor(Date.now() / 1000) - 200 },
      { ...row("n:s"), agent_type: "shell", preview: "$ cargo test", reason: null },
    ]);
    await online(t);
    expect(screen.getByTestId("prompt-n:h").textContent).toBe("❯ 把 sidebar 换掉");
    expect(screen.getByTestId("progress-n:h").textContent).toBe("↳ Edit Sidebar.tsx");
    expect(screen.queryByTestId("preview-n:h")).toBeNull();
    expect(screen.getByTestId("state-n:h").textContent).toBe("waiting 3m");
    expect(screen.getByTestId("preview-n:s").textContent).toBe("$ cargo test");
    // status_changed 带来的新预览就地替换；没带的字段沿用。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:h", status: "turn_done", source: "hook", reason: "turn ended", alive: true, progress: "改完了" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("progress-n:h").textContent).toBe("↳ 改完了");
    expect(screen.getByTestId("prompt-n:h").textContent).toBe("❯ 把 sidebar 换掉");
    // 一条 running 行变成 WAITING 后要挪进 NEEDS ATTENTION（A17）。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:s", status: "waiting", source: "text", reason: "prompt", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    // WAITING 90 > TURN_DONE 85：s 排到 h 前面；两条都在 NEEDS ATTENTION，RUNNING 标题消失。
    expect(screen.getAllByTestId(/^row-n:/).map((el) => el.getAttribute("data-testid"))).toEqual(["row-n:s", "row-n:h"]);
    expect(screen.queryByTestId("section-running")).toBeNull();
  });

  it("shows the hook-not-connected hint from the server and drops it once hooks are heard (agora-dvh.15)", async () => {
    const hint = "终端活动了一阵仍没收到任何 hook 事件。请在 Codex TUI 里输入 /hooks，按 t 信任 agora 的条目。";
    const t = setup([{ ...row("n:c"), agent_type: "codex", hooks_unheard: hint }]);
    await online(t);
    const el = screen.getByTestId("hooks-unheard-n:c");
    expect(el.textContent).toBe(`⚠ hook 没接上：${hint}`);
    expect(el.getAttribute("title")).toBe(hint);
    // 第一条 hook 事件到了：服务端把字段清成 null，提示消失。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:c", status: "running", source: "hook", reason: "prompt submitted", alive: true, hooks_unheard: null }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.queryByTestId("hooks-unheard-n:c")).toBeNull();
  });

  it("shows the runtime's own degraded reason in a banner and drops it once health is ok again (agora-bgr)", async () => {
    const reason = "运行时 server 不可用: protocol version mismatch (client 8, server 7)";
    let report: unknown = { status: "ok", runtime: { status: "degraded", reason, path_source: "shell" } };
    const health = new HealthWatcher({ fetchHealth: async () => report, okMs: 1e9, degradedMs: 1e9 });
    const t = setup([{ ...row("n:a", "unknown"), alive: false }], [], undefined, health);
    await online(t);
    const banner = screen.getByTestId("runtime-degraded");
    expect(banner.textContent).toBe(`⚠ 运行时 degraded：${reason}。会话状态暂不可知，进程没有被杀。`);
    expect(banner.getAttribute("title")).toBe(reason);
    // 运行时恢复（server 换代 / 升级完成）：下一次重拉转回 ok，横幅自己消失，不用刷新页面。
    report = { status: "ok", runtime: { status: "ok", reason: null, path_source: "shell" } };
    await act(async () => {
      await health.refresh();
    });
    expect(screen.queryByTestId("runtime-degraded")).toBeNull();
    // 公开子集（未认证形态，没有 runtime 段）不算 degraded：不会因此显示任何节点配置（ADR-003 D1）。
    report = { status: "ok" };
    await act(async () => {
      await health.refresh();
    });
    expect(screen.queryByTestId("runtime-degraded")).toBeNull();
  });

  it("shows the api_version banner and renders no session data when the node speaks another major; re-checks on WS reconnect and comes back once compatible (agora-7ku.4)", async () => {
    let system: unknown = { api_version: { major: 2, minor: 0 }, version: "0.9.0", node: "n" };
    const version = new VersionWatcher({ fetchSystem: async () => system, page: { major: 1, minor: 0 } });
    const t = setup([row("n:a"), row("n:b", "waiting")], [], undefined, undefined, version);
    // 首连：WS 连上的那一刻比一次。
    await online(t);
    expect(version.checks).toBe(1);
    const banner = screen.getByTestId("api-version-mismatch");
    expect(banner.textContent).toBe("⚠ 节点 API 版本 2.0，页面按 1.0 构建，请刷新页面");
    expect(banner.getAttribute("title")).toBe("节点 API 版本 2.0，页面按 1.0 构建，请刷新页面");
    // 会话区一个字不渲染：侧栏行、终端都不在——快照已经到了 store 里，但形态可能变了，渲染就是错读。
    expect(t.store.snapshot()).toHaveLength(2);
    expect(screen.queryByTestId("row-n:a")).toBeNull();
    expect(screen.queryByTestId("row-n:b")).toBeNull();
    expect(screen.queryByText("从左侧选一个 agent。")).toBeNull();
    expect(mounted).toEqual([]);
    // 横幅在 runtime-degraded 那一条的位置与样式上，不是第二套。
    expect(banner.className).toBe("runtime-degraded");

    // 读不出版本（旧二进制的裸整数）同样是不兼容，文案指向升级节点。
    system = { api_version: 1, version: "0.0.1", node: "n" };
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    expect(version.checks).toBe(2);
    expect(screen.getByTestId("api-version-mismatch").textContent).toBe("⚠ 节点没有报告可识别的 API 版本，页面按 1.0 构建，请升级节点后刷新页面");

    // 节点升级完成、WS 重连：版本对上了，页面自己回来，不用用户做什么。
    system = { api_version: { major: 1, minor: 3 }, version: "1.3.0", node: "n" };
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    expect(version.checks).toBe(3);
    expect(screen.queryByTestId("api-version-mismatch")).toBeNull();
    expect(screen.getByTestId("row-n:a")).toBeTruthy();
    expect(screen.getByTestId("row-n:b")).toBeTruthy();
  });

  it("a compatible node shows no api_version banner, minor differences included (agora-7ku.4)", async () => {
    const version = new VersionWatcher({
      fetchSystem: async () => ({ api_version: { major: 1, minor: 9 }, version: "1.9.0", node: "n" }),
      page: { major: 1, minor: 0 },
    });
    const t = setup([row("n:a")], [], undefined, undefined, version);
    await online(t);
    expect(version.snapshot()?.kind).toBe("compatible");
    expect(screen.queryByTestId("api-version-mismatch")).toBeNull();
    expect(screen.getByTestId("row-n:a")).toBeTruthy();
  });

  it("an external session is tagged in the sidebar and opens without a terminal (A16)", async () => {
    const t = setup([{ ...row("n:x"), origin: "external", agent_type: "claude" }]);
    await online(t);
    expect(screen.getByText("external")).toBeTruthy();
    fireEvent.click(screen.getByTestId("row-n:x"));
    expect(screen.getByTestId("no-terminal")).toBeTruthy();
    expect(mounted).toEqual([]);
  });

  it("labels the node on every row once the local node is known, local ones uncolored (A49; reverses agora-7ku.5)", async () => {
    // MISSION §3.5 每行标明节点；本机 id 来自 /api/system（setup 里是 "n"），本机也标、不着色。
    const t = setup([row("n:a"), { ...row("zuan:7"), node: "zuan", stale: true }]);
    await online(t);
    const local = screen.getByTestId("row-node-n:a");
    expect(local.getAttribute("data-node")).toBe("n");
    expect(local.className).toContain("local");
    expect(screen.getByTestId("row-node-zuan:7").textContent).toBe("@ zuan");
    expect(screen.getByTestId("row-node-zuan:7").className).toContain("peer");
  });

  it("the header names 本机 after /api/system's node and shows every peer from the same health poll, and a peer going stale keeps its last-seen (agora-7ku.12, 7ku.5)", async () => {
    const seen = "2026-09-02T23:10:00Z";
    let report: unknown = {
      status: "ok",
      runtime: { status: "ok", reason: null },
      peers: { zuan: { online: true, last_seen: seen, retrying: false, last_error: null } },
    };
    const health = new HealthWatcher({ fetchHealth: async () => report, okMs: 1e9, degradedMs: 1e9 });
    const t = setup([row("n:a")], [], undefined, health);
    await online(t);
    expect(health.polls).toBe(1); // 节点状态没有自己的轮询
    // 本机那一枚叫 /api/system 报的 node id（setup 里是 "n"），不是写死的"本机"。
    expect(screen.getByTestId("node-n").textContent).toBe("n●");
    expect(screen.queryByTestId("node-本机")).toBeNull();
    expect(screen.getByTestId("node-zuan").textContent).toBe("zuan●");
    // zuan 掉线：还在 header 上，带"上次见到"，不是消失（不变量 8）；横幅不出现。
    report = {
      status: "ok",
      runtime: { status: "ok", reason: null },
      peers: { zuan: { online: false, last_seen: seen, retrying: true, last_error: "unreachable" } },
    };
    await act(async () => {
      await health.refresh();
    });
    expect(screen.getByTestId("node-zuan").textContent).toContain("zuan✗不可达 · 上次见到 ");
    expect(screen.queryByTestId("runtime-degraded")).toBeNull();
  });

  it("a peer_changed event moves the header dot at once, without waiting for the health poll; a reconnect re-pulls health (agora-c8h)", async () => {
    const seen = "2026-09-02T23:10:00Z";
    const report: unknown = {
      status: "ok",
      runtime: { status: "ok", reason: null },
      peers: { zuan: { online: true, last_seen: seen, retrying: false, last_error: null } },
    };
    const health = new HealthWatcher({ fetchHealth: async () => report, okMs: 1e9, degradedMs: 1e9 });
    const t = setup([row("n:a")], [], undefined, health);
    await online(t);
    expect(screen.getByTestId("node-zuan").textContent).toBe("zuan●");
    // 侧栏行变灰的同一条流上来了 peer_changed：Header 的点立刻变，health 一次都没多拉。
    await act(async () => {
      t.sock.send([{ type: "peer_changed", name: "zuan", peer: { online: false, last_seen: seen, retrying: true, last_error: "fingerprint_mismatch" } }]);
    });
    expect(screen.getByTestId("node-zuan").textContent).toContain("zuan✗指纹不匹配 · 上次见到 ");
    expect(health.polls).toBe(1);
    await act(async () => {
      t.sock.send([{ type: "peer_changed", name: "zuan", peer: { online: true, last_seen: seen, retrying: false, last_error: null } }]);
    });
    expect(screen.getByTestId("node-zuan").textContent).toBe("zuan●");
    expect(health.polls).toBe(1);
    // 断流再连上：错过的翻转补不回来，重连时重拉一次 health 对齐（首连不拉，上面 polls 仍是 1）。
    await online(t);
    expect(health.polls).toBe(2);
  });
});

describe("Workspace · 重排稳定（A51，agora-4yr.4）", () => {
  /** 侧栏三段的样子：标题与行按 DOM 先后。 */
  const sectioned = () =>
    Array.from(screen.getByTestId("section-attention").parentElement!.querySelectorAll("[data-testid]"))
      .map((el) => el.getAttribute("data-testid")!)
      .filter((id) => id.startsWith("section-") || (id.startsWith("row-") && !id.startsWith("row-node-") && !id.startsWith("row-stale-")));

  it("while the pointer is over the sidebar a status change does not reorder rows but updates the symbol (A51)", async () => {
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    pointerIntoSidebar();
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:c", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    // 位置一行没动：WAITING 的 c 按实时排序早该跳到最前，人还悬在侧栏里，就别动它。
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    // 内容照常更新——冻的是次序不是状态。用户要看到"它在等我了"，只是不要它跑掉。
    const dot = screen.getByTestId("row-n:c").querySelector(".dot")!;
    expect(dot.textContent).toBe("⚠");
    expect(dot.classList.contains("st-waiting")).toBe(true);
    expect(screen.getByTestId("counts").textContent).toBe("Running 2 · Needs Input 1");
    // 冻结期间没有任何一行是"刚落位"的，不该有高亮。
    expect(document.querySelectorAll("li.moved")).toHaveLength(0);
  });

  it("after leaving the sidebar and 3 s (fake timers) the row moves and carries the moved class (A51)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    pointerIntoSidebar();
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    pointerOutOfSidebar();
    // 指针一离开不等于立刻跳：3 s 还没走完，顺序照旧（人可能只是滑过主区又要滑回来）。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(FREEZE_MS - 100);
    });
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200);
    });
    // 落位：b 到 NEEDS ATTENTION 顶部。a 只是被挤着挪了一格，LIS 留下 a、c，只点亮真正跳了的 b。
    expect(rowOrder()).toEqual(["n:b", "n:a", "n:c"]);
    expect(sectioned()).toEqual(["section-attention", "row-n:b", "section-running", "row-n:a", "row-n:c"]);
    expect(rowLi("n:b").classList.contains("moved")).toBe(true);
    expect(rowLi("n:a").classList.contains("moved")).toBe(false);
    expect(rowLi("n:c").classList.contains("moved")).toBe(false);
    // 高亮只活一个落位周期：下一次列表更新（顺序没变）就清空，不会一直亮着。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:c", status: "running", source: "hook", reason: "activity", alive: true, progress: "Edit x" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(rowOrder()).toEqual(["n:b", "n:a", "n:c"]);
    expect(document.querySelectorAll("li.moved")).toHaveLength(0);
  });

  it("Alt/Option+2 during a freeze jumps to the second row as displayed (A51)", async () => {
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    pointerIntoSidebar();
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:c", status: "failed", source: "process", reason: "exited", alive: false }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    // 实时排序里 FAILED 的 c 早该是第 1 条；冻着的显示顺序仍是 a b c，序号也按显示顺序数。
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    expect(rowLi("n:a").getAttribute("data-ordinal")).toBe("1");
    expect(rowLi("n:b").getAttribute("data-ordinal")).toBe("2");
    // Alt/Option+2 打开的必须是**眼睛看到的**第 2 条 b，而不是实时排序的第 2 条 a。
    fireEvent.keyDown(window, { code: "Digit2", altKey: true });
    expect(screen.getByTestId("term-n:b")).toBeTruthy();
    expect(screen.getByTestId("crumb").textContent).toContain("b");
    // 跳转本身也算侧栏交互：跳完顺序仍然冻着，连按两次不会落到两行不同的会话上。
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
  });

  it("tree mode never freezes", async () => {
    const t = setup([row("n:a"), row("n:b"), row("n:c")]);
    await online(t);
    fireEvent.click(screen.getByTestId("sidebar-mode-tree"));
    pointerIntoSidebar();
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:c", status: "failed", source: "process", reason: "exited", alive: false }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    // 树视图按创建序，状态变了本来就不重排——这里真正要钉的是它**没有记下**一份冻结顺序。
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    // 指针还在侧栏里（frozen 仍为真）切回「需要我」：看到的必须是实时的 attention 顺序。
    // 树模式也走 stableOrder 的话，这里冻住的会是树的顺序，c 不会跳到最前。
    fireEvent.click(screen.getByTestId("sidebar-mode-attention"));
    expect(rowOrder()).toEqual(["n:c", "n:a", "n:b"]);
    // 刚切过来的第一帧不是"落位"，不该点亮任何行。
    expect(document.querySelectorAll("li.moved")).toHaveLength(0);
  });

  it("a row that becomes finished during a freeze stays in place and the FINISHED head keeps its count (A51)", async () => {
    // 2026-09-10 交叉验证点名的那条日常路径：逐个处理时点下一行，会把上一行写进 seen，
    // finishedCollapsed 立刻为真——而 onOpen 自己就调 touchSidebar 冻着顺序。只冻顺序不冻分段的话，
    // 这一行当场从 DOM 消失、FINISHED 表头插到列表中间、计数从 1 变成 3。
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const t = setup([
      { ...row("n:ext", "finished"), origin: "external" },
      { ...row("n:own", "finished"), origin: "agora" },
      row("n:run"),
      row("n:wait", "waiting"),
    ]);
    await online(t);
    expect(sectioned()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished"]);
    fireEvent.click(screen.getByTestId("row-n:own"));
    fireEvent.click(screen.getByTestId("row-n:wait"));
    // 冻结期间：own 还在原位、还在 DOM 里（「不许在冻结期间隐藏行」），三段表头与计数一个字没变。
    expect(sectioned()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished"]);
    expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 1");
    expect(rowLi("n:own").getAttribute("data-ordinal")).toBe("2");
    // 3 s 落位之后才收进折叠区，计数才跟着变。
    await settle();
    expect(sectioned()).toEqual(["section-attention", "row-n:wait", "section-running", "row-n:run", "section-finished"]);
    expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 2");
  });

  it("<StrictMode> still reports the rows that moved: prev order is written in the commit phase (A51)", async () => {
    // main.tsx 是 <StrictMode>，dev 下每次渲染跑两遍、effect 挂载 → 清理 → 再挂载。整条冻结 / 落位链路
    // （prev 的记与读、moved 的算与清）在双跑下必须照样出高亮。
    //
    // 实测边界（2026-09-10，React 19.2）：把 prev 挪回 useMemo 的 render 阶段写**不会**让这条变红——
    // StrictMode 双跑提交的是第一遍的结果，第二遍读到的毒值被丢掉（探针见 Workspace.tsx 那段注释）。
    // 这条钉的是链路本身，不是 ref 写在哪一阶段；别据此以为 render 阶段写 ref 是安全的。
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const t = setup([row("n:a"), row("n:b"), row("n:c")], [], undefined, undefined, undefined, true);
    await online(t);
    pointerIntoSidebar();
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(rowOrder()).toEqual(["n:a", "n:b", "n:c"]);
    pointerOutOfSidebar();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(FREEZE_MS + 1);
    });
    expect(rowOrder()).toEqual(["n:b", "n:a", "n:c"]);
    expect(rowLi("n:b").classList.contains("moved")).toBe(true);
    expect(rowLi("n:a").classList.contains("moved")).toBe(false);
  });
});

describe("Workspace · 看 diff（MISSION §6.3 看结果；A41，agora-h1k.5）", () => {
  it("看 diff swaps the main area to a read-only diff on the diff socket; closing it returns to the terminal, rows unchanged", async () => {
    const t = setup([row("n:a", "turn_done"), row("n:b")]);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    // 行展开：/changes 拉了一次，列表里有那个文件。
    expect(t.requests.filter((r) => r.url === "/api/sessions/n%3Aa/changes").length).toBe(1);
    expect(screen.getByTestId("changes-list-n:a").textContent).toContain("M a.txt");
    expect(mounted).toEqual(["n:a"]);
    expect(sockets).toEqual(["rw:default"]);

    fireEvent.click(screen.getByTestId("diff-n:a"));
    // 主区切成 diff：crumb 是 `git diff / a` 且没有 Settings；终端以 diff socket 只读挂载，会话终端已卸载。
    expect(screen.getByTestId("crumb-diff").textContent).toBe("git diff / a");
    expect(screen.queryByText("Settings")).toBeNull();
    expect(screen.getByTestId("term-diff-n:a")).toBeTruthy();
    expect(mounted).toEqual(["n:a", "n:a"]);
    expect(unmounted).toEqual(["n:a"]);
    expect(sockets).toEqual(["rw:default", "ro:defaultDiffSocket"]);
    // 侧栏没多一行：diff 不是会话；它的会话行仍是选中的（展开区还在，不重拉）。
    expect(screen.getAllByTestId(/^row-n:/).length).toBe(2);
    expect(screen.getByTestId("changes-list-n:a")).toBeTruthy();
    expect(t.requests.filter((r) => r.url === "/api/sessions/n%3Aa/changes").length).toBe(1);

    // 别的行变了（byId 变 → "行没了就清空"跑一遍）：a 还在，diff 视图留着。
    await act(async () => {
      t.sock.send([{ type: "status_changed", id: "n:b", status: "waiting", source: "hook", reason: "permission", alive: true }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.getByTestId("term-diff-n:a")).toBeTruthy();
    // 再点一次看 diff：不重挂（WS 不重连）。
    fireEvent.click(screen.getByTestId("diff-n:a"));
    expect(mounted).toEqual(["n:a", "n:a"]);

    // 「关闭 diff」：diff 终端卸载（= WS 关），回到会话终端；rows 不变；全程没有写请求。
    fireEvent.click(screen.getByTestId("close-diff"));
    expect(screen.queryByTestId("crumb-diff")).toBeNull();
    expect(screen.queryByTestId("term-diff-n:a")).toBeNull();
    expect(screen.getByTestId("term-n:a")).toBeTruthy();
    expect(unmounted).toEqual(["n:a", "n:a"]);
    expect(screen.getAllByTestId(/^row-n:/).length).toBe(2);
    await flush();
    expect(t.requests.filter((r) => r.method !== "GET")).toEqual([]);
  });

  it("clicking another row leaves the diff for that row's terminal; the diff goes away with its session", async () => {
    // session_removed 会让 store 重拉快照，所以快照里也得把 a 拿掉（同一个数组）。
    const rows = [row("n:a", "finished"), row("n:b", "finished")];
    const t = setup(rows);
    await online(t);
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    fireEvent.click(screen.getByTestId("diff-n:a"));
    expect(screen.getByTestId("term-diff-n:a")).toBeTruthy();
    // 点 b 行：主区是 b 的终端，a 的 diff 卸载（WS 关）。
    fireEvent.click(screen.getByTestId("row-n:b"));
    await flush();
    expect(screen.queryByTestId("term-diff-n:a")).toBeNull();
    expect(screen.getByTestId("term-n:b")).toBeTruthy();
    // 在看 b 的 diff 时再点 b 行：回到 b 的终端。
    fireEvent.click(screen.getByTestId("diff-n:b"));
    expect(screen.getByTestId("term-diff-n:b")).toBeTruthy();
    fireEvent.click(screen.getByTestId("row-n:b"));
    expect(screen.queryByTestId("term-diff-n:b")).toBeNull();
    expect(screen.getByTestId("term-n:b")).toBeTruthy();
    // 正在看 b 的 diff 时会话 b 被删：主区回到空。
    fireEvent.click(screen.getByTestId("diff-n:b"));
    rows.splice(1, 1);
    await act(async () => {
      t.sock.send([{ type: "session_removed", id: "n:b" }]);
      await new Promise((r) => setTimeout(r, 5));
    });
    expect(screen.queryByTestId("term-diff-n:b")).toBeNull();
    expect(screen.queryByTestId("crumb-diff")).toBeNull();
    expect(screen.getByText("从左侧选一个 agent。")).toBeTruthy();
  });
});
