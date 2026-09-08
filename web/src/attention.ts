/**
 * Attention Dashboard 的排序与行文案（MISSION §6.3；ADR-002 D8；agora-dvh.10）。
 *
 * 纯函数，所有客户端形态用同一条规则渲染同样的行。分数：凡是卡在人身上的
 * （FAILED / WAITING / TURN_DONE / FINISHED）高于不需要人的（RUNNING / STARTING），UNKNOWN 排中间。
 * 同分先按任务优先级（bd 的 P0–P4，无 bd 视为 P2），再按等待时长（状态起点越早越靠前）。
 * FINISHED 再分来源与看没看过（`finishedCollapsed`，A46）：收起来的进侧栏末尾默认折叠的 Finished 区。
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
  /** beads 里的验收标准全文（`bd show --json` 的 acceptance_criteria），没写就是 null（agora-h1k.3）。 */
  acceptance?: string | null;
}

export function attentionScore(status: string): number {
  return SCORE[status] ?? SCORE.unknown;
}

/**
 * 「看过」的 FINISHED 行集合（MISSION §4.6 三条证据的第 ①；A46，agora-j4w.1）：会话 id 的集合，
 * 是浏览器视图状态，不是服务端字段。空集 = 谁都没看过。
 *
 * 集合里的元素是 `seenKey`（`<id>@<status_since>`）而不是裸 id（agora-23h，2026-09-08 对抗审查）：记号
 * 得跟着"这一次完成"走。Restart 之后 running → finished 若被同一批事件（EventsClient 300 ms 合并窗）或
 * 断线重连的 resync 跳过中间态，裸 id 的记号没机会作废，新结果直接落进收起的 Finished 区而没人看过。
 * 新一次 FINISHED 的 status_since 必然不同（set_at 随 (status, source) 变刷新），键带上它就能认出是新结果。
 */
export type SeenSet = ReadonlySet<string>;

const NO_SEEN: SeenSet = new Set();

/** 「看过」集合的键：这一行的这一次完成。没有 status_since 的行（旧节点 / 测试桩）退化为 `<id>@`。 */
export function seenKey(row: { id: string; status_since?: unknown }): string {
  return `${row.id}@${typeof row.status_since === "number" ? row.status_since : ""}`;
}

/**
 * FINISHED 行分来源（MISSION §6.3 排序表；A46）：
 * - origin = external：一律不算"等你"——它的工作面在别的窗口，人在终端里自己结束了会话（§4.6 证据 ②），
 *   agora 这边没有 pane、没有 Restart，能给的只有两行摘要，所以直接进折叠的 Finished 区。不看 reason
 *   分类：Claude / Grok 连关窗口都发 SessionEnd、Codex 关窗口不发（bd memories external-exit-hooks-ctrlc-vs-hup，
 *   2026-09-08 实测），按 reason 分既不可靠也不必要。
 * - origin = agora / adopted：看过（选中展开过一次）之前算"等你"，看过之后进 Finished 区。
 */
export function finishedCollapsed(row: SessionRow, seen: SeenSet = NO_SEEN): boolean {
  if (row.status !== "finished") return false;
  return row.origin === "external" || seen.has(seenKey(row));
}

/** NEEDS ATTENTION 区：分数 ≥ FINISHED 的都是"等你"的——除了已收进 Finished 区的 FINISHED 行（`finishedCollapsed`）。 */
export function needsAttention(row: SessionRow, seen: SeenSet = NO_SEEN): boolean {
  return attentionScore(row.status) >= SCORE.finished && !finishedCollapsed(row, seen);
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

/** 侧栏的三段：NEEDS ATTENTION → RUNNING（不需要人的一切）→ FINISHED（收起来的已完成，默认折叠）。 */
export type Section = "attention" | "running" | "finished";

export function sectionOf(row: SessionRow, seen: SeenSet = NO_SEEN): Section {
  if (finishedCollapsed(row, seen)) return "finished";
  return needsAttention(row, seen) ? "attention" : "running";
}

/**
 * 先 NEEDS ATTENTION，再 RUNNING，最后 FINISHED 折叠区，各段内部保持传入顺序：侧栏显示顺序 = 这个顺序，
 * Alt/Option+N 跳的也是它——折叠区收起时行不画，序号照数（折叠与否不改变第 N 条是谁，A46）。
 */
export function partitionByAttention(rows: SessionRow[], seen: SeenSet = NO_SEEN): SessionRow[] {
  const order: Section[] = ["attention", "running", "finished"];
  return order.flatMap((section) => rows.filter((r) => sectionOf(r, seen) === section));
}

/**
 * 「看过」集合的持久化（localStorage；agora-j4w.1）：换个浏览器 / 清了站点数据就从头算——它只是视图状态，
 * 丢了的代价是几行 FINISHED 回到 NEEDS ATTENTION 再看一眼。读写都包 try/catch：隐私窗口、被禁的存储
 * 访问 `localStorage` 本身会抛。
 */
export const SEEN_STORAGE_KEY = "agora.seen-finished";

export function loadSeen(storage: Pick<Storage, "getItem"> | null = safeStorage()): Set<string> {
  try {
    const raw = storage?.getItem(SEEN_STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return new Set(Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function storeSeen(seen: SeenSet, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(SEEN_STORAGE_KEY, JSON.stringify([...seen]));
  } catch {
    // 存不下就算了：下次打开页面再看一眼而已。
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
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
