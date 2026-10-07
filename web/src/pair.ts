import { apiFetch } from "./net";

/** 设备配对（ADR-003 D2）：链接 `<origin>/#pair=<token>`，前端兑换后清掉 fragment。 */

export function extractPairToken(hash: string): string | null {
  const m = /^#pair=([A-Za-z0-9_-]+)$/.exec(hash);
  return m ? m[1] : null;
}

/**
 * 从粘贴板里认出 token：整条链接（`https://host/#pair=<token>`）或裸 token 都行。
 *
 * 为什么门页要有这条：iOS 主屏 PWA 与 Safari 的存储 / cookie 可能不共享——扫码通常进 Safari，
 * 配对好了之后从主屏打开还是未配对（ADR-003 D2 的已知形状，agora-thc.1）。这时把链接粘进
 * 主屏 PWA 的门页就能补齐，不必回到终端。裸 token 也收（人可能只复制了那一截）。
 */
export function extractPairTokenFromText(text: string): string | null {
  const trimmed = text.trim();
  const m = /#pair=([A-Za-z0-9_-]{20,})/.exec(trimmed);
  if (m) return m[1];
  // 裸 token：32 字节 base64url 是 43 字符；放宽到 ≥20，交给服务端判无效比前端猜错好。
  return /^[A-Za-z0-9_-]{20,}$/.test(trimmed) ? trimmed : null;
}

export interface PairedDevice {
  id: string;
  name: string;
}

/** `POST /api/auth/pair`：成功返回设备，失败（未知 / 已用 / 过期）返回 null。 */
export async function redeemPair(token: string): Promise<PairedDevice | null> {
  const resp = await apiFetch("/api/auth/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
  });
  if (!resp.ok) return null;
  const body = (await resp.json()) as { device?: PairedDevice };
  return body.device ?? null;
}

/**
 * 同一 token 只 POST 一次（agora-3w8）。
 *
 * React 19 StrictMode 的开发模式对 effect 同步地 挂载 → 清理 → 再挂载，App 里读 fragment 并兑换的
 * effect 因此跑两遍、两遍都读到同一个 token。反例（2026-09-06，agora-hhu 代检，Vite dev 页）：daemon
 * 日志同一毫秒两条 POST /api/auth/pair，第一条 200「设备已配对」、第二条 401「链接未知、已用或已过期」
 * ——token 单次使用（ADR-003 D2）——页面据第二条显示「配对链接无效」，其实 cookie 已经发下来了，reload
 * 才正常。生产构建没有这一步，但会让做代检的人以为配对失败。
 *
 * 修法是按 token 缓存 promise：第二次调用拿同一个在途 / 已落定的 promise，不再发请求。**不要**改成
 * "第一次成功后立刻清 fragment 让第二次看不到 token"：第二次 effect 会走 isPaired() 分支，而第一次的
 * 结果被 cancelled 丢掉——竞态下页面停在「未配对」。清 fragment 仍由调用方在 promise 落定后做。
 * 缓存是模块级的：同一页面生命周期内 token 就是用过了，reload 拿的是新模块，不需要失效。
 */
const redeemed = new Map<string, Promise<PairedDevice | null>>();

export function redeemPairOnce(token: string): Promise<PairedDevice | null> {
  let pending = redeemed.get(token);
  if (!pending) {
    pending = redeemPair(token);
    redeemed.set(token, pending);
  }
  return pending;
}

/** 有没有有效 session：`GET /api/auth/devices` 200 即已配对。 */
export async function isPaired(): Promise<boolean> {
  const resp = await apiFetch("/api/auth/devices");
  return resp.ok;
}

/** 把 token 从地址栏与历史里抹掉：它是一次性的，留着只会误导。 */
export function clearFragment(): void {
  if (window.location.hash) {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
  }
}
