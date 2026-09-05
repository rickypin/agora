/**
 * Attention Dashboard 的排序与行文案（MISSION §6.3；ADR-002 D8；agora-dvh.10）。
 *
 * 纯函数，所有客户端形态用同一条规则渲染同样的行。分数：凡是卡在人身上的
 * （FAILED / WAITING / TURN_DONE / FINISHED）高于不需要人的（RUNNING / STARTING），UNKNOWN 排中间。
 * 同分先按任务优先级（bd 的 P0–P4，无 bd 视为 P2），再按等待时长（状态起点越早越靠前）。
 */
import type { SessionRow } from "./events";

export const SCORE: Record<string, number> = {
  failed: 100,
  waiting: 90,
  turn_done: 85,
  finished: 80,
  unknown: 40,
  idle: 30,
  starting: 20,
  running: 10,
};

/** 没有 bd 的会话视为 P2。 */
export const DEFAULT_PRIORITY = 2;

export interface TaskInfo {
  id: string;
  title: string;
  priority: number;
  status?: string;
}

export function attentionScore(status: string): number {
  return SCORE[status] ?? SCORE.unknown;
}

/** NEEDS ATTENTION 区：分数 ≥ FINISHED 的都是"等你"的。 */
export function needsAttention(row: SessionRow): boolean {
  return attentionScore(row.status) >= SCORE.finished;
}

export function taskOf(row: SessionRow): TaskInfo | null {
  const t = row.task;
  if (t && typeof t === "object" && typeof (t as TaskInfo).id === "string") return t as TaskInfo;
  return null;
}

export function taskPriority(row: SessionRow): number {
  const p = taskOf(row)?.priority;
  return typeof p === "number" && p >= 0 && p <= 4 ? p : DEFAULT_PRIORITY;
}

function since(row: SessionRow): number {
  const s = row.status_since;
  return typeof s === "number" ? s : Number.MAX_SAFE_INTEGER;
}

/** 分数降序 → 优先级升序 → 等得久的在前；其余保持原顺序（稳定）。 */
export function sortByAttention(rows: SessionRow[]): SessionRow[] {
  return rows
    .map((row, i) => ({ row, i }))
    .sort((a, b) => {
      const s = attentionScore(b.row.status) - attentionScore(a.row.status);
      if (s !== 0) return s;
      const p = taskPriority(a.row) - taskPriority(b.row);
      if (p !== 0) return p;
      const w = since(a.row) - since(b.row);
      if (w !== 0) return w;
      return a.i - b.i;
    })
    .map((x) => x.row);
}

/** 先 NEEDS ATTENTION 再 RUNNING，各自保持传入顺序：侧栏显示顺序 = 这个顺序（Alt/Option+N 跳的也是它）。 */
export function partitionByAttention(rows: SessionRow[]): SessionRow[] {
  return [...rows.filter(needsAttention), ...rows.filter((r) => !needsAttention(r))];
}

/** 第一列是任务而不是进程名：issue id + 标题 > 首条 prompt 摘要 > display name。 */
export function taskLabel(row: SessionRow): string {
  const t = taskOf(row);
  if (t) return `${t.id} ${t.title}`.trim();
  if (typeof row.task_ref === "string" && row.task_ref.trim()) return row.task_ref.trim();
  return String(row.name ?? row.display_name ?? row.id);
}

/**
 * `❯` 行与任务标签是同一句话：标签退回了首条 prompt 摘要、而当前 prompt 就是那一条（agora-k9r）。
 * 连显两行一模一样的字只是噪音。有 beads 任务时标签是 issue 标题，不算。
 */
export function promptRepeatsLabel(row: SessionRow): boolean {
  if (taskOf(row)) return false;
  const ref = typeof row.task_ref === "string" ? row.task_ref.trim() : "";
  const prompt = typeof row.prompt === "string" ? row.prompt.trim() : "";
  return ref !== "" && ref === prompt;
}

export const STATUS_TEXT: Record<string, string> = {
  waiting: "waiting",
  turn_done: "turn done",
  running: "working",
  starting: "starting",
  idle: "idle",
  finished: "finished",
  failed: "failed",
  unknown: "unknown",
};

/** `3m` / `2h` / `5d`；不到一分钟不显示。 */
export function formatAgo(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 60) return "";
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** "waiting 3m"：状态文案 + 从状态起点算起的时长。 */
export function statusLine(row: SessionRow, nowSeconds: number): string {
  const text = STATUS_TEXT[row.status] ?? row.status;
  const s = row.status_since;
  const ago = typeof s === "number" ? formatAgo(nowSeconds - s) : "";
  return ago ? `${text} ${ago}` : text;
}

export interface Counts {
  running: number;
  needsInput: number;
  turnDone: number;
  finished: number;
  failed: number;
  idle: number;
  unknown: number;
}

/** header 的一行计数（docs/spec/ux.md 线框）。 */
export function countByStatus(rows: SessionRow[]): Counts {
  const c: Counts = { running: 0, needsInput: 0, turnDone: 0, finished: 0, failed: 0, idle: 0, unknown: 0 };
  for (const r of rows) {
    switch (r.status) {
      case "running":
      case "starting":
        c.running += 1;
        break;
      case "waiting":
        c.needsInput += 1;
        break;
      case "turn_done":
        c.turnDone += 1;
        break;
      case "finished":
        c.finished += 1;
        break;
      case "failed":
        c.failed += 1;
        break;
      case "idle":
        c.idle += 1;
        break;
      default:
        c.unknown += 1;
    }
  }
  return c;
}
