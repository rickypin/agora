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
  it("会换行的正文可折；单行的三行（任务/摘要/行头）一律 nowrap + ellipsis", () => {
    for (const sel of [".mobile-note", ".mobile-question"]) {
      expect(has(sel, /overflow-wrap:\s*anywhere/), `${sel} 需要 overflow-wrap: anywhere`).toBe(true);
    }
    for (const sel of [".mobile-card-task", ".mobile-row-task", ".mobile-row-summary", ".mobile-row-name", ".mobile-agent", ".mobile-node-chip"]) {
      expect(has(sel, /text-overflow:\s*ellipsis/), `${sel} 需要 text-overflow: ellipsis`).toBe(true);
      expect(has(sel, /overflow:\s*hidden/), `${sel} 要把裁切留在自己盒子里（否则就是被祖先框盖住）`).toBe(true);
    }
  });

  it("行头是按结构分的优先级：名字可省略、状态永不让位、symbol 不退让", () => {
    // 2026-10-08 用户第二次报"文本被卡片外框盖住"：根因是一行塞了五个元素又没有可靠的收缩顺序。
    // 现在的结构：第一行只有 symbol + 名字（可省略）+ 状态（flex: none），优先级靠结构成立；
    // 徽标/节点挪到第二行，与任务文本按 100:100:1 顺序让位（同桌面侧栏 agora-8lb）。
    expect(has(".mobile-row-name", /flex:\s*0 1 auto/), "名字可收缩").toBe(true);
    expect(has(".mobile-row-name", /min-width:\s*2em/), "名字下限 2em").toBe(true);
    expect(has(".mobile-status", /flex:\s*none/), "状态不让位").toBe(true);
    expect(has(".mobile-symbol", /flex:\s*none/), "symbol 不退让").toBe(true);
    for (const sel of [".mobile-row-head", ".mobile-row-sub"]) {
      expect(has(sel, /display:\s*flex/), sel).toBe(true);
      expect(has(sel, /min-width:\s*0/), sel).toBe(true);
    }
    // 行头绝不折（收件箱是重复列表，行高要可预测）；卡片头是唯一的那一个，可以折成两行。
    expect(has(".mobile-row-head", /flex-wrap:\s*wrap/), "行头不该折行").toBe(false);
    expect(has(".mobile-card-head", /flex-wrap:\s*wrap/), "卡片头装不下时折行").toBe(true);
    expect(has(".mobile-row-task", /flex:\s*1 1 auto/), "任务文本最后让位（吃剩下的空间）").toBe(true);
    for (const sel of [".mobile-agent", ".mobile-node-chip"]) {
      expect(has(sel, /flex:\s*none/), `${sel} 不缩（第二行的锚点）`).toBe(true);
    }
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

describe("卡片吸顶与 composer 固定（agora-o975.1）", () => {
  it("卡片头吸顶、composer 钉底：sticky + 不透明背景 + z-index", () => {
    // 2026-10-08 真机反馈：.mobile-card 是滚动容器（overflow-y: auto），而头与 composer 都是它的
    // 普通子元素——内容一长头就滚掉、composer 要滚到底才看得见。两者都要 sticky、要不透明背景
    // （否则气泡从底下透出来）、要 z-index（否则滚过来的内容画在它们上面）。
    const head = blocks(".mobile-card-head").join("\n");
    expect(head, ".mobile-card-head 吸顶").toMatch(/position:\s*sticky/);
    expect(head, ".mobile-card-head 顶在滚动口").toMatch(/top:\s*0/);
    expect(head, ".mobile-card-head 要挡住下面的内容").toMatch(/background:\s*var\(--bg\)/);
    expect(head, ".mobile-card-head 要压在滚过来的气泡上面").toMatch(/z-index:\s*\d+/);

    const composer = blocks(".mobile-composer").join("\n");
    expect(composer, ".mobile-composer 钉底").toMatch(/position:\s*sticky/);
    expect(composer, ".mobile-composer 钉在滚动口底部").toMatch(/bottom:\s*0/);
    expect(composer, ".mobile-composer 要挡住下面的内容").toMatch(/background:\s*var\(--bg\)/);
    expect(composer, ".mobile-composer 要压在滚过来的气泡上面").toMatch(/z-index:\s*\d+/);
    // composer 还要吃掉底部安全区：主屏指示条不许盖住发送键。
    expect(composer, "composer 吃 safe-bottom").toContain("var(--safe-bottom)");
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

  it("横屏（矮视口）吃满宽度：去掉 44rem 的手机列上限与两侧边框", () => {
    // 2026-10-08 iPhone 16 Pro 横屏实测：874×402 里 44rem（rem 基准 13px）= 572px 居中，两侧各
    // 留 150px 空带 = "没有自适应横屏宽度"。判据用"矮视口"而不是 orientation（桌面窗口也能是横向），
    // 并把桌面那两条"居中一条栏"的边框去掉。
    const m = css.match(/@media \(orientation: landscape\) and \(max-height: 520px\) \{([\s\S]*?)\n\}/);
    expect(m, "横屏媒体查询").toBeTruthy();
    const block = m?.[1] ?? "";
    expect(block).toContain("max-width: none");
    expect(block).toContain("border-left: none");
    expect(block).toContain("border-right: none");
  });

  it("行必须自己声明 align-items: stretch（iOS 的 UA 给 <button> 塞了 flex-start）", () => {
    // 2026-10-08 iPhone 18.5 实测（用户在设置页「显示边框」看到每行黄框、报告里 row.w=377 而
    // row.sw=405/1123/1229、子元素宽度=各自内容宽度）：WebKit 的 UA 样式对 button 有
    // `align-items: flex-start`，我们只写了 display: flex 没写 align-items，于是 UA 值生效——
    // 列方向下每个子元素按内容自适应宽度，"最长的那句话"把整行内容撑到 1100+，再被行的
    // overflow: hidden 裁掉，表现为"文本被卡片外框盖住"。Playwright 的 WebKit 该 UA 值是 normal，
    // 所以本地怎么量都干净；修复就是把 stretch 显式写出来，UA 再也盖不住。
    const row = css.slice(css.indexOf(".mobile-row {"), css.indexOf("}", css.indexOf(".mobile-row {")));
    expect(row, ".mobile-row 的 align-items").toContain("align-items: stretch");
  });

  it("上下让位走 --safe-top/--safe-bottom（JS 自适应），env() 作为 :root 默认", () => {
    // 为什么要变量：同一台 iPhone 上，主屏 PWA 全屏时 env(safe-area-inset-top)=62 该照加；视口比
    // 屏幕矮时（浏览器 chrome 在）这 62px 系统已经让出去了、再加就是凭空多一条空白（2026-10-08
    // 实测 inner=[402,812] screen=[402,874]）。JS 算出来覆盖 --safe-top/--safe-bottom；JS 还没跑
    // 的第一帧走 :root 的 env() 默认，让位不会消失。
    expect(css, ":root 默认 top").toMatch(/--safe-top:\s*env\(safe-area-inset-top, 0px\)/);
    expect(css, ":root 默认 bottom").toMatch(/--safe-bottom:\s*env\(safe-area-inset-bottom, 0px\)/);
    expect(css, "顶栏用变量").toContain("var(--safe-top)");
    expect(css, "composer 用变量").toContain("var(--safe-bottom)");
  });

  it("四条 safe-area 都要让：上下（状态栏 / 主屏指示条）与左右（横屏刘海）", () => {
    const all = [...blocks(".mobile-top"), ...blocks(".mobile-inbox"), ...blocks(".mobile-card"), ...blocks(".mobile-composer"), ...blocks(".gate-mobile")].join("\n");
    // 左右没有"系统已经让出"的问题（横屏刘海才非 0），直接 env()；
    for (const side of ["left", "right"]) {
      expect(all, `safe-area-inset-${side}`).toContain(`safe-area-inset-${side}`);
    }
    expect(all, "上下走变量").toContain("var(--safe-top)");
    expect(all, "上下走变量").toContain("var(--safe-bottom)");
  });

  it("横屏不放大文字（-webkit-text-size-adjust）", () => {
    const all = [...blocks("html"), ...blocks("body")].join("\n");
    expect(all).toContain("-webkit-text-size-adjust");
  });
});
