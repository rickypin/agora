/**
 * agora 的 service worker（agora-thc.4 生命周期 + agora-thc.7 推送 + agora-vmt6 离线壳）。
 *
 * 生命周期：install 立刻接管（skipWaiting）、activate 认领已有页面（clients.claim），这样升级后
 * PWA 里不会长期停在旧版本（A39）。
 *
 * 壳缓存（agora-vmt6，2026-10-24）：
 * - 只缓存同源 GET 的**导航文档**与 `/assets/*`；`/api/*` 一律直连，不缓存（状态、终端、权限
 *   命令都不该从缓存里出来）。`/sw.js` 不拦截，浏览器每次按 no-cache 取新脚本。
 * - 导航是**网络优先**：在线时永远先问网络，成功就把最新 HTML 写回壳；网络失败或 4 s 没回来
 *   才回缓存的 /m（/m/* 深链）或 /。升级后在线不会停在旧 HTML（agora-8xwg 的旧壳问题）。
 * - `/assets/*` 文件名带内容 hash，缓存优先；新构建的新 URL 会重新取并缓存。
 * - install 预缓存 /m、/ 与它们引用的 `/assets/*`，所以第一次在线打开之后，下一次冷启动
 *   即使节点不可达也有壳（再交给 B2 的 20 s 期限与「暂时无法确认」文案）。
 * - activate 只删旧版本命名的 `agora-*` 缓存；同一个缓存里旧的 hash 资源会留着，量级是
 *   每次前端构建几百 KB，等下一次壳版本号变化时一起清。
 *
 * 旧壳风险（agora-8xwg）：如果哪天改成「缓存优先」，升级后手机会停在旧 HTML；这里的
 * 网络优先 + 4 s 超时是那条教训的守卫。
 *
 * 推送（thc.7）：载荷只有服务端给的 `{ title, session }`（docs/spec/api.md：不带正文——正文是
 * 权限命令 / 回复原文，锁屏不显示）。`tag = session`：同一个会话在通知中心只占一格（换一条新的
 * 覆盖旧的）。**每个 push 都必须 showNotification**：iOS 可能因为没展示通知而吊销订阅（未实测，
 * 2026-10-07 记为待 👁 的取舍）——"有可见客户端就不弹"的抑制策略等真机确认允许之后再上，现在
 * 一律显示；会话按 tag 替换，系统提醒每次用随机 UUID 独立展示。
 *
 * notificationclick：优先聚焦已经开着的 /m 页面并把会话交给它（不重新加载——页面自己打开会话卡，
 * 可发送的行还会把焦点放进 composer）；没有就 openWindow `/m?session=…`。
 */
const SHELL_CACHE = "agora-shell-v1";
const ASSET_CACHE = "agora-assets-v1";
const SHELL_PATHS = ["/m", "/"];
const NAV_TIMEOUT_MS = 4000;
const FETCH_TIMEOUT_MS = 5000;
const ASSET_RE = /(?:src|href)="(\/assets\/[^"]+)"/g;

/** 带超时的 fetch：install 预缓存与后台补资源不能把 SW 的安装拖死。 */
function fetchWithTimeout(input, init, ms) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  return fetch(input, { ...init, signal: controller.signal }).finally(() => clearTimeout(timer));
}

/**
 * 写入缓存前去掉 `Content-Encoding` / `Content-Length`：Fetch API 给的 body 已经解码，
 * 原样把 gzip 头存进去，离线回放时可能被再解码一次。Response 会按实际 body 重算长度。
 */
async function putDecoded(cache, key, response) {
  const headers = new Headers(response.headers);
  headers.delete("content-encoding");
  headers.delete("content-length");
  const body = await response.clone().arrayBuffer();
  await cache.put(
    key,
    new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers,
    }),
  );
}

/** 解析 HTML 里引用的 /assets/*（只用于预缓存，解析失败就当没有）。 */
function assetUrls(html) {
  return [...new Set([...html.matchAll(ASSET_RE)].map((match) => match[1]))];
}

/** install：把 /m、/ 与它们引用的 assets 放进壳缓存；失败不阻断安装（在线首访还会补）。 */
async function cacheShells() {
  try {
    const shells = await caches.open(SHELL_CACHE);
    const assets = await caches.open(ASSET_CACHE);
    await Promise.all(
      SHELL_PATHS.map(async (path) => {
        try {
          const response = await fetchWithTimeout(path, { cache: "no-cache" }, FETCH_TIMEOUT_MS);
          if (!response.ok) return;
          const html = await response.clone().text();
          await putDecoded(shells, path, response);
          await Promise.all(
            assetUrls(html).map(async (url) => {
              try {
                const asset = await fetchWithTimeout(url, {}, FETCH_TIMEOUT_MS);
                if (asset.ok) await putDecoded(assets, url, asset);
              } catch {
                // 单个资源失败不影响壳：导航时还会再取一次。
              }
            }),
          );
        } catch {
          // 节点暂时不可达 / 超时：保留旧壳，下一次在线导航会更新。
        }
      }),
    );
  } catch {
    // 存储被拒 / 配额满：装不上壳也要让 SW 正常激活；在线导航仍是直连。
  }
}

/** 导航成功后顺手补新 HTML 引用的 assets；不阻塞页面返回。 */
async function primeAssets(html) {
  try {
    const urls = assetUrls(html);
    if (!urls.length) return;
    const cache = await caches.open(ASSET_CACHE);
    await Promise.all(
      urls.map(async (url) => {
        if (await cache.match(url)) return;
        try {
          const asset = await fetchWithTimeout(url, {}, FETCH_TIMEOUT_MS);
          if (asset.ok) await putDecoded(cache, url, asset);
        } catch {
          // 页面自己还在取；这里失败不补。
        }
      }),
    );
  } catch {
    // 解析 / 缓存失败都不该影响页面。
  }
}

async function navigate(request, url) {
  const key = url.pathname.startsWith("/m") ? "/m" : "/";
  let cache = null;
  let cached = null;
  try {
    cache = await caches.open(SHELL_CACHE);
    cached = await cache.match(key);
  } catch {
    // 存储不可用：照常走网络，不因为有壳逻辑就挡住页面。
  }
  const network = fetch(request).then(async (response) => {
    if (response.ok && cache) {
      try {
        await putDecoded(cache, key, response.clone());
      } catch {
        // 配额满 / 存储被拒：页面照常返回，下一次再补。
      }
      void response.clone().text().then(primeAssets).catch(() => {});
    }
    return response;
  });
  // 超时后仍可能失败：这个 catch 只防止未处理的 rejection，不改变下面的返回值。
  network.catch(() => {});
  if (!cached) return network;
  return Promise.race([
    network,
    new Promise((resolve) => setTimeout(() => resolve(cached), NAV_TIMEOUT_MS)),
  ]).catch(() => cached);
}

async function cacheFirstAsset(request) {
  let cache = null;
  try {
    cache = await caches.open(ASSET_CACHE);
    const hit = await cache.match(request);
    if (hit) return hit;
  } catch {
    // 存储不可用：退回直连。
  }
  const response = await fetch(request);
  if (response.ok && cache) {
    try {
      await putDecoded(cache, request, response.clone());
    } catch {
      // 配额满 / 存储被拒：这次请求照常返回。
    }
  }
  return response;
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      await cacheShells();
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const keep = new Set([SHELL_CACHE, ASSET_CACHE]);
      try {
        const names = await caches.keys();
        await Promise.all(
          names
            .filter((name) => name.startsWith("agora-") && !keep.has(name))
            .map((name) => caches.delete(name)),
        );
      } catch {
        // 清理旧缓存失败不影响接管页面。
      }
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  // 状态 / 终端 / 权限命令绝不从缓存里出（ADR-002；AGENTS 的「状态只有一个来源」）。
  if (url.pathname.startsWith("/api/")) return;
  if (request.mode === "navigate") {
    event.respondWith(navigate(request, url));
    return;
  }
  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(cacheFirstAsset(request));
  }
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
      tag: session || `agora#${crypto.randomUUID()}`,
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
      const mine = all.filter((c) => new URL(c.url).origin === self.location.origin);
      const mobile = mine.find((c) => {
        const path = new URL(c.url).pathname;
        return path === "/m" || path.startsWith("/m/");
      });
      const open = mobile || (session ? mine[0] : null);
      if (open) {
        await open.focus();
        open.postMessage({ type: "agora-open-session", session });
        return;
      }
      await self.clients.openWindow(path);
    })(),
  );
});
