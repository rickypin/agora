/** 侧栏宽度：px 记忆，不存百分比（agora-uvd.5）。 */

export const SIDEBAR_WIDTH_KEY = "agora.sidebar-width";
export const MIN = 220;
export const DEFAULT = 260;

export function maxWidth(viewportWidth = typeof window === "undefined" ? DEFAULT * 2 : window.innerWidth): number {
  return Math.floor(viewportWidth * 0.5);
}

export function clamp(w: number, viewportWidth = typeof window === "undefined" ? DEFAULT * 2 : window.innerWidth): number {
  return Math.min(maxWidth(viewportWidth), Math.max(MIN, Math.round(w)));
}

/**
 * 读上次拖出来的宽度。隐私窗口 / 被禁的存储 / 非数字都回默认 260：这只是视图状态，丢了就回到骨架宽度。
 */
export function loadWidth(storage: Pick<Storage, "getItem"> | null = safeStorage()): number {
  try {
    const raw = storage?.getItem(SIDEBAR_WIDTH_KEY);
    if (raw == null || raw.trim() === "") return DEFAULT;
    const n = Number(raw);
    if (!Number.isFinite(n)) return DEFAULT;
    return clamp(n);
  } catch {
    return DEFAULT;
  }
}

export function storeWidth(w: number, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(SIDEBAR_WIDTH_KEY, String(Math.round(w)));
  } catch {
    // 存不下就算了：下次打开回到默认 260。
  }
}

export function clearWidth(storage: Pick<Storage, "removeItem"> | null = safeStorage()): void {
  try {
    storage?.removeItem(SIDEBAR_WIDTH_KEY);
  } catch {
    // 删不掉也无所谓：loadWidth 对非法值会回默认。
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
