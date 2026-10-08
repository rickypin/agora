/**
 * 手机端 CSS 的不变式（agora-x70t）。有些事在 jsdom 里测不出来（没有布局引擎），但它们恰恰是
 * iPhone 上真出过问题的那几条（2026-10-08 用户反馈 + agent-browser 计算样式量测），所以在这里
 * 把 CSS 当数据来钉：pwa.test.ts / sw.test.ts 已经在测"浏览器里跑的那份字节"，这里测的就是
 * index.html 与 index.css 的那几个字节。
 */
import { describe, expect, it } from "vitest";
// `?raw` 是 Vite 的导入（tsconfig 的 types 里有 vite/client）：测的就是构建时打进 bundle 的那份字节，
// 不引入 node:fs（web 的 tsconfig 不装 @types/node，也不该为一条测试装）。
import css from "./index.css?raw";
import html from "../index.html?raw";
import { DEFAULT_MOBILE_TEXT_SIZE, MOBILE_TEXT_SIZES } from "./mobileText";

/** 所有 `selector { body }`（@media 的内层规则也按同样形状取出来；这个文件里没有更深的嵌套）。 */
function rules(): Array<{ sel: string; body: string }> {
  return [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({ sel: m[1].trim(), body: m[2] }));
}

/** 选择器里提到这个片段的所有规则（`.mobile` 与 `.gate-mobile` 都算"手机端"）。 */
function blocks(needle: string): string[] {
  return rules()
    .filter((r) => r.sel.includes(needle))
    .map((r) => r.body);
}

function has(needle: string, pattern: RegExp): boolean {
  return blocks(needle).some((b) => pattern.test(b));
}

/** 字号字面量（px）：手机端不许出现 < 16px 的。 */
function pxFontSizes(needle: string): number[] {
  const out: number[] = [];
  for (const body of blocks(needle)) {
    for (const m of body.matchAll(/font-size:\s*([^;]+);/g)) {
      for (const px of m[1].matchAll(/(\d+(?:\.\d+)?)px/g)) out.push(Number(px[1]));
    }
  }
  return out;
}

describe("手机端字号体系（agora-x70t.xsgz）", () => {
  it("基准字号与 mobileText.ts 的档位表一致，且默认档 ≥16px", () => {
    const base = blocks(".mobile").find((b) => b.includes("--m-fs:"));
    expect(base, ".mobile / .gate-mobile 上必须有 --m-fs 基准").toBeTruthy();
    expect(base).toContain(`--m-fs: ${MOBILE_TEXT_SIZES.find((s) => s.key === DEFAULT_MOBILE_TEXT_SIZE)!.px}px`);
    expect(MOBILE_TEXT_SIZES.find((s) => s.key === DEFAULT_MOBILE_TEXT_SIZE)!.px).toBeGreaterThanOrEqual(16);
  });

  it("四档 data-text 都在 CSS 里，值就是档位表的 px（默认档由基准那条规则承担）", () => {
    for (const s of MOBILE_TEXT_SIZES) {
      if (s.key === DEFAULT_MOBILE_TEXT_SIZE) continue; // 默认档 = .mobile 上的 --m-fs（上一条已钉）
      expect(has(`[data-text="${s.key}"]`, new RegExp(`--m-fs:\\s*${s.px}px`)), `data-text="${s.key}"`).toBe(true);
    }
  });

  it("手机端没有写死的 px 字号：全部随 --m-fs 走（小于 16px 的一律红）", () => {
    for (const needle of [".mobile", ".gate-mobile"]) {
      for (const px of pxFontSizes(needle)) {
        expect(px, `${needle} 里出现了 ${px}px 的字号：改用 var(--m-fs) / --m-fs-mini / --m-fs-note`).toBeGreaterThanOrEqual(16);
      }
    }
  });

  it("输入框字号有 16px 的下限（iOS 聚焦不整页放大），触控 44px 是绝对值", () => {
    expect(has(".mobile-composer input", /font-size:\s*max\(16px/), "composer 输入框").toBe(true);
    expect(has(".mobile", /--m-tap:\s*max\(44px/), ".mobile / .gate-mobile 的 --m-tap").toBe(true);
  });
});

describe("手机端溢出与触控（agora-x70t.qz01）", () => {
  it("长词可折：任务行 / 说明 / 问题 / 卡片头的名字不能画到卡片外", () => {
    for (const sel of [".mobile-card-task", ".mobile-note", ".mobile-question", ".mobile-card-head .mobile-row-name"]) {
      expect(has(sel, /overflow-wrap:\s*anywhere/), `${sel} 需要 overflow-wrap: anywhere`).toBe(true);
    }
  });

  it("硬夹断与收缩：行/卡片不许有东西画出框外，名字能收缩", () => {
    // WebKit 与 Chromium 对 flex 项的收缩行为不同（2026-10-08 实测：80 字符无空格名字在 WebKit
    // 里盒子 1138px 顶出卡片框，Chromium 里会换行），所以除了让文字可折，还要有硬夹断兜底。
    expect(has(".mobile-row", /overflow:\s*hidden/), ".mobile-row 硬夹断").toBe(true);
    expect(has(".mobile-card", /overflow-x:\s*hidden/), ".mobile-card 横向硬夹断").toBe(true);
    expect(has(".mobile-row-head > \*", /min-width:\s*0/), "行头的子项要能收缩").toBe(true);
    expect(has(".mobile-row-name, .mobile-status", /overflow-wrap:\s*anywhere/), "名字/状态可折").toBe(true);
    expect(has(".mobile-bubble-body", /min-width:\s*0/), "气泡正文要能收缩").toBe(true);
  });

  it("主要动作的命中高度有触控下限", () => {
    for (const sel of [".mobile-back", ".mobile-gear", ".mobile-more-toggle", ".mobile-composer button", ".mobile-decision-actions button", ".mobile-finished-toggle", ".mobile-push-toggle", ".mobile-text-sizes button", ".mobile .dialog button"]) {
      expect(has(sel, /min-height:\s*var\(--m-tap\)/), `${sel} 需要 min-height: var(--m-tap)`).toBe(true);
    }
  });

  it("收件箱的单行省略号是设计不是 bug：task / summary 仍是 nowrap + ellipsis", () => {
    for (const sel of [".mobile-row-task", ".mobile-row-summary"]) {
      expect(has(sel, /text-overflow:\s*ellipsis/), sel).toBe(true);
      expect(has(sel, /white-space:\s*nowrap/), sel).toBe(true);
    }
  });
});

describe("手机端视口 / 键盘 / 安全区（agora-x70t.ten0）", () => {
  it("viewport 要 viewport-fit=cover 与 interactive-widget=resizes-content", () => {
    expect(html).toContain("viewport-fit=cover");
    expect(html).toContain("interactive-widget=resizes-content");
  });

  it("壳子高度用 100dvh（Safari 工具栏不把 composer 顶出屏幕）", () => {
    expect(has(".mobile", /100dvh/), "100dvh").toBe(true);
  });

  it("四条 safe-area 都要让：上下（状态栏 / 主屏指示条）与左右（横屏刘海）", () => {
    const all = [...blocks(".mobile-top"), ...blocks(".mobile-inbox"), ...blocks(".mobile-card"), ...blocks(".mobile-composer"), ...blocks(".gate-mobile")].join("\n");
    for (const side of ["top", "bottom", "left", "right"]) {
      expect(all, `safe-area-inset-${side}`).toContain(`safe-area-inset-${side}`);
    }
  });

  it("横屏不放大文字（-webkit-text-size-adjust）", () => {
    const all = [...blocks("html"), ...blocks("body")].join("\n");
    expect(all).toContain("-webkit-text-size-adjust");
  });
});
