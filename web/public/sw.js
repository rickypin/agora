/**
 * agora 的最小 service worker（agora-thc.4 生命周期 + agora-thc.7 推送；docs/spec/ux.md /m）。
 *
 * 生命周期：install 立刻接管（skipWaiting）、activate 认领已有页面（clients.claim），这样升级后
 * PWA 里不会长期停在旧版本（A39）。**不缓存任何东西**：收件箱与终端离线没有意义，缓存反而会把
 * 升级前的旧 bundle 留在手机上；`/sw.js` 由服务端以 no-cache 下发，浏览器每次打开都能拿到新脚本。
 *
 * 推送（thc.7）：载荷只有服务端给的 `{ title, session }`（docs/spec/api.md：不带正文——正文是
 * 权限命令 / 回复原文，锁屏不显示）。`tag = session`：同一个会话在通知中心只占一格（换一条新的
 * 覆盖旧的）。**每个 push 都必须 showNotification**：iOS 可能因为没展示通知而吊销订阅（未实测，
 * 2026-10-07 记为待 👁 的取舍）——"有可见客户端就不弹"的抑制策略等真机确认允许之后再上，现在
 * 一律显示 + tag 去重。
 *
 * notificationclick：优先聚焦已经开着的 /m 页面并把会话交给它（不重新加载——页面自己打开会话卡，
 * 可发送的行还会把焦点放进 composer）；没有就 openWindow `/m?session=…`。
 */
self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

/** push 载荷解析失败不抛：宁可弹一条无会话的通知，也不能让事件 reject（iOS 会记这笔账）。 */
function pushPayload(event) {
  try {
    return event.data ? event.data.json() : {};
  } catch {
    return {};
  }
}

self.addEventListener("push", (event) => {
  const data = pushPayload(event);
  const session = typeof data.session === "string" && data.session ? data.session : null;
  const title = typeof data.title === "string" && data.title ? data.title : "agora";
  event.waitUntil(
    self.registration.showNotification(title, {
      tag: session || "agora",
      data: { session },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const raw = event.notification.data ? event.notification.data.session : null;
  const session = typeof raw === "string" && raw ? raw : null;
  const path = session ? "/m?session=" + encodeURIComponent(session) : "/m";
  event.waitUntil(
    (async () => {
      const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const mine = all.filter((c) => c.url.startsWith(self.location.origin));
      const open = mine.find((c) => c.url.includes("/m")) || mine[0];
      if (open) {
        await open.focus();
        open.postMessage({ type: "agora-open-session", session });
        return;
      }
      await self.clients.openWindow(path);
    })(),
  );
});
