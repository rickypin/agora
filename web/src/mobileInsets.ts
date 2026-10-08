/**
 * 安全区自适应（agora-xu12）。
 *
 * 为什么需要：`viewport-fit=cover` + `black-translucent` 时（主屏 PWA 全屏），页面确实铺到刘海下面，
 * CSS 里的 `env(safe-area-inset-top)`（iPhone 16 Pro 上是 62px）必须照加；但同一台手机上如果视口比
 * 屏幕小（Safari 的浏览器 chrome 在，或状态栏样式不是 translucent），**这 62px 系统已经让出去了**，
 * 再加一遍就是顶部凭空多一条空白。
 * 2026-10-09 完整 iOS 18.5 Simulator 反例：black-translucent 主屏启动时 inner=402×812、
 * screen=402×874，标题仍在状态栏下面。不能单凭高度较小就清掉安全区；独立模式优先保留 env，
 * 浏览器模式才按长短边判断。软键盘缩小视口时也不能丢掉独立模式的顶部让位。
 * 结果写进 --safe-top / --safe-bottom；左右两侧直接用 env()。
 */
export interface Insets {
  /** 实际有效的上/下让位（px）。 */
  top: number;
  bottom: number;
  /** 视口尺寸是否覆盖整屏；独立模式较小时也可能覆盖状态栏。 */
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

export interface Viewport {
  w: number;
  h: number;
}

/**
 * 算当前该用的让位（纯函数，好单测）。
 *
 * **两个维度都要比**，而且按"长短边"比：只看高度的话横屏必然误判——横屏时 `innerHeight` 是 402，
 * 而 `screen.height` 在 iOS 上仍是竖屏的 874（`screen` 两维是否随旋转交换，各浏览器口径不一），
 * 于是判定"非全屏"、把横屏底部那 21px 的 home indicator 让位丢掉（2026-10-08 自查发现）。
 * 按排序后的长短边比就两种口径都对：视口盖住整块屏幕 ⇔ 长短边分别相等。
 */
export function resolveInsets(inner: Viewport, screen: Viewport, env: { top: number; bottom: number }, standalone = false): Insets {
  const same = (a: number, b: number) => Math.abs(a - b) <= 4;
  const innerSides = [Math.min(inner.w, inner.h), Math.max(inner.w, inner.h)];
  const screenSides = [Math.min(screen.w, screen.h), Math.max(screen.w, screen.h)];
  const known = screen.w > 0 && screen.h > 0;
  const fullscreen = !known || (same(innerSides[0], screenSides[0]) && same(innerSides[1], screenSides[1]));
  return {
    top: standalone || fullscreen ? env.top : 0,
    bottom: standalone || fullscreen ? env.bottom : 0,
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

function isStandalone(): boolean {
  return (navigator as Navigator & { standalone?: boolean }).standalone === true
    || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

/** iOS 18.5 standalone reports dvh=812 but lvh=874 (2026-10-09 Simulator).
 * Keep the full canvas until the keyboard consumes substantially more than the status bar.
 * Compare the screen edge opposite the current width so portrait keyboard resizing is not
 * mistaken for a landscape rotation. Browser chrome continues to use dynamic viewport units.
 */
export function mobileViewportHeight(inner: Viewport, screen: Viewport, standalone: boolean): string {
  const portrait = Math.abs(inner.w - Math.min(screen.w, screen.h)) <= 4;
  const height = portrait ? Math.max(screen.w, screen.h) : Math.min(screen.w, screen.h);
  return standalone && height - inner.h < 150 ? "100lvh" : "100dvh";
}

export function currentInsets(): Insets {
  return resolveInsets(
    { w: window.innerWidth, h: window.innerHeight },
    { w: window.screen?.width ?? 0, h: window.screen?.height ?? 0 },
    envInsets(),
    isStandalone(),
  );
}

let installed = false;

/** 装一次：立刻算 + 视口变化（旋转 / 地址栏收起 / 键盘）时重算。 */
export function installSafeInsets(): Insets {
  const resize = () => document.documentElement.style.setProperty("--mobile-height", mobileViewportHeight(
    { w: window.innerWidth, h: window.visualViewport?.height ?? window.innerHeight },
    { w: window.screen.width, h: window.screen.height }, isStandalone(),
  ));
  const run = () => { applyInsets(currentInsets()); resize(); };
  const insets = (() => {
    try {
      return currentInsets();
    } catch {
      return resolveInsets({ w: 0, h: 0 }, { w: 0, h: 0 }, { top: 0, bottom: 0 });
    }
  })();
  applyInsets(insets);
  resize();
  if (typeof window !== "undefined" && !installed) {
    installed = true;
    window.addEventListener("resize", run);
    window.visualViewport?.addEventListener("resize", run);
  }
  return insets;
}
