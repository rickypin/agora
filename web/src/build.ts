/**
 * 前端构建号（agora-xu12）：vite 的 `define` 在构建时把它替换成字面量（`<sha>-<UTC 时刻>`），
 * 显示在设置页底部、并随诊断报告一起复制出去——用来判"这台手机跑的是不是最新前端"
 * （iOS 主屏 PWA 会把 start_url 钉在缓存里，行为上是看不出来的）。
 */
declare const __WEB_BUILD__: string;

export function webBuild(): string {
  try {
    return __WEB_BUILD__;
  } catch {
    // 单测（vitest 走 define 也一样有；这里兜底 jest 之外的直跑）
    return "dev";
  }
}
