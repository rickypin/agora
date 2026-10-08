/**
 * 安全区自适应（agora-xu12）。
 *
 * 为什么需要：`viewport-fit=cover` + `black-translucent` 时（主屏 PWA 全屏），页面确实铺到刘海下面，
 * CSS 里的 `env(safe-area-inset-top)`（iPhone 16 Pro 上是 62px）必须照加；但同一台手机上如果视口比
 * 屏幕矮（Safari 的浏览器 chrome 在，或状态栏样式不是 translucent），**这 62px 系统已经让出去了**，
 * 再加一遍就是顶部凭空多一条空白。实测证据（2026-10-08，同一台 iPhone 18.5）：
 *   03:03 的报告 inner=[402,812] screen=[402,874]，safeArea.top=62 —— 差值正好是状态栏那 62px。
 * 判据取"视口高度是否覆盖整屏"（差 > 4px 就算没覆盖），不猜 display-mode：它在浏览器与 PWA 里都可能
 * 是一样的。四条都算出来写进 `--safe-top` / `--safe-bottom`（CSS 只用这两个变量，不再直接用 env()）。
 */
export interface Insets {
  /** 实际有效的上/下让位（px）。 */
  top: number;
  bottom: number;
  /** 视口是否覆盖整屏（false = 系统已经让出了 chrome）。 */
  fullscreen: boolean;
  /** 读到的 env(safe-area-inset-top/bottom) 原始值（报告用）。 */
  envTop: number;
  envBottom: number;
}

/** 读一次 env(safe-area-inset-*)（computed style 拿不到 env 本值，得靠探针元素的 padding）。 */
export function envInsets(): { top: number; bottom: number } {
  if (typeof document === "undefined") return { top: 0, bottom: 0 };
  const probe = document.createElement("div");
  probe.style.cssText =
    "position:fixed;left:-9999px;top:-9999px;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left);";
  document.body.appendChild(probe);
  const cs = getComputedStyle(probe);
  const out = { top: parseFloat(cs.paddingTop) || 0, bottom: parseFloat(cs.paddingBottom) || 0 };
  probe.remove();
  return out;
}

/** 算当前该用的让位（纯函数，好单测）。 */
export function resolveInsets(inner: number, screen: number, env: { top: number; bottom: number }): Insets {
  const fullscreen = screen <= 0 || inner >= screen - 4;
  return {
    top: fullscreen ? env.top : 0,
    bottom: fullscreen ? env.bottom : 0,
    fullscreen,
    envTop: env.top,
    envBottom: env.bottom,
  };
}

/** 写进根元素的 CSS 变量；返回算出来的值（诊断报告用）。 */
export function applyInsets(insets: Insets, root: Element | null = document.documentElement): void {
  if (root instanceof HTMLElement) {
    root.style.setProperty("--safe-top", `${insets.top}px`);
    root.style.setProperty("--safe-bottom", `${insets.bottom}px`);
  }
}

export function currentInsets(): Insets {
  return resolveInsets(window.innerHeight, window.screen?.height ?? 0, envInsets());
}

let installed = false;

/** 装一次：立刻算 + 视口变化（旋转 / 地址栏收起 / 键盘）时重算。 */
export function installSafeInsets(): Insets {
  const run = () => applyInsets(currentInsets());
  const insets = (() => {
    try {
      return currentInsets();
    } catch {
      return resolveInsets(0, 0, { top: 0, bottom: 0 });
    }
  })();
  applyInsets(insets);
  if (typeof window !== "undefined" && !installed) {
    installed = true;
    window.addEventListener("resize", run);
    window.visualViewport?.addEventListener("resize", run);
  }
  return insets;
}
