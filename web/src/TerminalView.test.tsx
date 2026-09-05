// @vitest-environment jsdom
/**
 * TerminalView 的焦点纪律（agora-p29）：新开的浏览器标签页里点侧栏打开会话，挂载时那一次
 * term.focus() 偶尔不生效，键入不进 pane。守卫三条：① WS 收到 status:attached 之后再 focus 一次，
 * 不是只在挂载时；② 用户已经在别的输入框里打字时 attached 不抢焦点；③ term-host 上的 pointerdown
 * 把焦点交回 xterm 的 helper textarea。
 *
 * xterm 本体在 jsdom 里开不了（canvas）：这里换成一个只会造 helper textarea、数 focus 次数的替身；
 * WS 用假 socket 走真实的 TerminalClient 协议。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { TerminalSocketLike } from "./terminal";

const xterm = vi.hoisted(() => ({ focusCalls: 0 }));

vi.mock("@xterm/xterm", () => {
  class Terminal {
    cols = 80;
    rows = 24;
    private textarea: HTMLTextAreaElement | null = null;
    private root: HTMLDivElement | null = null;
    constructor(_opts: unknown) {}
    loadAddon(): void {}
    open(el: HTMLElement): void {
      // 与真 xterm 同构的最小 DOM：.xterm > .xterm-helper-textarea。
      this.root = document.createElement("div");
      this.root.className = "xterm";
      this.textarea = document.createElement("textarea");
      this.textarea.className = "xterm-helper-textarea";
      this.root.appendChild(this.textarea);
      el.appendChild(this.root);
    }
    focus(): void {
      xterm.focusCalls += 1;
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

import { TerminalView } from "./TerminalView";

class FakeSocket implements TerminalSocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  sent: string[] = [];
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {}
  /** 服务端来一帧。 */
  frame(f: unknown): void {
    this.onmessage?.({ data: JSON.stringify(f) });
  }
}

let sock: FakeSocket;
// 稳定引用：TerminalView 把 connect 定在挂载时，内联闭包也行，但这里明说它不随渲染变。
const connect = () => sock;

beforeAll(() => {
  // jsdom 没有 ResizeObserver；TerminalView 用它跟着容器 fit。
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
});

afterEach(() => {
  cleanup();
  xterm.focusCalls = 0;
});

function helper(): HTMLTextAreaElement {
  return document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
}

function setup() {
  sock = new FakeSocket();
  const ui = render(<TerminalView sessionId="n:a" connect={connect} />);
  return { ui, host: ui.container.querySelector(".term-host") as HTMLDivElement };
}

async function attached() {
  await act(async () => {
    sock.onopen?.({});
    sock.frame({ type: "status", status: "attached" });
  });
}

describe("TerminalView focus (agora-p29)", () => {
  it("focuses once on mount and again when the WS reports attached", async () => {
    setup();
    expect(xterm.focusCalls).toBe(1);
    expect(document.activeElement).toBe(helper());
    // 挂载时的 focus 没生效（新标签页里 document 还没拿到焦点）——模拟成焦点落回 body。
    helper().blur();
    expect(document.activeElement).toBe(document.body);
    await attached();
    expect(screen.getByText("已连接")).toBeTruthy();
    expect(xterm.focusCalls).toBe(2);
    expect(document.activeElement).toBe(helper());
  });

  it("does not steal focus from a text field the user is already typing in", async () => {
    setup();
    const filter = document.createElement("input");
    document.body.appendChild(filter);
    filter.focus();
    expect(document.activeElement).toBe(filter);
    await attached();
    expect(document.activeElement).toBe(filter);
    expect(xterm.focusCalls).toBe(1); // 只有挂载那一次
    filter.remove();
  });

  it("pointerdown anywhere on the term-host hands focus back to xterm's helper textarea", () => {
    const { host } = setup();
    helper().blur();
    expect(document.activeElement).toBe(document.body);
    fireEvent.pointerDown(host);
    expect(document.activeElement).toBe(helper());
    // 点在 padding（target 就是 host 本身）：浏览器缺省的 mousedown 会把焦点挪走，必须被取消。
    const md = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    host.dispatchEvent(md);
    expect(md.defaultPrevented).toBe(true);
    // 点在 .xterm 里：让 xterm 自己处理（它的 mousedown 才管选区），这里不取消。
    const inner = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    helper().dispatchEvent(inner);
    expect(inner.defaultPrevented).toBe(false);
  });

  it("unmount detaches: removes the listeners along with the terminal", () => {
    const { ui, host } = setup();
    ui.unmount();
    // 卸载后再点已经没人接：不抛、也不去 focus 一个已经 dispose 的终端。
    const before = xterm.focusCalls;
    fireEvent.pointerDown(host);
    expect(xterm.focusCalls).toBe(before);
  });
});
