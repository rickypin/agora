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
import type { NotifierDeps } from "./notify";
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
}

/** 永远连不上的终端 WS：这里只关心焦点，不关心协议。 */
class FakeTermSocket implements TerminalSocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  send(): void {}
  close(): void {}
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

function setup(rows: SessionRow[]) {
  const sock = new FakeEventsSocket();
  const store = new SessionStore({ connect: () => sock, fetchSnapshot: async () => ({ sessions: rows, unregistered: [] }), coalesceMs: 0 });
  const f: FetchLike = async () => new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
  render(
    <Workspace
      store={store}
      api={sessionApi(f)}
      catalog={catalogApi(f)}
      notifyDeps={notify}
      health={new HealthWatcher({ fetchHealth: async () => ({ status: "ok", runtime: { status: "ok", reason: null } }) })}
      terminalConnect={() => new FakeTermSocket()}
    />,
  );
  return { sock };
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

  it("点已激活的标签页同理", async () => {
    const t = setup([row("n:a"), row("n:b")]);
    await act(async () => {
      t.sock.onopen?.({});
    });
    await flush();
    fireEvent.click(screen.getByTestId("row-n:a"));
    await flush();
    const tab = screen.getByTestId("tab-n:a");
    tab.focus();
    expect(focusInTerminal()).toBe(false);
    fireEvent.click(tab);
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
    expect(screen.getByTestId("tab-n:b").closest("[role=tab]")?.getAttribute("aria-selected")).toBe("true");
    expect(focusInTerminal()).toBe(true);
  });
});
