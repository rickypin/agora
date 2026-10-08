/**
 * 按钮上的 flex 容器必须自己声明 `align-items`（agora-x70t）。
 *
 * 为什么单开一条守卫：WebKit 的 UA 样式对 `<button>` 有 `align-items: flex-start`，而 Chromium 没有；
 * 更糟的是 Playwright 的 WebKit 与 iOS Safari 这个值还不一样（前者 normal、后者 flex-start），所以
 * **本地怎么量都是干净的**。2026-10-08 手机端收件箱因此每行内容被撑到 1100+ 像素、再被自己的
 * overflow: hidden 裁掉，用户在设置页用「显示边框」看到一片黄框（"文本被卡片外框盖住"）。修法不是
 * 调 flex-shrink，而是把 `align-items` 显式写出来，让 UA 值没有机会生效。
 *
 * 规则：任何一个用在 `<button className="…">` 上的类，只要有一条 CSS 规则同时给它 `display: flex`，
 * 那条规则就必须有 `align-items`（想用 UA 的默认值也请写出来）。桌面侧栏的 `.row` 早就显式写了
 * `align-items: flex-start`——它是横排、本意如此，所以安全。
 */
import { describe, expect, it } from "vitest";

// 用 vite 的 `?raw` 读源码而不是 node:fs：tsconfig 里没有 @types/node（同 vite.config.ts 顶部那条
// 注释），`node:fs` 在 typecheck 里是未声明的模块。index.css 的 `?raw` 已在 vite.config.ts 的
// `test.css.include` 里放行（别的守卫也这么读）。
const RAW = import.meta.glob(["./*.tsx", "./index.css"], {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;
const CSS = (RAW["./index.css"] ?? "").replace(/\/\*[\s\S]*?\*\//g, "");

/** `<button className="x y">` 里出现的类名（模板串里的 `${}` 片段忽略）。 */
function buttonClasses(): Set<string> {
  const out = new Set<string>();
  for (const [file, src] of Object.entries(RAW)) {
    if (!file.endsWith(".tsx")) continue;
    for (const m of src.matchAll(/<button\b[^>]*?className=(?:"([^"]+)"|\{`([^`]+)`\})/gs)) {
      for (const cls of (m[1] ?? m[2] ?? "").split(/[\s${}]+/)) {
        if (/^[a-z][\w-]*$/.test(cls)) out.add(cls);
      }
    }
  }
  return out;
}

/** CSS 规则（选择器, 声明块），注释已剔除。 */
function rules(): Array<{ sel: string; body: string }> {
  return [...CSS.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({ sel: m[1].trim(), body: m[2] }));
}

describe("按钮上的 flex 必须显式 align-items（agora-x70t）", () => {
  it("没有一条 flex 规则漏写 align-items", () => {
    const bad: string[] = [];
    for (const cls of buttonClasses()) {
      // 类名按**整词**匹配：`.row` 不能命中 `.row-main`（那是另一个元素上的类）。
      const asClass = new RegExp(`(^|[^\\w-])\\.${cls}(?![-\\w])`);
      for (const { sel, body } of rules()) {
        // 只看**选择器主体**（最后一个复合选择器）：`.row .meta` 管的是里面的文字，不是 `.row` 自己。
        const subject = sel.split(/[\s>+~]+/).pop() ?? "";
        if (!asClass.test(subject)) continue;
        if (/display:\s*(inline-)?flex/.test(body) && !/align-items\s*:/.test(body)) {
          bad.push(`${cls} ← ${sel.replace(/\s+/g, " ").slice(0, 70)}`);
        }
      }
    }
    expect(bad, "这些规则给按钮的 flex 容器留了 UA 默认值").toEqual([]);
  });

  it("守卫本身能抓：.mobile-row 一旦丢掉 align-items 就该红（反例自检）", () => {
    // 不用改文件：直接对"删掉 align-items 的 .mobile-row 规则"跑一遍同一套判定。
    const stripped = CSS.replace(/align-items:\s*stretch;/, "");
    const rule = stripped.match(/\.mobile-row\s*\{([^{}]*)\}/)?.[1] ?? "";
    expect(rule).toContain("display: flex");
    expect(rule).not.toContain("align-items");
  });
});
