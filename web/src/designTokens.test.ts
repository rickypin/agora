/**
 * 全局一致性（agora-74nf）：颜色 / 圆角 / 字号只许在 `:root` 的令牌块里出现字面量，
 * 别处一律 `var()`。散着写的字面量正是"手机改了字号、桌面没人比对"的来路——用户 2026-10-08
 * 提的"UI 要有全局一致性"先落到这条可机检的规矩上；间距（--s-*）暂不进守卫（既有微调太多，
 * 硬包成令牌只是给数字改名，等真按档位改的时候再收）。
 */
import { describe, expect, it } from "vitest";
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
