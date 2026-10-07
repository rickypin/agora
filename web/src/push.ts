/**
 * Web Push 客户端（agora-thc.7；MISSION §6.6；A19 客户端半边）。
 *
 * 服务端已经会发（thc.6）：本模块管订阅这一侧——权限、`pushManager.subscribe`、把订阅交给
 * 承载节点、取消、以及每次打开 /m 的自查重订（iOS 可能静默失效）。**桌面浏览器通知不动**
 * （notify.ts 那条路照旧），桌面也不装 push。
 *
 * 与 DOM 解耦：`navigator.serviceWorker` / `PushManager` / `fetch` 都经 [`PushDeps`] 注入，
 * vitest 在 node 里用假 pushManager 跑（与 notify.ts 同一纪律）。iOS 上 `pushManager` 只在
 * **已加到主屏**的 PWA 里存在——浏览器标签页里 supported=false，设置页据此显示加主屏引导。
 */
import { apiFetch } from "./net";

export interface PushSubscriptionJson {
  endpoint: string;
  keys: { p256dh: string; auth: string };
}

export interface SubscriptionLike {
  endpoint: string;
  toJSON(): PushSubscriptionJson;
  unsubscribe(): Promise<boolean>;
}

export type Permission = "default" | "granted" | "denied" | "unsupported";

/** 设置的开关状态：`on` 才代表这条设备正在收推送。 */
export type PushState =
  | "on"
  | "off"
  | "denied"
  | "unsupported"
  | "insecure"
  | "no-key"
  | "failed";

export interface PushEnv {
  secure: boolean;
  supported: boolean;
  /** iOS（含 iPadOS 13+ 的桌面 UA）：引导文案只对它有"加到主屏"这一步。 */
  ios: boolean;
  /** display-mode: standalone——已经加到主屏。 */
  standalone: boolean;
  permission: () => Permission;
  requestPermission: () => Promise<Permission>;
  getSubscription: () => Promise<SubscriptionLike | null>;
  subscribe: (applicationServerKey: Uint8Array<ArrayBuffer>) => Promise<SubscriptionLike>;
  /** `GET /api/system` 的 `push.vapid_public_key`；没启用推送是 null。 */
  fetchVapidKey: () => Promise<string | null>;
  postSubscription: (s: PushSubscriptionJson) => Promise<boolean>;
  deleteSubscription: (endpoint: string) => Promise<boolean>;
}

export interface PushReport {
  state: PushState;
  /** 排障用的一句人话；`failed` / `denied` / `no-key` 时有值。 */
  detail?: string;
}

/** 生产环境接线。 */
export function browserPushEnv(): PushEnv {
  const nav = typeof navigator === "undefined" ? undefined : navigator;
  const win = typeof window === "undefined" ? undefined : window;
  const ua = nav?.userAgent ?? "";
  const supported =
    !!nav &&
    "serviceWorker" in nav &&
    typeof (win as unknown as { PushManager?: unknown })?.PushManager === "function" &&
    "Notification" in (win ?? {});
  const reg = async () => nav!.serviceWorker.ready;
  // 浏览器类型的 toJSON 字段是可选的（endpoint? / keys?），本模块的契约要求齐——这里补一次，
  // 免得每个调用点都判空。
  const normalize = (sub: PushSubscription): SubscriptionLike => ({
    endpoint: sub.endpoint,
    toJSON: () => {
      const j = sub.toJSON();
      return {
        endpoint: j.endpoint ?? sub.endpoint,
        keys: { p256dh: j.keys?.p256dh ?? "", auth: j.keys?.auth ?? "" },
      };
    },
    unsubscribe: () => sub.unsubscribe(),
  });
  return {
    secure: win?.isSecureContext === true,
    supported,
    ios: /iPad|iPhone|iPod/.test(ua) || (/Macintosh/.test(ua) && (nav?.maxTouchPoints ?? 0) > 1),
    standalone:
      typeof win?.matchMedia === "function" && win.matchMedia("(display-mode: standalone)").matches,
    permission: () =>
      typeof Notification === "undefined" ? "unsupported" : (Notification.permission as Permission),
    requestPermission: () =>
      typeof Notification === "undefined"
        ? Promise.resolve("unsupported")
        : Notification.requestPermission() as Promise<Permission>,
    getSubscription: async () => {
      const sub = await (await reg()).pushManager.getSubscription();
      return sub ? normalize(sub) : null;
    },
    subscribe: async (key) =>
      normalize(
        await (await reg()).pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key }),
      ),
    fetchVapidKey: async () => {
      const resp = await apiFetch("/api/system");
      if (!resp.ok) return null;
      const body = (await resp.json()) as { push?: { vapid_public_key?: string | null } };
      return body.push?.vapid_public_key ?? null;
    },
    postSubscription: async (s) => {
      const resp = await apiFetch("/api/push/subscriptions", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(s),
      });
      return resp.ok;
    },
    deleteSubscription: async (endpoint) => {
      const resp = await apiFetch("/api/push/subscriptions", {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ endpoint }),
      });
      return resp.ok || resp.status === 404;
    },
  };
}

/** 先在能用性上短路：不满足就不碰权限与订阅。 */
function blocked(env: PushEnv): PushReport | null {
  if (!env.secure) return { state: "insecure", detail: "要 HTTPS（或本机地址）才能收推送" };
  if (!env.supported) {
    return env.ios && !env.standalone
      ? { state: "unsupported", detail: "iOS 只在「已添加到主屏幕」的 PWA 里支持推送" }
      : { state: "unsupported", detail: "这个浏览器不支持 Web Push" };
  }
  return null;
}

/** 当前状态（设置页首屏）：有订阅 = on，其余按能不能订阅归类。 */
export async function pushState(env: PushEnv): Promise<PushReport> {
  const no = blocked(env);
  if (no) return no;
  const permission = env.permission();
  if (permission === "denied") return { state: "denied", detail: "通知权限被拒绝，在系统设置里改" };
  if (permission !== "granted") return { state: "off" };
  const sub = await env.getSubscription();
  return sub ? { state: "on" } : { state: "off" };
}

/**
 * 打开推送（必须在用户手势里调用：iOS 的权限请求只在手势里有效）。
 * 权限 → 公钥 → 订阅 → 交给承载节点，四步任一失败都返回可显示的原因。
 */
export async function enablePush(env: PushEnv): Promise<PushReport> {
  const no = blocked(env);
  if (no) return no;
  let permission = env.permission();
  if (permission === "default") {
    permission = await env.requestPermission();
  }
  if (permission !== "granted") {
    return { state: permission === "denied" ? "denied" : "failed", detail: "没有通知权限" };
  }
  const key = await env.fetchVapidKey();
  if (!key) {
    return { state: "no-key", detail: "承载节点还没启用推送（/api/system 没给 VAPID 公钥）" };
  }
  try {
    const sub = (await env.getSubscription()) ?? (await env.subscribe(urlBase64ToUint8Array(key)));
    const ok = await env.postSubscription(sub.toJSON());
    if (!ok) return { state: "failed", detail: "订阅没能登记到承载节点" };
    return { state: "on" };
  } catch (err) {
    return { state: "failed", detail: `订阅失败：${String(err)}` };
  }
}

/** 关掉推送：先退订再让服务端删（顺序反了会留下收不到的端点，服务端 410 才会清）。 */
export async function disablePush(env: PushEnv): Promise<PushReport> {
  const no = blocked(env);
  if (no) return no;
  try {
    const sub = await env.getSubscription();
    if (sub) {
      await env.deleteSubscription(sub.endpoint);
      await sub.unsubscribe();
    }
    return { state: "off" };
  } catch (err) {
    return { state: "failed", detail: `取消订阅失败：${String(err)}` };
  }
}

/**
 * 每次打开 /m 的自查（agora-thc.7）：iOS 的订阅可能被系统静默回收，本地看着还在、其实已经
 * 没人发。规则：本地有订阅就重新登记一次（服务端换库 / 换端点都能补回来）；本地没有但权限
 * 是 granted 就重新订阅（用户上次开了、后来被回收）。失败只返回状态，不打扰用户。
 */
export async function selfCheckPush(env: PushEnv): Promise<PushReport> {
  const no = blocked(env);
  if (no) return no;
  if (env.permission() !== "granted") return { state: "off" };
  try {
    const existing = await env.getSubscription();
    if (existing) {
      const ok = await env.postSubscription(existing.toJSON());
      return ok ? { state: "on" } : { state: "failed", detail: "自查登记失败，下次打开再试" };
    }
    const key = await env.fetchVapidKey();
    if (!key) return { state: "no-key", detail: "承载节点还没启用推送" };
    const sub = await env.subscribe(urlBase64ToUint8Array(key));
    const ok = await env.postSubscription(sub.toJSON());
    return ok ? { state: "on" } : { state: "failed", detail: "自查重订失败，下次打开再试" };
  } catch (err) {
    return { state: "failed", detail: `自查重订失败：${String(err)}` };
  }
}

/** VAPID 公钥是 base64url 的 65 字节点 → 订阅 API 要的 Uint8Array。 */
export function urlBase64ToUint8Array(b64: string): Uint8Array<ArrayBuffer> {
  const padding = "=".repeat((4 - (b64.length % 4)) % 4);
  const raw = atob((b64 + padding).replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
  return out;
}
