/**
 * 手机端的状态词与行文案（agora-o975.4 / agora-o975.3；MISSION §6.9；docs/spec/ux.md「移动端交互收件箱 /m」）。
 *
 * 两端共享这份用户文案（agora-p7p0）。文件名为兼容既有 import 保留；
 * attention.ts 的 STATUS_TEXT 只保留底层兼容输出，组件不得再另造一份状态词表。
 * 桌面与收件箱用 minutes，手机会话卡片用 seconds；已读由调用方传入。
 *
 * 「已读」不是从行上读的：它是本设备的视图状态（MISSION §4.6 证据 ①），调用方把
 * `seen.has(seenKey(row))` 算成 boolean 传进来——记号跟着「这一次完成」走（agora-5gg.21）。
 */
import { formatAgo, isPeerRow } from "./attention";
import type { SessionRow } from "./events";

/** 状态词：waiting 的权限特例见 [`mobileStatusText`]。 */
export const MOBILE_STATUS_TEXT: Record<string, string> = {
  waiting: "等你",
  turn_done: "待查看",
  running: "在跑",
  starting: "启动中",
  idle: "闲着",
  finished: "已结束",
  failed: "失败",
  unknown: "状态待确认",
};

/** 一行手机端的状态词；`seen` 只对 turn_done 有意义（「待查看 / 已读」）。 */
export function mobileStatusText(row: SessionRow, seen: boolean): string {
  if (row.status === "waiting" && row.reason === "permission") return "等你批准";
  if (row.status === "turn_done" && seen) return "已读";
  return MOBILE_STATUS_TEXT[row.status] ?? row.status;
}

/**
 * 手机端的时长：不到一分钟显示秒（`42s`），一分钟起沿用桌面共用的 `formatAgo`（`3m` / `2h` /
 * `5d`）。桌面那份不到 60 s 返回空串——刚发出去的几十秒界面完全静止（2026-10-08 真机反馈第 3 条）。
 * 负值（页面钟比节点快）夹到 `0s`，不显示 `-3s`；非有限值不显示（与 `formatAgo` 一致）。
 * **只有卡片用它**（那里有 1 s 心跳）；收件箱列表走 [`mobileStatusLine`] 的 `"minutes"` 粒度。
 */
export function mobileAgo(seconds: number): string {
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return `${Math.max(0, Math.floor(seconds))}s`;
  return formatAgo(seconds);
}

/**
 * 行文案（状态词 + 时长）：本机精确、peer 行的 `≥` 是并入方给的下界（ADR-004，与桌面同一条语义，
 * 只换了词）。时长口径见 [`mobileAgo`]。
 *
 * `precision` 是时长粒度：卡片有两个面（agora-o975.3 审查修订，2026-10-08）——卡片有 1 s 心跳，
 * 用 `"seconds"`（`在跑 42s`）；收件箱列表 30 s 才走一格，用 `"minutes"`（沿用 `formatAgo`，
 * 不到 60 s 不显示）——列表上显示秒会“冻住”，反而更像死了。默认 `"seconds"` 是卡片那个面。
 */
export type MobileAgoPrecision = "seconds" | "minutes";

export function mobileStatusLine(
  row: SessionRow,
  seen: boolean,
  nowSeconds: number,
  precision: MobileAgoPrecision = "seconds",
): string {
  const text = mobileStatusText(row, seen);
  const s = row.status_since;
  const ago =
    typeof s === "number" ? (precision === "minutes" ? formatAgo(nowSeconds - s) : mobileAgo(nowSeconds - s)) : "";
  if (!ago) return text;
  return isPeerRow(row) ? `${text} ≥${ago}` : `${text} ${ago}`;
}
