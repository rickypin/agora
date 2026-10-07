/**
 * agora 的最小 service worker（agora-thc.4；docs/spec/ux.md /m；A35 / A37 的可安装半边）。
 *
 * 只做生命周期：install 立刻接管（skipWaiting）、activate 认领已有页面（clients.claim），
 * 这样升级后 PWA 里不会长期停在旧版本（A39）。**不缓存任何东西**：收件箱与终端离线没有
 * 意义，缓存反而会把升级前的旧 bundle 留在手机上；`/sw.js` 由服务端以 no-cache 下发，
 * 浏览器每次打开都能拿到新脚本。
 *
 * push / notificationclick 的挂点由 agora-thc.7 填；本文件在那之前保持无状态。
 */
self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});
