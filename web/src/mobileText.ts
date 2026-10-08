/**
 * 手机端的字号档位（agora-x70t.xsgz；MISSION §6.9 的"能力同源、呈现分级"）。
 *
 * 为什么要有这个旋钮：iPhone 上整屏文字 13px（桌面密度）读起来吃力（2026-10-08 用户反馈 +
 * agent-browser 计算样式量测），而 iOS Safari 不把系统的"文字大小"设置暴露给网页（`-apple-system-body`
 * 只在 Safari 里生效、还会带走行高与字体族），所以档位由我们自己给，默认取 Apple HIG 的 body 17pt。
 *
 * 为什么是 17 而不是 16：16px 只是 iOS **不自动放大输入框**的临界值（低于它一聚焦就整页放大），
 * 是下限不是目标；按 HIG 的 body 取 17，顺带也满足了下限。
 *
 * 档位是比例而不是绝对值：CSS 侧只有一个 `--m-fs`（`web/src/index.css` 的 `.mobile`），字号与
 * 间距都由它派生（`--m-1..--m-4`、`--m-tap`），所以这里改一个数，整屏的字、行距、留白、触控目标
 * 一起变——只放大字号不放大间距，屏幕会立刻显得挤。
 *
 * 存 localStorage：与"看过"集合（attention.ts 的 SEEN_STORAGE_KEY）同一条纪律——读写了包
 * try/catch（隐私窗口 / 存储被禁时访问 localStorage 本身会抛），读不出或值不认识就退回默认档；
 * 丢了的代价只是回到标准字号。
 */
export const MOBILE_TEXT_STORAGE_KEY = "agora.mobile-text";

/** 四档：值进 `data-text` 属性，px 是 CSS 侧 `--m-fs` 的取值（两边必须一致，守卫在 mobileCss.test.ts）。 */
export const MOBILE_TEXT_SIZES = [
  { key: "s", label: "小", px: 15 },
  { key: "m", label: "标准", px: 17 },
  { key: "l", label: "大", px: 19 },
  { key: "xl", label: "特大", px: 22 },
] as const;

export type MobileTextSize = (typeof MOBILE_TEXT_SIZES)[number]["key"];

export const DEFAULT_MOBILE_TEXT_SIZE: MobileTextSize = "m";

export function isMobileTextSize(v: unknown): v is MobileTextSize {
  return MOBILE_TEXT_SIZES.some((s) => s.key === v);
}

export function loadMobileTextSize(
  storage: Pick<Storage, "getItem"> | null = safeStorage(),
): MobileTextSize {
  try {
    const raw = storage?.getItem(MOBILE_TEXT_STORAGE_KEY);
    return isMobileTextSize(raw) ? raw : DEFAULT_MOBILE_TEXT_SIZE;
  } catch {
    return DEFAULT_MOBILE_TEXT_SIZE;
  }
}

export function storeMobileTextSize(
  size: MobileTextSize,
  storage: Pick<Storage, "setItem"> | null = safeStorage(),
): void {
  try {
    storage?.setItem(MOBILE_TEXT_STORAGE_KEY, size);
  } catch {
    // 存不下就算了：这一次会话内仍然生效，下次打开回到默认档。
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
