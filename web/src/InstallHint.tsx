/**
 * 「添加到主屏幕」引导（agora-thc.7；docs/spec/ux.md /m 门屏）。
 *
 * iOS 没有 beforeinstallprompt，而且 `pushManager` 只在**已加到主屏**的 PWA 里存在——所以这段
 * 图文引导是 iPhone 收到推送的前置步骤，不是装饰。门页与设置页共用。
 */

/** 这张屏上是不是 iOS 且不在主屏（standalone）：是就值得显示引导。 */
export function iosWithoutStandalone(): boolean {
  if (typeof navigator === "undefined" || typeof window === "undefined") return false;
  const ua = navigator.userAgent;
  const ios = /iPad|iPhone|iPod/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  if (!ios) return false;
  // jsdom 没有 matchMedia：判不出 standalone 时按"不在主屏"算，引导宁可多显示一次。
  if (typeof window.matchMedia !== "function") return true;
  return !window.matchMedia("(display-mode: standalone)").matches;
}

export function InstallHint({ compact = false }: { compact?: boolean }) {
  return (
    <div className="install-hint" data-testid="install-hint">
      <p>要收推送，先把 agora 加到主屏幕：</p>
      <ol>
        <li>用 Safari 打开这个页面</li>
        <li>点底部的「分享」</li>
        <li>选「添加到主屏幕」，从主屏幕图标打开</li>
      </ol>
      {!compact && <p className="muted">主屏里的 agora 才能申请通知权限；Safari 标签页里收不到。</p>}
    </div>
  );
}
