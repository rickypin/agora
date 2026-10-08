/**
 * 前端构建号（agora-xu12）：vite 的 `define` 在构建时把它替换成字面量（`<YYYY-MM-DDTHH:MMZ>`），
 * 显示在设置页底部、并随诊断报告一起复制出去——用来判"这台手机跑的是不是最新前端"
 * （iOS 主屏 PWA 会把 start_url 钉在缓存里，行为上是看不出来的）。同一份值也写成 `web/dist/build.txt`，
 * 被内嵌进 binary 由 `/api/system` 的 `web_build` 报出来：两边一比就有确定答案，不必再吵"你重启了吗"。
 *
 * **不用 git sha**：web bundle 在 commit 之前构建，`git rev-parse HEAD` 拿到的是父提交（2026-10-08
 * 实测：手机上显示 823b09c、实际已经是它的下一个提交，白排查一轮）。
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
