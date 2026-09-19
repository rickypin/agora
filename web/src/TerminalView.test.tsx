// @vitest-environment jsdom
/**
 * TerminalView 的焦点纪律（agora-p29）：新开的浏览器标签页里点侧栏打开会话，挂载时那一次
 * term.focus() 偶尔不生效，键入不进 pane。守卫三条：① WS 收到 status:attached 之后再 focus 一次，
 * 不是只在挂载时；② 用户已经在别处操作可交互控件时 attached 不抢焦点（input 与 button 都算，agora-y3h）；③ term-host 上的 pointerdown
 * 把焦点交回 xterm 的 helper textarea。
 *
 * xterm 本体在 jsdom 里开不了（canvas）：这里换成一个只会造 helper textarea、数 focus 次数的替身；
 * WS 用假 socket 走真实的 TerminalClient 协议。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { TerminalSocketLike } from "./terminal";

const xterm = vi.hoisted(() => ({
  focusCalls: 0,
  /** TerminalView 装上的 custom key handler（agora-hhu 的平台守卫要直接调它）。 */
  keyHandler: null as ((ev: KeyboardEvent) => boolean) | null,
}));

vi.mock("@xterm/xterm", () => {
  class Terminal {
    cols = 80;
    rows = 24;
    /** 真 xterm 也把 helper textarea 公开成 `textarea`（TerminalView 在上面挂 input 监听）。 */
    textarea: HTMLTextAreaElement | null = null;
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
    attachCustomKeyEventHandler(h: (ev: KeyboardEvent) => boolean): void {
      xterm.keyHandler = h;
    }
    dispose(): void {
      this.root?.remove();
      xterm.keyHandler = null;
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
import { runtimeSessionGone } from "./terminal";

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

  it("does not steal focus from a button the user is already interacting with (agora-y3h)", async () => {
    // WAITING 面板聚焦的是 Allow <button>。旧 typingElsewhere 只认文本输入，attached 会把
    // 焦点抢回 helper textarea——点通知落到未打开的 WAITING 行就是这条路径。
    setup();
    const allow = document.createElement("button");
    allow.textContent = "Allow";
    document.body.appendChild(allow);
    allow.focus();
    expect(document.activeElement).toBe(allow);
    await attached();
    expect(document.activeElement).toBe(allow);
    expect(xterm.focusCalls).toBe(1); // 只有挂载那一次；补交被挡住，agora-p29 那次仍在
    allow.remove();
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

/**
 * Option/Alt+←/→ 词跳的平台判断（agora-hhu）：keys.ts 不读 navigator，靠 TerminalView 挂载时按
 * navigator.platform 传进来——传错了（或忘了传）mac 用户就会收到 zsh 不认的 ESC[1;5D。keys.test.ts
 * 守字节本身，这里守"TerminalView 把对的平台传给了它"。jsdom 的 navigator.platform 是 ""，两边都靠
 * 在实例上定义属性来假扮。
 */
describe("TerminalView passes the browser platform to the key layer (agora-hhu)", () => {
  function pretendPlatform(platform: string) {
    Object.defineProperty(navigator, "platform", { value: platform, configurable: true });
  }
  afterEach(() => {
    // 删掉实例属性，回到 jsdom 原型上的 getter。
    delete (navigator as unknown as { platform?: string }).platform;
  });

  function altArrow(k: "ArrowLeft" | "ArrowRight"): KeyboardEvent {
    return new KeyboardEvent("keydown", { key: k, altKey: true, cancelable: true });
  }

  it.each([
    ["MacIntel", "\x1bb", "\x1bf"],
    ["iPad", "\x1bb", "\x1bf"],
    ["Linux x86_64", "\x1b[1;5D", "\x1b[1;5C"],
    ["Win32", "\x1b[1;5D", "\x1b[1;5C"],
  ])("on %s Alt+←/→ sends %j / %j through the WS", async (platform, left, right) => {
    pretendPlatform(platform);
    setup();
    await attached();
    const handler = xterm.keyHandler;
    expect(handler).not.toBeNull();
    sock.sent.length = 0;
    const l = altArrow("ArrowLeft");
    expect(handler!(l)).toBe(false);
    expect(l.defaultPrevented).toBe(true);
    const r = altArrow("ArrowRight");
    expect(handler!(r)).toBe(false);
    expect(sock.sent).toEqual([JSON.stringify({ type: "input", data: left }), JSON.stringify({ type: "input", data: right })]);
  });

  it("IME punctuation: the keypress is left to the browser and the character the IME actually inserts is sent (agora-n0l)", async () => {
    pretendPlatform("MacIntel");
    setup();
    await attached();
    sock.sent.length = 0;
    // macOS Chrome + 中文输入法（2026-09-08 控制台实测）：keydown 的 key 是半角 `,`，xterm 6.0 会在
    // 这里就发字节并 preventDefault，输入法随后 insertText 的 `，` 就没了。这一层放行 keydown 与 keypress……
    for (const type of ["keydown", "keypress"]) {
      const ev = new KeyboardEvent(type, { key: ",", cancelable: true });
      expect(xterm.keyHandler!(ev)).toBe(false);
      expect(ev.defaultPrevented).toBe(false);
    }
    expect(sock.sent).toEqual([]);
    // ……再从 textarea 的 input 事件里发输入法真正插入的字符。
    const ta = helper();
    ta.dispatchEvent(new InputEvent("input", { data: "，", inputType: "insertText", bubbles: true }));
    expect(sock.sent).toEqual([JSON.stringify({ type: "input", data: "，" })]);
    // 组合中的 CJK 正文归 xterm 的 CompositionHelper，这里不重复发。
    ta.dispatchEvent(new InputEvent("input", { data: "中", inputType: "insertCompositionText", bubbles: true }));
    ta.dispatchEvent(new InputEvent("input", { data: "中", inputType: "insertText", bubbles: true }));
    expect(sock.sent).toHaveLength(1);
  });
});


/**
 * 运行时会话已不在的行（agora-u5p，Mac 2026-09-18 现场 A2）：终端那一格过去画一个永远连不上的
 * 「重新连接」——那一行没有 pane 可连，按下去只有一次 `no sessions`。现在画文案 + Restart / 删除记录，
 * 并且根本不去建那条 WS。
 */
describe("TerminalView when the runtime session is gone (agora-u5p)", () => {
  const goneActions = { onRestart: vi.fn(), onRemove: vi.fn() };

  function setupGone(props: { runtimeGone?: { onRestart: () => void; onRemove: () => void; busy?: boolean; note?: string | null } | null; readOnly?: boolean } = {}) {
    sock = new FakeSocket();
    const connectSpy = vi.fn(() => sock as unknown as TerminalSocketLike);
    const ui = render(
      <TerminalView
        sessionId="n:a"
        connect={connectSpy}
        readOnly={props.readOnly}
        runtimeGone={props.runtimeGone === undefined ? goneActions : props.runtimeGone}
      />,
    );
    return { ui, connectSpy };
  }

  it("recognizes the row by status + reason, not by a string match on the whole sentence", () => {
    expect(runtimeSessionGone({ status: "finished", reason: "runtime session gone (server gone; no exit status)" })).toBe(true);
    expect(runtimeSessionGone({ status: "finished", reason: "killed by user (runtime session gone; session gone)" })).toBe(true);
    // 对照：活着的行、别的结束原因、以及还没判出状态的一行都不算。
    expect(runtimeSessionGone({ status: "running", reason: null })).toBe(false);
    expect(runtimeSessionGone({ status: "finished", reason: "exit code 1" })).toBe(false);
    expect(runtimeSessionGone({ status: "unknown", reason: "runtime unavailable: protocol version mismatch" })).toBe(false);
    expect(runtimeSessionGone(null)).toBe(false);
  });

  it("shows the fact instead of a reconnect button and never opens the WS", () => {
    const { connectSpy } = setupGone();
    expect(screen.getByTestId("term-link").textContent).toBe("运行时会话已不在");
    expect(screen.queryByTestId("reconnect")).toBeNull();
    expect(connectSpy).not.toHaveBeenCalled();
    expect(screen.getByTestId("gone-restart").textContent).toBe("Restart");
    expect(screen.getByTestId("gone-remove").textContent).toBe("删除记录");
  });

  it("the two buttons are the way out", () => {
    setupGone();
    fireEvent.click(screen.getByTestId("gone-restart"));
    fireEvent.click(screen.getByTestId("gone-remove"));
    expect(goneActions.onRestart).toHaveBeenCalledOnce();
    expect(goneActions.onRemove).toHaveBeenCalledOnce();
  });

  it("says why an attempt failed and goes idle while one is running", () => {
    setupGone({ runtimeGone: { ...goneActions, busy: true, note: "session_gone: 节点说不了" } });
    expect(screen.getByTestId("gone-note").textContent).toContain("session_gone");
    expect((screen.getByTestId("gone-restart") as HTMLButtonElement).disabled).toBe(true);
  });

  it("reconnects once the row comes back to life (Restart 之后不必切行走一圈)", async () => {
    sock = new FakeSocket();
    const connectSpy = vi.fn(() => sock as unknown as TerminalSocketLike);
    const ui = render(
      <TerminalView sessionId="n:a" connect={connectSpy} runtimeGone={goneActions} />,
    );
    expect(connectSpy).not.toHaveBeenCalled();
    // Restart 成功、事件流把行推回 alive：Workspace 那一侧就不再传 runtimeGone。
    ui.rerender(<TerminalView sessionId="n:a" connect={connectSpy} runtimeGone={null} />);
    expect(connectSpy).toHaveBeenCalledOnce();
    await act(async () => {
      sock.onopen?.({});
      sock.frame({ type: "status", status: "attached" });
    });
    expect(screen.getByTestId("term-link").textContent).toBe("已连接");
    expect(screen.queryByTestId("gone-restart")).toBeNull();
  });

  it("leaves the read-only diff terminal alone: its button is 重新运行", () => {
    const { connectSpy } = setupGone({ readOnly: true });
    // 只读终端连的是 git diff 那条 WS，与这一行的运行时会话在不在无关：它照常连。
    expect(connectSpy).toHaveBeenCalledOnce();
    act(() => {
      sock.onmessage?.({ data: JSON.stringify({ type: "exit", exit: { kind: "code", value: 0 } }) });
    });
    // 退出后的出口是「重新运行」（再跑一次 git diff），不是被 runtimeGone 换成 Restart。
    expect(screen.getByTestId("rerun").textContent).toBe("重新运行");
    expect(screen.queryByTestId("gone-restart")).toBeNull();
  });
});
