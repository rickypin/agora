import { describe, expect, it } from "vitest";
import {
  handleTerminalKey,
  isDesktop,
  matchShortcut,
  terminalKey,
  TERMINAL_CTRL_KEYS,
  type KeyLike,
  type Preventable,
} from "./keys";

function key(k: string, mods: Partial<KeyLike> = {}): KeyLike {
  return { key: k, ctrlKey: false, metaKey: false, shiftKey: false, altKey: false, type: "keydown", ...mods };
}

/** 带 preventDefault 记账的事件（xterm 的 custom key handler 收到的就是这形状）。 */
function evt(k: string, mods: Partial<KeyLike> = {}): Preventable & { prevented: number } {
  const e = { ...key(k, mods), prevented: 0, preventDefault: () => void e.prevented++ };
  return e;
}

describe("终端键位保真（agora-xqa.3）", () => {
  it("Shift+Enter 发 ESC CR，不是裸 CR", () => {
    // 裸 CR 与 Enter 无法区分，TUI 一按就提交——这条守卫在的意义就是不许再发 \r。
    expect(terminalKey(key("Enter", { shiftKey: true }))).toBe("\x1b\r");
    expect(terminalKey(key("Enter", { shiftKey: true }))).not.toBe("\r");
  });

  it("光秃秃的 Enter 与 Ctrl/Alt+Enter 一律交回 xterm", () => {
    expect(terminalKey(key("Enter"))).toBeNull();
    expect(terminalKey(key("Enter", { shiftKey: true, ctrlKey: true }))).toBeNull();
    expect(terminalKey(key("Enter", { shiftKey: true, altKey: true }))).toBeNull();
  });

  it("Cmd+←/→ 变 Home/End，并且拦下浏览器的历史后退", () => {
    const left = evt("ArrowLeft", { metaKey: true });
    let sent = "";
    expect(handleTerminalKey(left, (d) => (sent += d))).toBe(false);
    expect(sent).toBe("\x1b[H");
    expect(left.prevented).toBe(1);

    const right = evt("ArrowRight", { metaKey: true });
    sent = "";
    expect(handleTerminalKey(right, (d) => (sent += d))).toBe(false);
    expect(sent).toBe("\x1b[F");
    expect(right.prevented).toBe(1);
  });

  it("Option+←/→ 按词跳仍归 xterm（实测通过项不能回退）", () => {
    for (const k of ["ArrowLeft", "ArrowRight"]) {
      const e = evt(k, { altKey: true });
      expect(handleTerminalKey(e, () => expect.unreachable("这一层不该发字节"))).toBe(true);
      expect(e.prevented).toBe(0);
    }
  });

  it("keypress 不重复处理（xterm 的 handler 两种事件都会调）", () => {
    expect(terminalKey({ ...key("Enter", { shiftKey: true }), type: "keypress" })).toBeNull();
  });
});

describe("全局快捷键不吞终端的 Ctrl 键（agora-xqa.14 / MISSION §6.5 硬约束）", () => {
  it.each([...TERMINAL_CTRL_KEYS])("Ctrl+%s 既不匹配快捷键，也不被终端层拦下", (k) => {
    expect(matchShortcut(key(k, { ctrlKey: true }))).toBeNull();
    const e = evt(k, { ctrlKey: true });
    expect(handleTerminalKey(e, () => expect.unreachable("Ctrl 组合原样交给 xterm"))).toBe(true);
    expect(e.prevented).toBe(0);
  });

  it("大写（配了 Shift 或 CapsLock）也一样不认", () => {
    expect(matchShortcut(key("C", { ctrlKey: true }))).toBeNull();
  });
});

describe("终端聚焦时全局快捷键也按得动（agora-82g）", () => {
  it("Alt/Option+K / F 开面板、聚焦过滤，认 code 不认 key", () => {
    // macOS 上 Option+K 的 key 是 `˚`、Option+F 是 `ƒ`。
    expect(matchShortcut(key("˚", { altKey: true, code: "KeyK" }))).toEqual({ action: "palette" });
    expect(matchShortcut(key("ƒ", { altKey: true, code: "KeyF" }))).toEqual({ action: "filter" });
    expect(matchShortcut(key("k", { altKey: true, code: "KeyK", ctrlKey: true }))).toBeNull();
  });

  it("终端层把 Cmd 系与 Alt/Option 系的快捷键让给全局层：不发字节、不 preventDefault、返回 false", () => {
    // xterm 收到 false 就不 cancel 事件，keydown 照常冒泡到 window，全局层自己 preventDefault。
    const deferred = [
      evt("k", { metaKey: true }),
      evt("f", { metaKey: true }),
      evt("˚", { altKey: true, code: "KeyK" }),
      evt("ƒ", { altKey: true, code: "KeyF" }),
      evt("£", { altKey: true, code: "Digit3" }),
      evt("‘", { altKey: true, code: "BracketRight" }),
      evt("Dead", { altKey: true, code: "KeyN" }),
    ];
    for (const e of deferred) {
      expect(handleTerminalKey(e, () => expect.unreachable("这一层不该发字节"))).toBe(false);
      expect(e.prevented).toBe(0);
    }
  });

  it("终端聚焦时 Ctrl+K / Ctrl+F 归 pane（kill-line、vim / less 翻页），不进面板", () => {
    for (const k of ["k", "f"]) {
      const e = evt(k, { ctrlKey: true });
      expect(handleTerminalKey(e, () => expect.unreachable("Ctrl 组合原样交给 xterm"))).toBe(true);
      expect(e.prevented).toBe(0);
    }
    // 焦点不在终端时 Ctrl+K / F 仍是面板与过滤（全局层直接收到）。
    expect(matchShortcut(key("k", { ctrlKey: true }))).toEqual({ action: "palette" });
  });

  it("不是快捷键的普通键与 Alt+←/→ 仍归 xterm", () => {
    for (const e of [evt("a"), evt("Enter"), evt("ArrowLeft", { altKey: true }), evt("x", { altKey: true, code: "KeyX" })]) {
      expect(handleTerminalKey(e, () => expect.unreachable("这一层不该发字节"))).toBe(true);
    }
  });
});

describe("键位表（docs/spec/ux.md）", () => {
  it("Cmd/Ctrl+K / F", () => {
    expect(matchShortcut(key("k", { metaKey: true }))).toEqual({ action: "palette" });
    expect(matchShortcut(key("k", { ctrlKey: true }))).toEqual({ action: "palette" });
    expect(matchShortcut(key("f", { metaKey: true }))).toEqual({ action: "filter" });
    expect(matchShortcut(key("f", { ctrlKey: true }))).toEqual({ action: "filter" });
  });

  it("Alt/Option+] / [ 上下一个、Alt/Option+N 新建，认 code 不认 key", () => {
    // macOS 上 Option+] 的 key 是 `‘`、Option+N 是死键 `Dead`：认 key 的实现一条都按不动。
    expect(matchShortcut(key("‘", { altKey: true, code: "BracketRight" }))).toEqual({ action: "next" });
    expect(matchShortcut(key("“", { altKey: true, code: "BracketLeft" }))).toEqual({ action: "prev" });
    expect(matchShortcut(key("Dead", { altKey: true, code: "KeyN" }))).toEqual({ action: "new" });
  });

  it("浏览器自己的加速键 agora 一律不认（agora-rzn）", () => {
    // macOS Chrome 人眼实测 2026-09-04：Cmd+Shift+]/[ 是下/上一个标签页、Cmd+N 是新窗口，
    // 在浏览器 UI 层就被吃掉，页面收不到 keydown——绑了也白绑，还会骗后来者以为有这个键。
    // Linux/Windows 上 Ctrl+Shift+]/[ 与 Ctrl+N 同理。
    for (const mods of [{ metaKey: true }, { ctrlKey: true }]) {
      expect(matchShortcut(key("}", { ...mods, shiftKey: true, code: "BracketRight" }))).toBeNull();
      expect(matchShortcut(key("{", { ...mods, shiftKey: true, code: "BracketLeft" }))).toBeNull();
      expect(matchShortcut(key("n", { ...mods, code: "KeyN" }))).toBeNull();
    }
  });

  it("Alt/Option+1…9 跳转，认 code 不认 key", () => {
    // Option+3 在 macOS 上 key 是 `£`。认 key 的实现在 mac 上一条都跳不了。
    expect(matchShortcut(key("£", { altKey: true, code: "Digit3" }))).toEqual({ action: "jump", index: 2 });
    expect(matchShortcut(key("1", { altKey: true, code: "Digit1" }))).toEqual({ action: "jump", index: 0 });
    expect(matchShortcut(key("0", { altKey: true, code: "Digit0" }))).toBeNull();
  });

  it("Cmd/Ctrl+数字被浏览器保留，agora 不抢", () => {
    expect(matchShortcut(key("1", { metaKey: true, code: "Digit1" }))).toBeNull();
    expect(matchShortcut(key("1", { ctrlKey: true, code: "Digit1" }))).toBeNull();
  });

  it("没带修饰键的普通输入一概不认", () => {
    for (const k of ["k", "f", "n", "1", "Enter", "a"]) expect(matchShortcut(key(k))).toBeNull();
  });
});

describe("环境", () => {
  // TerminalView 有没有把 handleTerminalKey 装到 xterm 上，由 tests/arch_boundary.rs 扫源码守（xterm 在 jsdom 里开不起来）。
  it("没有 window 的环境（node）当桌面处理", () => {
    expect(isDesktop()).toBe(true);
  });
});
