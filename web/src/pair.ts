/** 设备配对（ADR-003 D2）：链接 `<origin>/#pair=<token>`，前端兑换后清掉 fragment。 */

export function extractPairToken(hash: string): string | null {
  const m = /^#pair=([A-Za-z0-9_-]+)$/.exec(hash);
  return m ? m[1] : null;
}

export interface PairedDevice {
  id: string;
  name: string;
}

/** `POST /api/auth/pair`：成功返回设备，失败（未知 / 已用 / 过期）返回 null。 */
export async function redeemPair(token: string): Promise<PairedDevice | null> {
  const resp = await fetch("/api/auth/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
  });
  if (!resp.ok) return null;
  const body = (await resp.json()) as { device?: PairedDevice };
  return body.device ?? null;
}

/** 有没有有效 session：`GET /api/auth/devices` 200 即已配对。 */
export async function isPaired(): Promise<boolean> {
  const resp = await fetch("/api/auth/devices");
  return resp.ok;
}

/** 把 token 从地址栏与历史里抹掉：它是一次性的，留着只会误导。 */
export function clearFragment(): void {
  if (window.location.hash) {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
  }
}
