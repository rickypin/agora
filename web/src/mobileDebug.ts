/**
 * 手机端布局诊断（agora-x70t 的现场取证工具）。
 *
 * 为什么要有它：2026-10-08 用户两次报"首屏文本被卡片外框盖住"，而我在 Playwright WebKit 用真数据、
 * 四种宽度、四档字号、横纵两轴全量过了一遍都复现不出来（Chromium 与 WebKit 的差异已经修过一轮，
 * agora-c03z）。手机是唯一跑真 Safari 的机器，**看不到它就没法再猜**——所以把"哪块元素出框 / 被裁"
 * 这件事做成页面上能看见、能复制的东西：
 *   1. 「显示边框」：给 /m 里每个元素描边；出框（盒子超出父容器）描红、被裁（内容宽/高超出自己的
 *      裁剪盒）描黄——用户一眼就知道我说的是哪一块，报给我的时候不用描述。
 *   2. 「复制布局报告」：把视口 / 安全区 / 字号档位 / 每行的文字与盒子 / 可疑元素（出框、被裁、
 *      纵向被裁）序列化成一段 JSON，粘给我就是现场。
 *
 * 只读、不发网络（不引入上报通道：一次性排障不值得为它开一个写端点）、不含任何凭据：报告里只有
 * 盒子数字、class 名与**行上已经显示出来的那点文字**（本来就是屏幕上可见的东西）。
 */
import { webBuild } from "./build";
import { currentInsets } from "./mobileInsets";

const OUTLINE_ATTR = "data-outline";
const OUTLINE_KEY = "agora.mobile-outline";
const CLIP_ATTR = "data-clip";
const OVERFLOW_ATTR = "data-overflow";

interface Box {
  w: number;
  h: number;
  sw: number;
  sh: number;
  fs: string;
  ox: string;
  oy: string;
  te: string;
  text: string;
}

function box(el: Element | null): Box | null {
  if (!el) return null;
  const r = el.getBoundingClientRect();
  const cs = getComputedStyle(el);
  return {
    w: Math.round(r.width),
    h: Math.round(r.height),
    sw: el.scrollWidth,
    sh: el.scrollHeight,
    fs: cs.fontSize,
    ox: cs.overflowX,
    oy: cs.overflowY,
    te: cs.textOverflow,
    text: (el.textContent ?? "").trim().slice(0, 60),
  };
}

/**
 * 内容超出自己的裁剪盒（横向或纵向）——"被裁"的判据。
 *
 * 只算 `hidden` / `clip`：`auto` / `scroll` 的溢出是**滚动**，是设计意图（收件箱、终端、diff 都靠它），
 * 报出来只会把现场淹掉——2026-10-08 用户那份报告里 `div.mobile-inbox` 就以"被裁"混进了嫌疑列表，
 * 而真正有问题的是每行（`overflow: hidden`）。
 */
function isClipped(el: Element): boolean {
  const cs = getComputedStyle(el);
  const hard = (v: string) => v === "hidden" || v === "clip";
  const clipX = hard(cs.overflowX) && el.scrollWidth > el.clientWidth + 1;
  const clipY = hard(cs.overflowY) && el.scrollHeight > el.clientHeight + 1;
  return clipX || clipY;
}

/** 盒子超出父元素的内容盒——"画到框外"的判据（父若是滚动容器则不算）。 */
function isOutOfParent(el: Element): boolean {
  const p = el.parentElement;
  if (!p) return false;
  const pcs = getComputedStyle(p);
  if (["hidden", "clip", "auto", "scroll"].includes(pcs.overflowX)) return false;
  const r = el.getBoundingClientRect();
  const pr = p.getBoundingClientRect();
  const right = pr.right - parseFloat(pcs.borderRightWidth || "0");
  const left = pr.left + parseFloat(pcs.borderLeftWidth || "0");
  return r.right > right + 0.5 || r.left < left - 0.5;
}

function selectorOf(el: Element): string {
  const cls = (el.className || "").toString().split(/\s+/).filter(Boolean).slice(0, 3).join(".");
  return `${el.tagName.toLowerCase()}${cls ? "." + cls : ""}`;
}

/** 可疑元素：出框 / 被裁（带类名与盒子，够我定位到 CSS 规则）。诊断自己的元素不算。 */
function suspects(): Array<{ el: string; kind: string; box: Box }> {
  const out: Array<{ el: string; kind: string; box: Box }> = [];
  for (const el of document.querySelectorAll(".mobile *")) {
    if (el.closest(".mobile-diag-actions, .mobile-diag-report")) continue;
    const cs = getComputedStyle(el);
    if (cs.display === "none" || cs.visibility === "hidden") continue;
    const r = el.getBoundingClientRect();
    if (!r.width && !r.height) continue;
    const kinds: string[] = [];
    if (isOutOfParent(el)) kinds.push("出框");
    if (isClipped(el)) kinds.push("被裁");
    if (!kinds.length) continue;
    const b = box(el);
    if (b) out.push({ el: selectorOf(el), kind: kinds.join("+"), box: b });
    if (out.length >= 40) break;
  }
  return out;
}

/** 安全区四条（用一个探针元素的 padding 读 env()——computed style 里拿不到 env 本值）。 */
function safeAreas(): Record<string, number> {
  const probe = document.createElement("div");
  probe.style.cssText =
    "position:fixed;left:-9999px;top:-9999px;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left);";
  document.body.appendChild(probe);
  const cs = getComputedStyle(probe);
  const out = {
    top: parseFloat(cs.paddingTop) || 0,
    right: parseFloat(cs.paddingRight) || 0,
    bottom: parseFloat(cs.paddingBottom) || 0,
    left: parseFloat(cs.paddingLeft) || 0,
  };
  probe.remove();
  return out;
}

let inboxSnapshot: string | null = null;

/** 抓一份当前屏的布局（收件箱挂载/行变化时调；开销是几百个元素的盒子读取，手机上 <1ms）。 */
export function rememberLayout(): void {
  try {
    inboxSnapshot = layoutReport();
  } catch {
    // 诊断本身永远不该把页面搞挂
  }
}

/** 上一次收件箱的布局快照（设置屏 / 会话卡里复制报告时，问题现场还是收件箱）。 */
export function lastLayout(): string | null {
  return inboxSnapshot;
}

/** 复制用的完整报告：当前屏 + 最近一次收件箱快照。 */
export function clipboardReport(): string {
  let current: unknown = null;
  try {
    current = JSON.parse(layoutReport());
  } catch {
    current = { error: "report failed" };
  }
  let inbox: unknown = null;
  try {
    inbox = inboxSnapshot ? JSON.parse(inboxSnapshot) : null;
  } catch {
    inbox = null;
  }
  return JSON.stringify({ current, inbox }, null, 1);
}

export function layoutReport(): string {
  const mobile = document.querySelector(".mobile");
  const rows = [...document.querySelectorAll(".mobile-row")].slice(0, 12).map((r) => ({
    id: r.getAttribute("data-testid"),
    box: box(r),
    head: box(r.querySelector(".mobile-row-head")),
    name: box(r.querySelector(".mobile-row-name")),
    status: box(r.querySelector(".mobile-status")),
    sub: box(r.querySelector(".mobile-row-sub")),
    agent: box(r.querySelector(".mobile-agent")),
    chip: box(r.querySelector(".mobile-node-chip")),
    task: box(r.querySelector(".mobile-row-task")),
    summary: box(r.querySelector(".mobile-row-summary")),
  }));
  const card = document.querySelector(".mobile-card");
  const vv = window.visualViewport;
  const report = {
    at: new Date().toISOString(),
    // 先看这一行：手机上跑的是不是最新前端（agora-xu12 的判据）。
    build: webBuild(),
    ua: navigator.userAgent,
    // jsdom 没有 matchMedia（诊断本身不该因为环境缺个 API 就炸掉）。
    standalone: typeof window.matchMedia === "function" ? window.matchMedia("(display-mode: standalone)").matches : false,
    viewport: {
      inner: [window.innerWidth, window.innerHeight],
      visual: vv ? [Math.round(vv.width), Math.round(vv.height), Math.round(vv.offsetTop)] : null,
      screen: [window.screen.width, window.screen.height],
      dpr: window.devicePixelRatio,
      doc: [document.documentElement.clientWidth, document.documentElement.scrollWidth],
    },
    safeArea: safeAreas(),
    // 实际生效的让位（JS 自适应后的值）；与上一条不同的行就是"系统已经让出去、不该再加"的那种情况。
    insets: (() => {
      try {
        const it = currentInsets();
        return { top: it.top, bottom: it.bottom, fullscreen: it.fullscreen, envTop: it.envTop, envBottom: it.envBottom };
      } catch {
        return null;
      }
    })(),
    scale: mobile
      ? {
          dataText: mobile.getAttribute("data-text"),
          fs: getComputedStyle(mobile).fontSize,
          tap: getComputedStyle(mobile).getPropertyValue("--m-tap").trim(),
        }
      : null,
    rows,
    card: card
      ? {
          box: box(card),
          head: box(card.querySelector(".mobile-card-head")),
          task: box(card.querySelector(".mobile-card-task")),
          user: box(card.querySelector(".mobile-bubble.user > span:not(.mobile-bubble-mark)")),
          body: box(card.querySelector(".mobile-bubble-body")),
          composer: box(card.querySelector(".mobile-composer")),
          input: box(card.querySelector(".mobile-composer input")),
        }
      : null,
    suspects: suspects(),
  };
  return JSON.stringify(report, null, 1);
}

/** 描边开着吗（存起来：设置屏里开的，回收件箱要还在——问题现场就是收件箱）。 */
export function outlineEnabled(): boolean {
  try {
    return localStorage.getItem(OUTLINE_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * 描边开关：红=出框、黄=被裁。状态存 localStorage，并在**每一屏**挂载时重新标记——
 * /m 的收件箱与设置屏是两个不同的 <main>，只在一个屏上设属性，切屏就丢了（用户没法拿它看收件箱）。
 */
export function toggleOutline(): boolean {
  const on = !outlineEnabled();
  try {
    if (on) localStorage.setItem(OUTLINE_KEY, "1");
    else localStorage.removeItem(OUTLINE_KEY);
  } catch {
    // 存不下就只在当前屏生效
  }
  applyOutline();
  return on;
}

/** 把当前屏的元素按 出框/被裁 打上属性（描边关着时清掉）。 */
export function applyOutline(root: Element | null = document.querySelector(".mobile")): void {
  if (!root) return;
  if (!outlineEnabled()) {
    root.removeAttribute(OUTLINE_ATTR);
    for (const el of document.querySelectorAll(`[${CLIP_ATTR}],[${OVERFLOW_ATTR}]`)) {
      el.removeAttribute(CLIP_ATTR);
      el.removeAttribute(OVERFLOW_ATTR);
    }
    return;
  }
  root.setAttribute(OUTLINE_ATTR, "1");
  for (const el of root.querySelectorAll("*")) {
    if (el.closest(".mobile-diag-actions, .mobile-diag-report")) continue;
    if (isOutOfParent(el)) el.setAttribute(OVERFLOW_ATTR, "1");
    if (isClipped(el)) el.setAttribute(CLIP_ATTR, "1");
  }
}

/** 复制到剪贴板；iOS 主屏 PWA 里 clipboard 可能被拒，返回 false 让界面退到 textarea。 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
