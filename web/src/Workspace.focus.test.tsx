// @vitest-environment jsdom
/**
 * 点已激活的侧栏行 / 标签页后焦点要回到终端（agora-vcc，agora-p29 的剩余一角）：Tab reducer 对
 * 已激活的 id 是 no-op，TerminalView 不重挂也不会再 focus，按钮被点击天然拿到焦点，键入就进不了
 * pane、Enter 还会再点一次。这里用真 Workspace + 真 TerminalView，xterm 换成 TerminalView.test 同款
 * 替身（只造 .xterm > helper textarea），终端 WS 用不会连的假 socket。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import type { SessionRow, SocketLike } from "./events";
import { HealthWatcher } from "./health";
import type { NotificationLike, NotifierDeps } from "./notify";
import { SessionStore } from "./store";
import type { TerminalSocketLike } from "./terminal";
import { Workspace } from "./Workspace";

vi.mock("@xterm/xterm", () => {
  class Terminal {
    cols = 80;
    rows = 24;
    private textarea: HTMLTextAreaElement | null = null;
    private root: HTMLDivElement | null = null;
    constructor(_opts: unknown) {}
    loadAddon(): void {}
    open(el: HTMLElement): void {
      this.root = document.createElement("div");
      this.root.className = "xterm";
      this.textarea = document.createElement("textarea");
      this.textarea.className = "xterm-helper-textarea";
      this.root.appendChild(this.textarea);
      el.appendChild(this.root);
    }
    focus(): void {
      this.textarea?.focus();
    }
    write(): void {}
    onData(): { dispose(): void } {
      return { dispose() {} };
    }
    onResize(): { dispose(): void } {
      return { dispose() {} };
    }
    attachCustomKeyEventHandler(): void {}
    dispose(): void {
      this.root?.remove();
    }
  }
  return { Terminal };
});
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit(): void {}
  },
}));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

class FakeEventsSocket implements SocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  close(): void {}
  send(events: unknown[]): void {
    this.onmessage?.({ data: JSON.stringify(events) });
  }
}

/**
 * 终端 WS 替身：默认不连；agora-y3h 那条要模拟 attached 到达后再 focus 一次，
 * 所以暴露 frame / onopen，由测试在面板聚焦之后自己推一帧。
 */
class FakeTermSocket implements TerminalSocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  send(): void {}
  close(): void {}
  frame(f: unknown): void {
    this.onmessage?.({ data: JSON.stringify(f) });
  }
}

function row(id: string): SessionRow {
  return { id, node: "n", status: "running", alive: true, display_name: id.slice(2), agent_type: "claude", reason: null, respond_via: "hook" };
}

const notify: NotifierDeps = {
  permission: () => "denied",
  request: async () => "denied",
  create: () => ({ onclick: null, close() {} }),
  focus: () => {},
};

function setup(rows: SessionRow[], notifyDeps: NotifierDeps = notify) {
  const sock = new FakeEventsSocket();
  const termSocks: FakeTermSocket[] = [];
  const store = new SessionStore({ connect: () => sock, fetchSnapshot: async () => ({ sessions: rows, unregistered: [] }), coalesceMs: 0 });
  const f: FetchLike = async () => new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
  render(
    <Workspace
      store={store}
      api={sessionApi(f)}
      catalog={catalogApi(f)}
      notifyDeps={notifyDeps}
      health={new HealthWatcher({ fetchHealth: async () => ({ status: "ok", runtime: { status: "ok", reason: null } }) })}
      terminalConnect={() => {
        const s = new FakeTermSocket();
        termSocks.push(s);
        return s;
      }}
    />,
  );
  return { sock, termSocks };
}

/** WS 报告 attached：TerminalView 会再 focus 一次（agora-p29 的补交）。 */
async function fireAttached(sock: FakeTermSocket) {
  await act(async () => {
    sock.onopen?.({});
    sock.frame({ type: "status", status: "attached" });
  });
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

beforeAll(() => {
  // jsdom 没有 ResizeObserver；TerminalView 用它跟着容器 fit。
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe(): void {}
    disconnect(): void {}
  };
});

afterEach(cleanup);

/** 焦点在 xterm 的 helper textarea 上。 */
function focusInTerminal(): boolean {
  return document.activeElement?.closest(".xterm") != null;
}

it("点通知落到另一行：焦点进回答面板的输入框，不被新终端挂载时的那次 focus 盖掉（A50，agora-4yr.1）", async () => {
  // 这条只能在"终端替身真的会 focus"的这个文件里成立（Workspace.test.tsx 的 TerminalView 是个
  // 不 focus 的 div，那边四条焦点守卫对本 bug 全绿）。通知点击是「选中这一行」+「聚焦它的面板」
  // 同一批 state 更新：面板与 TerminalView 在同一次 commit 挂载，面板在树里靠前、effect 先跑，
  // 终端挂载 effect 末尾那句 term.focus() 后跑——面板不把聚焦推迟一个宏任务就会被盖掉
  // （2026-09-10 agent-browser 代检实测：crumb 换了、activeElement 还是 xterm 的 helper textarea）。
  const created: NotificationLike[] = [];
  const granted: NotifierDeps = {
    permission: () => "granted",
    request: async () => "granted",
    create: () => {
      const n: NotificationLike = { onclick: null, close() {} };
      created.push(n);
      return n;
    },
    focus: () => {},
  };
  const t = setup([row("n:a"), row("n:b")], granted);
  await act(async () => {
    t.sock.onopen?.({});
  });
  await flush();
  fireEvent.click(screen.getByTestId("row-n:a"));
  await flush();
  expect(focusInTerminal()).toBe(true);
  await act(async () => {
    t.sock.send([
      { type: "session_updated", id: "n:b", session: { ...row("n:b"), status: "turn_done", detail: "Two files." } },
      { type: "notification", id: "n:b", title: "Claude / b @ n is done", body: "Two files.", status: "turn_done" },
    ]);
  });
  await flush();
  expect(created.length).toBe(1);
  await act(async () => {
    created[0]!.onclick?.({});
  });
  await flush();
  expect(screen.getByTestId("crumb").textContent).toContain("b /");
  expect(document.activeElement).toBe(screen.getByTestId("next-input"));
  expect(focusInTerminal()).toBe(false);
});

it("点通知落到尚未打开的 WAITING 行：焦点进 Allow 按钮，不被 attached 之后那次 focus 盖掉（agora-y3h）", async () => {
  // TURN_DONE 那条只赢挂载时的 term.focus()——它落在 <input>，旧 typingElsewhere 护得住。
  // WAITING 落在 Allow <button>，必须让替身在面板聚焦之后再走一次 attached 补交，才断得出
  // 「button 也算人正在别处操作」。顺序不能反：先 attached 再 flush，面板的 setTimeout(0)
  // 会把 Allow 盖回去，这条就会在没修 typingElsewhere 时也绿。
  const created: NotificationLike[] = [];
  const granted: NotifierDeps = {
    permission: () => "granted",
    request: async () => "granted",
    create: () => {
      const n: NotificationLike = { onclick: null, close() {} };
      created.push(n);
      return n;
    },
    focus: () => {},
  };
  const t = setup([row("n:a"), row("n:b")], granted);
  await act(async () => {
    t.sock.onopen?.({});
  });
  await flush();
  fireEvent.click(screen.getByTestId("row-n:a"));
  await flush();
  expect(focusInTerminal()).toBe(true);
  await act(async () => {
    t.sock.send([
      {
        type: "session_updated",
        id: "n:b",
        session: {
          ...row("n:b"),
          status: "waiting",
          source: "hook",
          reason: "permission",
          detail: "Bash: echo hi",
          pending_decision: { request_id: "req-b", summary: "Bash: echo hi", epoch: 1 },
        },
      },
      { type: "notification", id: "n:b", title: "Claude / b @ n needs you", body: "Bash: echo hi", status: "waiting" },
    ]);
  });
  await flush();
  expect(created.length).toBe(1);
  await act(async () => {
    created[0]!.onclick?.({});
  });
  await flush();
  expect(screen.getByTestId("crumb").textContent).toContain("b /");
  expect(document.activeElement).toBe(screen.getByTestId("allow"));
  const term = t.termSocks[t.termSocks.length - 1];
  expect(term).toBeTruthy();
  await fireAttached(term!);
  expect(document.activeElement).toBe(screen.getByTestId("allow"));
  expect(focusInTerminal()).toBe(false);
});

describe("点已激活的行 / 标签页后焦点回到终端（agora-vcc）", () => {
  it("再点一次已激活的侧栏行，activeElement 回到 .xterm 内", async () => {
    const t = setup([row("n:a"), row("n:b")]);
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    const rowA = screen.getByTestId("row-n:a");
    fireEvent.click(rowA);
    await flush();
    expect(focusInTerminal()).toBe(true); // 首次打开：挂载时 focus（p29 修好的路径）

    // 真浏览器里按钮在 mousedown 就拿到焦点；jsdom 的 click 不挪焦点，手工模拟这一步。
    rowA.focus();
    expect(focusInTerminal()).toBe(false);
    fireEvent.click(rowA);
    expect(focusInTerminal()).toBe(true);
  });

  it("在看它的 diff 时再点这一行：回到终端，焦点在终端（重挂载的 focus）", async () => {
    const t = setup([row("n:a"), row("n:b")]);
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    const rowA = screen.getByTestId("row-n:a");
    fireEvent.click(rowA);
    await flush();
    fireEvent.click(screen.getByTestId("diff-n:a"));
    await flush();
    expect(screen.getByTestId("crumb-diff")).toBeTruthy();
    rowA.focus();
    fireEvent.click(rowA);
    await flush();
    expect(screen.queryByTestId("crumb-diff")).toBeNull();
    expect(focusInTerminal()).toBe(true);
  });

  it("切到别的会话再切回来仍走重挂载的 focus；点未激活的行不重复交焦点", async () => {
    const t = setup([row("n:a"), row("n:b")]);
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    const rowB = screen.getByTestId("row-n:b");
    rowB.focus();
    fireEvent.click(rowB);
    await flush();
    expect(screen.getByTestId("crumb").textContent).toContain("b /");
    expect(focusInTerminal()).toBe(true);
  });
});
