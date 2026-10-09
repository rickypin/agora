/**
 * 全局一致性（agora-74nf）：颜色 / 圆角 / 字号只许在 `:root` 的令牌块里出现字面量，
 * 别处一律 `var()`。散着写的字面量正是"手机改了字号、桌面没人比对"的来路——用户 2026-10-08
 * 提的"UI 要有全局一致性"先落到这条可机检的规矩上；间距（--s-*）暂不进守卫（既有微调太多，
 * 硬包成令牌只是给数字改名，等真按档位改的时候再收）。
 */
import { describe, expect, it } from "vitest";
import { DEFAULT } from "./sidebarWidth";
import css from "./index.css?raw";

const rootMatch = css.match(/:root\s*\{[^}]*\}/);
const root = rootMatch?.[0] ?? "";
const rest = css.replace(root, "");

/** `:root` 里定义过 / 别处引用过的自定义属性。 */
const defined = new Set([...css.matchAll(/(--[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
const used = new Set([...css.matchAll(/var\((--[a-z0-9-]+)/g)].map((m) => m[1]));

describe("设计令牌（agora-74nf）", () => {
  it(":root 定义完整的语义色 / 圆角 / 字号 / 间距档", () => {
    const required = [
      "--bg", "--panel", "--panel-2", "--fg", "--muted", "--accent", "--border", "--warn", "--ok", "--bad",
      "--sel", "--danger-border", "--warn-bg", "--flash",
      "--r-1", "--r-2", "--r-3", "--r-4",
      "--fs-1", "--fs-2", "--fs-3", "--fs-4",
      "--s-1", "--s-2", "--s-3", "--s-4", "--s-5", "--s-6", "--s-7",
    ];
    for (const name of required) {
      expect(root, `${name} 要定义在 :root`).toContain(`${name}:`);
    }
    expect(root).toContain("font-size: var(--fs-3)");
  });

  it(":root 之外不再有颜色字面量", () => {
    const hexes = [...rest.matchAll(/#[0-9a-fA-F]{3,6}\b/g)].map((m) => m[0]);
    expect(hexes, `:root 外出现了颜色字面量：${hexes.join(", ")}；加进 :root 令牌再用 var()`).toEqual([]);
  });

  it(":root 之外不再有 px 字号与 px 圆角", () => {
    const fs = [...rest.matchAll(/font-size:\s*[\d.]+px/g)].map((m) => m[0]);
    expect(fs, `:root 外出现了 px 字号：${fs.join(", ")}；用 --fs-* 或 --m-fs 派生`).toEqual([]);
    const radii = [...rest.matchAll(/border-radius:\s*[\d.]+px/g)].map((m) => m[0]);
    expect(radii, `:root 外出现了 px 圆角：${radii.join(", ")}；用 --r-*`).toEqual([]);
  });

  it("引用的自定义属性都有定义（防拼错；--hue/--sidebar-w 由 JS 在行上设，例外）", () => {
    const runtime = new Set(["--hue", "--sidebar-w"]);
    const missing = [...used].filter((n) => !defined.has(n) && !runtime.has(n));
    expect(missing, `用了没定义的令牌：${missing.join(", ")}`).toEqual([]);
  });
});

describe("跨设备设计契约（agora-p7p0）", () => {
  it("CSS 原文确实加载，壳不能再分叉基础语义色", () => {
    expect(css.length).toBeGreaterThan(1000);
    expect(root).toContain("--on-accent:");
    expect(root).toContain(`--layout-sidebar: ${DEFAULT}px`);
    expect(rest).not.toMatch(/--(?:bg|panel|panel-2|fg|muted|accent|warn|ok|bad|sel|focus)\s*:/);
    expect(css).not.toContain("--mobile-accent");
    expect(rest).not.toMatch(/rgba?\(\s*\d/);
  });

  it("可阅读前景在各表面至少 4.5:1；焦点与输入边界至少 3:1", () => {
    const values = new Map([...root.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
    const value = (key: string): string => {
      const v = values.get(key) ?? "";
      const alias = v.match(/^var\((--[\w-]+)\)$/);
      return alias ? value(alias[1]) : v;
    };
    const luminance = (key: string) => {
      const hex = value(key);
      expect(hex, key).toMatch(/^#[\da-f]{6}$/i);
      const rgb = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
        .map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
      return rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
    };
    const contrast = (a: string, b: string) => {
      const x = luminance(a), y = luminance(b);
      return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
    };
    for (const bg of ["--bg", "--panel", "--panel-2", "--sel"]) {
      for (const fg of ["--fg", "--muted", "--accent", "--warn", "--bad", "--ok"]) {
        expect(contrast(fg, bg), `${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
      }
      expect(contrast("--focus", bg), `focus on ${bg}`).toBeGreaterThanOrEqual(3);
    }
    expect(contrast("--on-accent", "--accent")).toBeGreaterThanOrEqual(4.5);
    for (const bg of ["--bg", "--panel"]) expect(contrast("--control-border", bg)).toBeGreaterThanOrEqual(3);
  });

  it("键盘焦点是共享的，终端读同一份颜色和字体", async () => {
    expect(css).toContain(":focus-visible");
    expect(css).toContain("outline: 2px solid var(--focus)");
    const source = (await import("./TerminalView.tsx?raw")).default;
    for (const token of ["--terminal-bg", "--terminal-fg", "--terminal-font-size", "--font-mono"]) {
      expect(source).toContain(`getPropertyValue("${token}")`);
    }
    expect(source).not.toMatch(/(?:background|foreground):\s*"#/);
  });
});
