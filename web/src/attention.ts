/**
 * Attention Dashboard 的排序、分段与行文案（MISSION §6.3；ADR-002 D8；agora-dvh.10）。
 *
 * 纯函数，所有客户端形态用同一条规则渲染同样的行。分数：凡是卡在人身上的
 * （FAILED / WAITING / TURN_DONE / FINISHED）高于不需要人的（RUNNING / STARTING），UNKNOWN 排中间。
 * 同分先按任务优先级（bd 的 P0–P4，无 bd 视为 P2），再按等待时长（状态起点越早越靠前）——
 * 例外是 TURN_DONE：它段内按完成时间**倒序**，新完成在前（agora-5gg.21）。
 * FINISHED 再分来源与看没看过（`finishedCollapsed`，A46）：收起来的进侧栏末尾默认折叠的 Finished 区。
 * 「看过」2026-09-19 起同样适用于 TURN_DONE（agora-5gg.21，决策 agora-5gg.10）：看过一次降到中段，
 * 新一次完成再回来——但它降进的是 WORKING 段，**不是** Finished 折叠区（理由见 `needsAttention`）。
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
 * 「需要我」的新鲜度窗口（agora-82x7 的另一半，2026-10-08 用户拍板"按你的建议实施"）。
 *
 * 从第一性原理：「需要我」回答的是**现在**需要你——不是"曾经需要过你"。判据分两类：
 * - `waiting` / `failed` 不设上限：那是真的卡着人（一条挂起的权限、一次崩掉的运行），三小时后照样是
 *   现在的事；
 * - `turn_done` / `finished`（"等你回看"的那两种）超过这个窗口就不再占「需要我」：现场 8 行里有 6 行
 *   是 9/20 与 10/6 的旧账，最老 15 天——一块 15 天没动过的终端不是"现在需要你"，它只是把今天真正的事
 *   压下去。降段不等于消失：`turn_done` 落到「在跑」段（与"看过一次"同一个去处，agora-5gg.21），
 *   `finished` 落到折叠的「已完成」区（同时进入 Header「Finished N」一键清理的名单——名单按
 *   `sectionOf === "finished"` 算，与 `finishedCollapsed` 同一个判据）。
 *
 * 时刻取 `peer_status_since ?? status_since`（与 [`seenKey`] 同一个口径：peer 行要用 peer 自己那只钟）；
 * 两个都没有的行（旧节点 / 测试桩）**不判**——"不知道何时完成的"不该被当成"很久以前完成的"。
 */
export const ATTENTION_WINDOW_SECS = 12 * 3600;

/** 这一行"等你"的时刻（unix 秒）；不知道就是 null。 */
function waitedSince(row: { status_since?: unknown; peer_status_since?: unknown }): number | null {
  if (typeof row.peer_status_since === "number") return row.peer_status_since;
  return typeof row.status_since === "number" ? row.status_since : null;
}

/** 还在新鲜度窗口内吗（只有"等你回看"的两种状态会被时间淘汰；不知道时刻的不淘汰）。 */
function fresh(row: SessionRow, now: number): boolean {
  if (row.status !== "turn_done" && row.status !== "finished") return true;
  const since = waitedSince(row);
  if (since === null) return true;
  return now - since <= ATTENTION_WINDOW_SECS;
}

/** 当前时刻（unix 秒）；调用方都能传，缺省用墙上时钟（纯函数留出 `now` 只为了可测）。 */
function wallClock(): number {
  return Math.floor(Date.now() / 1000);
}

/**
 * 「看过」的行集合（MISSION §4.6 三条证据的第 ①；A46，agora-j4w.1；agora-5gg.21 扩到 TURN_DONE）：
 * 是浏览器视图状态，不是服务端字段。空集 = 谁都没看过。
 *
 * 集合里的元素是 `seenKey`（`<id>@<status_since>`）而不是裸 id（agora-23h，2026-09-08 对抗审查）：记号
 * 得跟着"这一次完成"走。Restart 之后 running → finished 若被同一批事件（EventsClient 300 ms 合并窗）或
 * 断线重连的 resync 跳过中间态，裸 id 的记号没机会作废，新结果直接落进收起的 Finished 区而没人看过。
 * 新一次 FINISHED 的 status_since 必然不同（set_at 随 (status, source) 变刷新），键带上它就能认出是新结果。
 * TURN_DONE 同理，且不依赖中间那一下：正常链路是下一轮的 prompt → RUNNING（`UserPromptSubmit`，三家都挂了
 * 这个 hook）→ 再一次 turn.ended，换状态时 set_at 自己就刷新了；而连着两条 turn.ended、中间什么状态都没变的
 * 路径由服务端补上——`turn.ended` / `turn.failed` 自带轮次边界，起点无条件记事件时刻（`src/status/machine.rs`
 * 的 `set_new_turn`，agora-8x6）。这条以前是反的（起点不动、记号不作废、第二轮结果回不到 NEEDS ATTENTION），
 * 所以前端这半边一个字都没改：键的形状本来就对，缺的是服务端把起点推过去。
 */
export type SeenSet = ReadonlySet<string>;

const NO_SEEN: SeenSet = new Set();

/** 「看过」集合的键：这一行的这一次完成。
 *
 * 优先用 `peer_status_since`（peer 行才有）：那是 peer 自己那只表上的 `status_since`，就是
 * "这一次完成"的身份；行上的 `status_since` 是本节点时钟重写过的，本机重启后按新测的钟差重算
 * 可能差 ±1 s（读数是整数秒），拿它当锚已看过的 peer FINISHED 会弹回 NEEDS ATTENTION（agora-cjv）。
 *
 * 本地行用 `status_since`；没有 `status_since` 的行（旧节点 / 测试桩）退化为 `<id>@<status>`——
 * 不带状态的话，同一行的 FINISHED 与 TURN_DONE 记号会撞同一个键、互相顶用（agora-no5）。 */
export function seenKey(row: {
  id: string;
  status: string;
  status_since?: unknown;
  peer_status_since?: unknown;
}): string {
  if (typeof row.peer_status_since === "number") return `${row.id}@${row.peer_status_since}`;
  return typeof row.status_since === "number" ? `${row.id}@${row.status_since}` : `${row.id}@${row.status}`;
}

/**
 * 「看过」只对这两种状态有意义（MISSION §4.6 证据 ①）：FINISHED → 进折叠区，TURN_DONE → 降到中段。
 * 写记号与清理记号两处都问它（`Workspace.tsx`），别处再判一种状态就会和这里对不上。
 */
export function seenRelevant(status: string): boolean {
  return status === "finished" || status === "turn_done";
}

/**
 * `origin = headless` 的行：宿主自己起的一次性会话（`claude -p` / `codex exec`）与宿主内部的子代理
 * （裁决 agora-5gg.7 选 B，实施 agora-5gg.20）。它们确实在跑、偶尔会挂权限，但不是一条等人回看的
 * 会话：一次 handoff 就能堆七行，摆在 NEEDS ATTENTION 里把人的会话冲掉了。
 */
export function isHeadless(row: SessionRow): boolean {
  return row.origin === "headless";
}

/**
 * agora 手里没有运行时句柄的那两种来源：`external`（人在另一个终端窗口里裸跑的 agent）与它的细分
 * `headless`（宿主自己起的无头会话）。两者都没有 pane、没有终端 WS、没有 Restart，能给的只有状态
 * 与经 hook 的 allow / deny。判据是 origin 而不是 `runtime_ref`：那个字段不在行上（`docs/spec/api.md`「外部会话」）。
 * 漏一处，headless 行就会在那一格被当成有终端的会话（`Workspace.tsx` 会去 attach 一个不存在的 pane）。
 */
export function isHandleless(row: SessionRow): boolean {
  return row.origin === "external" || isHeadless(row);
}

/**
 * FINISHED 行分来源（MISSION §6.3 排序表；A46）：
 * - origin = external：一律不算"等你"——它的工作面在别的窗口，人在终端里自己结束了会话（§4.6 证据 ②），
 *   agora 这边没有 pane、没有 Restart，能给的只有两行摘要，所以直接进折叠的 Finished 区。不看 reason
 *   分类：Claude / Grok 连关窗口都发 SessionEnd、Codex 关窗口不发（bd memories external-exit-hooks-ctrlc-vs-hup，
 *   2026-09-08 实测），按 reason 分既不可靠也不必要。
 * - origin = agora / adopted：看过（选中展开过一次）之前算"等你"，看过之后进 Finished 区。
 * - origin = headless：不看状态、不看看过，一律收起（见 [`isHeadless`]）。
 *
 * 折叠区的行是 Header「Finished N」一键清理的删除名单（`Sidebar.tsx` 的 `clearable` 按
 * `sectionOf === "finished"` 算），所以一行还在跑的无头会话也可能被那一键删掉记录——它本来就满
 * 24 h 不论状态都会被 `expire_external_finished` 自动删（agora-5gg.20），人手动删同一行不是新权力。
 */
export function finishedCollapsed(row: SessionRow, seen: SeenSet = NO_SEEN, now: number = wallClock()): boolean {
  // headless 不看状态：它从来不是「等你回看结果」（§4.6 的三条证据一条都不成立：没有工作面、
  // 没有人结束过它、也没有人会去选中它），停在 TURN_DONE / UNKNOWN 也一样进折叠区。
  if (isHeadless(row)) return true;
  if (row.status !== "finished") return false;
  // 超过新鲜度窗口的：不再占「需要我」，进折叠区（见 `ATTENTION_WINDOW_SECS`）。
  return row.origin === "external" || seen.has(seenKey(row)) || !fresh(row, now);
}

/**
 * NEEDS ATTENTION 区：分数 ≥ FINISHED 的都是"等你"的——除了已收进 Finished 区的 FINISHED 行
 * （`finishedCollapsed`），也除了**看过一次**的 TURN_DONE 行（agora-5gg.21，决策 agora-5gg.10）。
 *
 * 看过的 TURN_DONE 降到 WORKING 段而**不是** Finished 折叠区，两件硬理由：① 折叠区的行是 Header
 * 「Finished N」一键清理的删除对象（`Sidebar.tsx` 的 `clearable` 按 `sectionOf === "finished"` 算），
 * 那一行只是这一轮做完了、pane 里的进程还活着、下一条指令随时要发——把它算进可清理集合就会删掉活会话的记录；
 * ② 折叠区默认收起，看过的 TURN_DONE 收进去就等于再也回不来（新一轮完成靠新记号回到 NEEDS
 * ATTENTION，但人在「按项目」视图与折叠区里根本看不见它）。原问题（zuan capmaster 三行 208–255 h 的
 * 旧完成永远压在刚做完的行上面）是"压着"，不是"该藏起来"。
 *
 * 不看 origin（headless 除外，它在上面就被 `finishedCollapsed` 收走了）：external 的 TURN_DONE 也降。
 * FINISHED 那边 external 一律直接收起靠的是证据 ②（人在终端里
 * 自己结束了会话，结束即看过），TURN_DONE 没有这条——它还在跑，工作面在不在 agora 都得人瞟一眼，
 * 所以两种来源都走"选中看过一次才降"。
 */
export function needsAttention(row: SessionRow, seen: SeenSet = NO_SEEN, now: number = wallClock()): boolean {
  if (attentionScore(row.status) < SCORE.finished) return false;
  if (finishedCollapsed(row, seen, now)) return false;
  // turn_done 降段的两条路：看过一次（agora-5gg.21）或已经超过新鲜度窗口（`ATTENTION_WINDOW_SECS`）。
  if (row.status === "turn_done" && (seen.has(seenKey(row)) || !fresh(row, now))) return false;
  return true;
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

/**
 * 分数降序 → 优先级升序 → 等得久的在前；其余保持原顺序（稳定）。
 *
 * TURN_DONE 段内方向反过来（agora-5gg.21，决策 agora-5gg.10 的可选项目 A）："等得久的在前"是为 WAITING
 * 的公平性设计的（谁先卡住谁先被看到），套到"这一轮做完了等你回看"上语义是反的——zuan 的 capmaster 三行
 * 208–255 h 的旧完成永远压在刚做完的行上面。WAITING / FAILED 仍升序。同分必然同状态（SCORE 一一对应），
 * 所以取 a 的状态判方向就够。
 *
 * 没有 `status_since` 的行（旧节点 / 测试桩）先按"不知道何时完成"排在有时刻的行之后，与方向无关：
 * 别让"不知道"冒充"最新完成"、钉在段首。两个时刻都没了才比原顺序（稳定）。
 */
export function sortByAttention(rows: SessionRow[]): SessionRow[] {
  return rows
    .map((row, i) => ({ row, i }))
    .sort((a, b) => {
      const s = attentionScore(b.row.status) - attentionScore(a.row.status);
      if (s !== 0) return s;
      const p = taskPriority(a.row) - taskPriority(b.row);
      if (p !== 0) return p;
      // 时刻缺省的行一律排后面（与方向无关），再按方向比时刻：TURN_DONE 倒序、其余升序。
      const known = (r: SessionRow) => (typeof r.status_since === "number" ? 0 : 1);
      const k = known(a.row) - known(b.row);
      if (k !== 0) return k;
      const w = (a.row.status === "turn_done" ? -1 : 1) * (since(a.row) - since(b.row));
      if (w !== 0) return w;
      return a.i - b.i;
    })
    .map((x) => x.row);
}

/**
 * 侧栏的四段：NEEDS ATTENTION → UNCLEAR（说不清的行）→ WORKING（不需要人的一切 + 看过一次的
 * TURN_DONE）→ FINISHED（收起来的已完成，默认折叠）。四段是 agora-5gg.11：2026-09-18 Mac 截图上
 * 叫 RUNNING 的那一段 9 行没有一行在跑（UNKNOWN / STARTING / IDLE 全塞在里面），段名名不副实。
 * **分数表一个字没动**——只改分段：`unknown` 单独成段，`running / starting / idle` 与看过的
 * TURN_DONE 共用 WORKING 段。看过的 TURN_DONE 落中段是 agora-5gg.21。
 */
export type Section = "attention" | "unclear" | "working" | "finished";

/**
 * 「说不清」的行：状态 UNKNOWN，以及一切我们不认识、分数落到 unknown 档的状态名（旧节点报来新状态）。
 * UNCLEAR 段的判据，也是行上那句 reason + 出口提示的判据（`SessionRow.tsx`）——两边问同一个函数，
 * 不会出现「段里没有 reason 的行」。
 *
 * 为什么单独成段而不是留在中段（agora-5gg.11）：UNKNOWN 的分数 40 本来就高于 IDLE / STARTING / RUNNING
 * （MISSION §6.3「看不清，值得瞟一眼」），混在中段里却被段名说成"在跑"；段名换成 WORKING 也还是混。
 * 出口（`unknown_cause` 封闭枚举、TTL 淘汰）归 5gg.6 / e08，这一步只管把它摆到看得见的地方。
 */
export function unclearStatus(status: string): boolean {
  return status === "unknown" || SCORE[status] === undefined;
}

export function sectionOf(row: SessionRow, seen: SeenSet = NO_SEEN, now: number = wallClock()): Section {
  if (finishedCollapsed(row, seen, now)) return "finished";
  if (needsAttention(row, seen, now)) return "attention";
  return unclearStatus(row.status) ? "unclear" : "working";
}

/**
 * 先 NEEDS ATTENTION，再 UNCLEAR，再 WORKING，最后 FINISHED 折叠区，各段内部保持传入顺序：侧栏显示顺序
 * = 这个顺序，Alt/Option+N 跳的也是它——折叠区收起时行不画，序号照数（折叠与否不改变第 N 条是谁，A46）。
 * 段与段之间不重排、段内也不丢行，所以序号是 1…N 连续的一条线（每段各有标题时标题也不占序号）。
 */
export function partitionByAttention(
  rows: SessionRow[],
  seen: SeenSet = NO_SEEN,
  now: number = wallClock(),
): SessionRow[] {
  const order: Section[] = ["attention", "unclear", "working", "finished"];
  return order.flatMap((section) => rows.filter((r) => sectionOf(r, seen, now) === section));
}

/**
 * 「看过」集合的持久化（localStorage；agora-j4w.1，agora-5gg.21 起也存 TURN_DONE 的记号）：换个浏览器 /
 * 清了站点数据就从头算——它只是视图状态，
 * 丢了的代价是几行 FINISHED / TURN_DONE 回到 NEEDS ATTENTION 再看一眼。读写都包 try/catch：隐私窗口、被禁的存储
 * 访问 `localStorage` 本身会抛。
 *
 * 键名留着 `agora.seen-finished` 没改：`seenKey` = `<id>@<status_since>` 本来就是"这一行的这一次完成"，
 * 与状态无关，同一集合直接复用（决策 agora-5gg.10）；改键名会把老浏览器里已看的 FINISHED 记号全丢掉，
 * 那些行会集体回到 NEEDS ATTENTION——为了一个名字不值得。
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

/**
 * 这一行是从 peer 并入的吗。判据是 `stale` 键在不在：它只出现在 peer 行上，本机行从来没有
 * （docs/spec/api.md「peer 视图」）——在线的 peer 行是 `stale: false`，不是缺键。
 */
export function isPeerRow(row: SessionRow): boolean {
  return typeof row.stale === "boolean";
}

/** "waiting 3m"：状态文案 + 从状态起点算起的时长。 */
export function statusLine(row: SessionRow, nowSeconds: number): string {
  const text = STATUS_TEXT[row.status] ?? row.status;
  const s = row.status_since;
  const ago = typeof s === "number" ? formatAgo(nowSeconds - s) : "";
  if (!ago) return text;
  // peer 行的时长是一个**下界**：起点由并入它的这台机器在「第一次看见这个状态」的那一刻打
  // （ADR-004：不信 peer 报的绝对时刻，只用它报的时长差把起点往前推），真实起点只会更早。
  // 画成精确值就是撒谎——2026-09-18 Mac 的 daemon 重启把并入视图清空，zuan 那 24 行 8 天的
  // STARTING 全成了 0.7 h（agora-5gg.12）。本机行的起点是自己打的，精确，不带 ≥。
  return isPeerRow(row) ? `${text} ≥${ago}` : `${text} ${ago}`;
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
