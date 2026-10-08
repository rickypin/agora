/**
 * 手机端的状态词与行文案（agora-o975.4 / agora-o975.3；MISSION §6.9；docs/spec/ux.md「移动端交互收件箱 /m」）。
 *
 * 桌面共用的 `attention.ts` `STATUS_TEXT` 是英文（waiting / turn done / working），手机上读不懂
 * 「看过没看过、排队没排队」；「在跑」那个段名更把「点开看过一次、其实没在跑」的行说成在跑。
 * 所以手机自己一份：词表只在这里，**桌面一个字不动**（attention.test.ts 的既有用例照旧断言英文）。
 *
 * 「已看过」不是从行上读的：它是本设备的视图状态（MISSION §4.6 证据 ①），调用方把
 * `seen.has(seenKey(row))` 算成 boolean 传进来——记号跟着「这一次完成」走（agora-5gg.21）。
 */
import { formatAgo, isPeerRow } from "./attention";
import type { SessionRow } from "./events";

/** 状态词：waiting 的权限特例见 [`mobileStatusText`]。 */
export const MOBILE_STATUS_TEXT: Record<string, string> = {
  waiting: "等你",
  turn_done: "回完了",
  running: "在跑",
  starting: "启动中",
  idle: "闲着",
  finished: "已结束",
  failed: "失败",
  unknown: "说不清",
};

/** 一行手机端的状态词；`seen` 只对 turn_done 有意义（「回完了 / 已看过」）。 */
export function mobileStatusText(row: SessionRow, seen: boolean): string {
  if (row.status === "waiting" && row.reason === "permission") return "等你批准";
  if (row.status === "turn_done" && seen) return "已看过";
  return MOBILE_STATUS_TEXT[row.status] ?? row.status;
}

/**
 * 行文案（状态词 + 时长）：本机精确、peer 行的 `≥` 是并入方给的下界（ADR-004，与桌面同一条语义，
 * 只换了词）。时长口径见 [`mobileAgo`]。
 */
export function mobileStatusLine(row: SessionRow, seen: boolean, nowSeconds: number): string {
  const text = mobileStatusText(row, seen);
  const s = row.status_since;
  const ago = typeof s === "number" ? formatAgo(nowSeconds - s) : "";
  if (!ago) return text;
  return isPeerRow(row) ? `${text} ≥${ago}` : `${text} ${ago}`;
}
