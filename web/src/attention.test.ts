import { describe, expect, it } from "vitest";
import {
  ATTENTION_WINDOW_SECS,
  countByStatus,
  finishedCollapsed,
  formatAgo,
  isPeerRow,
  loadSeen,
  seenKey,
  needsAttention,
  partitionByAttention,
  sectionOf,
  SEEN_STORAGE_KEY,
  sortByAttention,
  statusLine,
  storeSeen,
  taskLabel,
  unclearStatus,
} from "./attention";
import type { SessionRow } from "./events";

function row(id: string, status: string, extra: Record<string, unknown> = {}): SessionRow {
  return { id, node: "n", status, alive: true, ...extra };
}

/**
 * 固定的时间基准（秒），**只用于显式传 `now`**。
 *
 * 2026-10-08 新鲜度窗口（`ATTENTION_WINDOW_SECS`）落地后，时间成了分段判据的一部分：测试里那些
 * `status_since: 5/7/100` 是配合一个固定的 `NOW` 才成立的字面量（相对墙上时钟它们是"十几年前"）。
 * 所以凡是问分段/降段的地方都把 `NOW` 显式传进去（纯函数留出 `now` 正是为了这个），字面时刻一个都不动——
 * 改字面量会连带把"越大越新"的相对顺序改反（第一次改就是这么翻车的）。
 */
const NOW = 400;

describe("attention", () => {
  it("puts whatever is stuck on a human above whatever is running (A17)", () => {
    // MISSION §6.3 的分数表：FAILED 100 / WAITING 90 / TURN_DONE 85 / FINISHED 80 / UNKNOWN 40 /
    // IDLE 30 / STARTING 20 / RUNNING 10。
    const rows = [
      row("run", "running"),
      row("idle", "idle"),
      row("fin", "finished"),
      row("wait", "waiting"),
      row("start", "starting"),
      row("unk", "unknown"),
      row("done", "turn_done"),
      row("fail", "failed"),
    ];
    expect(sortByAttention(rows).map((r) => r.id)).toEqual(["fail", "wait", "done", "fin", "unk", "idle", "start", "run"]);
    expect(rows.filter((r) => needsAttention(r, undefined, NOW)).map((r) => r.id)).toEqual(["fin", "wait", "done", "fail"]);
  });

  // A46（agora-j4w.1）：FINISHED 分来源与看没看过。external 的工作面在别的窗口，人在终端里自己结束了会话
  // （MISSION §4.6 证据 ②），agora 这边没有 pane 也没有 Restart——一律直接收进 Finished 区；agora / adopted
  // 来源的要等人在侧栏里选中展开过一次（证据 ①）。
  it("external FINISHED never needs attention; agora/adopted FINISHED does until seen (A46)", () => {
    const ext = row("ext", "finished", { origin: "external" });
    const own = row("own", "finished", { origin: "agora" });
    const adopted = row("ad", "finished", { origin: "adopted" });
    const none = new Set<string>();
    expect(needsAttention(ext, undefined, NOW)).toBe(false);
    expect(needsAttention(ext, none, NOW)).toBe(false);
    expect(finishedCollapsed(ext, undefined, NOW)).toBe(true);
    expect(needsAttention(own, none, NOW)).toBe(true);
    expect(needsAttention(adopted, none, NOW)).toBe(true);
    expect(finishedCollapsed(own, none, NOW)).toBe(false);
    const seen = new Set([seenKey(own)]);
    expect(needsAttention(own, seen, NOW)).toBe(false);
    expect(finishedCollapsed(own, seen, NOW)).toBe(true);
    expect(needsAttention(adopted, seen, NOW)).toBe(true);
    // 记号跟着这一次完成走（agora-23h）：同一行、新的 status_since 就是新结果，旧记号不算。
    // 没有 status_since 的退化键带状态（agora-no5）：bare `own@` 会让同 id 的 FINISHED / TURN_DONE 撞键。
    expect(seenKey(own)).toBe("own@finished");
    expect(seenKey(row("own", "finished", { status_since: 7 }))).toBe("own@7");
    expect(finishedCollapsed(row("own", "finished", { origin: "agora", status_since: 7 }), seen, NOW)).toBe(false);
    // 看过只对 FINISHED 有意义：WAITING 行在集合里也照样"等你"，RUNNING 行也不会因此进 Finished 区。
    expect(needsAttention(row("w", "waiting"), new Set(["w@waiting"]), NOW)).toBe(true);
    expect(finishedCollapsed(row("r", "running", { origin: "external" }), undefined, NOW)).toBe(false);
    expect(sectionOf(ext, undefined, NOW)).toBe("finished");
    expect(sectionOf(own, none, NOW)).toBe("attention");
    expect(sectionOf(own, seen, NOW)).toBe("finished");
    expect(sectionOf(row("r", "running"), undefined, NOW)).toBe("working");
  });

  // agora-5gg.21（决策 agora-5gg.10 选 B 为主）：A46 的「看过」扩到 TURN_DONE——选中看过一次即降到中段，
  // 新一次 TURN_DONE（新的 status_since）再回来。关键是它降进 WORKING 段（四段化前叫 RUNNING 段）而
  // **不是** Finished 折叠区：折叠区的行是 Header「Finished N」一键清理的删除对象，而那一行 pane 里的进程还活着。
  // 裁决 agora-5gg.7 选 B（实施 agora-5gg.20）：宿主自己起的无头一轮（`claude -p`）与内部子代理
  // 登记成 `origin = headless`。它们不是一条等人回看的会话，所以**不看状态**一律进折叠区。
  it("headless rows fold whatever status they are in (agora-5gg.7 B → agora-5gg.20)", () => {
    const none = new Set<string>();
    for (const status of ["failed", "waiting", "turn_done", "finished", "unknown"]) {
      const r = row(`h-${status}`, status, { origin: "headless" });
      expect(finishedCollapsed(r, none, NOW), status).toBe(true);
      expect(needsAttention(r, none, NOW), status).toBe(false);
      expect(sectionOf(r, none, NOW), status).toBe("finished");
    }
    // running / idle 本来就不进 NEEDS ATTENTION，但它们也不因 origin 落进 UNCLEAR：
    // 折叠在 sectionOf 的第一条判断里就了断了。
    expect(sectionOf(row("h-run", "running", { origin: "headless" }), none, NOW)).toBe("finished");
    // 记号无效：headless 行即使被选中展开过也不换地方（它从来不是"看过就不再弹"那种）。
    const hl = row("h-seen", "turn_done", { origin: "headless", status_since: 5 });
    expect(sectionOf(hl, new Set([seenKey(hl)]), NOW)).toBe("finished");
    // 反过来：同样状态的 external / agora 行不因这条改动改掉归属。
    expect(sectionOf(row("x", "turn_done", { origin: "external", status_since: 5 }), none, NOW)).toBe("attention");
    expect(sectionOf(row("a", "unknown", { origin: "agora" }), none, NOW)).toBe("unclear");
    // 分段拼接：无头行永远排到最后，与分数无关（它的分数比 running 高）。
    const rows = [row("r", "running"), row("h", "waiting", { origin: "headless" }), row("w", "waiting")];
    expect(partitionByAttention(sortByAttention(rows), none, NOW).map((r) => r.id)).toEqual(["w", "r", "h"]);
    // 计数跟着显示走（agora-2x3z）：无头行不论什么状态都收在 FINISHED 区，所以只进 Finished，
    // 不再进 Running / Needs Input / …——否则 `Running N` 里有一行在侧栏哪一段都找不到。
    const counts = countByStatus(rows);
    expect(counts.needsInput).toBe(1);
    expect(counts.finished).toBe(1);
  });

  // agora-2x3z（与 agora-l4r3 同一处口径）：计数的 Finished 跟着无头行被显示的那一段走，
  // 状态是 `running` 的无头行不进 Running。守卫的原例：在跑的 headless 会话不进 Running。
  it("counts a running headless row under Finished where it is drawn, never under Running (agora-2x3z)", () => {
    const rows = [
      row("run", "running"),
      row("h-run", "running", { origin: "headless" }),
      row("h-done", "turn_done", { origin: "headless" }),
      row("done", "turn_done", { origin: "agora", status_since: NOW - 60 }),
    ];
    const c = countByStatus(rows);
    expect(c.running).toBe(1);
    expect(c.turnDone).toBe(1);
    expect(c.finished).toBe(2);
    // 它们不是消失了：无头行仍然画在 Finished 折叠区里（分段与计数同源）。
    expect(sectionOf(rows[1], undefined, NOW)).toBe("finished");
    expect(sectionOf(rows[2], undefined, NOW)).toBe("finished");
  });

  it("a seen TURN_DONE row drops to the WORKING segment, never into the Finished folding (agora-5gg.21)", () => {
    const done = row("d", "turn_done", { origin: "agora", status_since: 10 });
    const extDone = row("x", "turn_done", { origin: "external", status_since: 20 });
    const none = new Set<string>();
    expect(needsAttention(done, none, NOW)).toBe(true);
    expect(sectionOf(done, none, NOW)).toBe("attention");
    const seen = new Set([seenKey(done), seenKey(extDone)]);
    expect(needsAttention(done, seen, NOW)).toBe(false);
    expect(sectionOf(done, seen, NOW)).toBe("working");
    // 不许进折叠区：`sectionOf === "finished"` 是 Sidebar 一键清理逐行发 DELETE 的依据。
    expect(finishedCollapsed(done, seen, NOW)).toBe(false);
    expect(sectionOf(done, seen, NOW)).not.toBe("finished");
    // external 的 TURN_DONE 一样降（不看 origin）：FINISHED 那边 external 直接收起靠的是证据 ②（人在终端里
    // 自己结束了会话），TURN_DONE 的进程还在，工作面在哪都得人自己瞟一眼。
    expect(needsAttention(extDone, seen, NOW)).toBe(false);
    expect(sectionOf(extDone, seen, NOW)).toBe("working");
    // 新一次完成：status_since 不同 → 旧记号作废，回到 NEEDS ATTENTION。
    expect(needsAttention(row("d", "turn_done", { origin: "agora", status_since: 11 }), seen, NOW)).toBe(true);
    expect(sectionOf(row("d", "turn_done", { origin: "agora", status_since: 11 }), seen, NOW)).toBe("attention");
    // 记号不越界：同一行的 WAITING 不会被 TURN_DONE 的记号压下去。
    expect(needsAttention(row("d", "waiting", { status_since: 10 }), seen, NOW)).toBe(true);
    // 分段：看过的 TURN_DONE 进 WORKING 段（分数 85 高于 running，所以在本段最前），行不丢、折叠区不涨。
    const rows = [row("run", "running"), done, extDone, row("fin", "finished", { origin: "external" })];
    const sorted = sortByAttention(rows);
    const shown = partitionByAttention(sorted, seen, NOW);
    expect(shown.map((r) => r.id)).toEqual(["x", "d", "run", "fin"]);
    expect(shown.length).toBe(rows.length);
    expect(shown.filter((r) => sectionOf(r, seen, NOW) === "finished").map((r) => r.id)).toEqual(["fin"]);
  });

  it("brings a row back to NEEDS ATTENTION on a second TURN_DONE with nothing in between (agora-8x6)", () => {
    // 连着两轮 turn.ended、中间没有 prompt 也没有任何别的状态变化：行还是 turn_done，只有 status_since
    // 往前走（服务端的 `set_new_turn`，`src/status/machine.rs`）。第二轮的结果必须回到 NEEDS ATTENTION——
    // 那正是人打开页面要找的东西。
    const first = row("d", "turn_done", { origin: "agora", status_since: 10 });
    const none = new Set<string>();
    expect(sectionOf(first, none, NOW)).toBe("attention");
    // 人选中看了一眼 → 记这一次完成的记号 → 降到 WORKING 段。
    const seen = new Set([seenKey(first)]);
    expect(sectionOf(first, seen, NOW)).toBe("working");
    // 第二轮做完：同一行、同一个状态，起点换了 → 旧记号作废，回到 NEEDS ATTENTION。
    const second = row("d", "turn_done", { origin: "agora", status_since: 40 });
    expect(seenKey(second)).not.toBe(seenKey(first));
    expect(needsAttention(second, seen, NOW)).toBe(true);
    expect(sectionOf(second, seen, NOW)).toBe("attention");
    expect(partitionByAttention([row("run", "running"), second], seen, NOW).map((r) => r.id)).toEqual(["d", "run"]);
    // 这条测的是前端跟着起点走，不是前端自己认回合：起点原地不动（修复前的服务端）时它仍然算看过，
    // 所以服务端那半边（`tests/state_machine.rs::a_second_turn_ended_moves_status_since_so_the_seen_mark_expires`）
    // 不能省——两边合起来才是这条不变量。
    expect(sectionOf(row("d", "turn_done", { origin: "agora", status_since: 10 }), seen, NOW)).toBe("working");
  });

  // agora-5gg.11：段名不副实（2026-09-18 Mac 截图上叫 RUNNING 的那一段 9 行没有一行在跑——UNKNOWN /
  // STARTING / IDLE 全塞在里面）。四段 NEEDS ATTENTION / UNCLEAR / WORKING / FINISHED，**分数表一个字没动**，
  // 只改 sectionOf 的分段：unknown 单独成段，running / starting / idle 与看过的 TURN_DONE 共用 WORKING 段。
  it("gives UNKNOWN its own UNCLEAR segment and renames the rest to WORKING without touching the scores (agora-5gg.11)", () => {
    expect(sectionOf(row("u", "unknown"), undefined, NOW)).toBe("unclear");
    // 不认识的状态名（旧节点报来的新状态）分数落到 unknown 档，段也跟着走：`unclearStatus` 是段与行
    // （`SessionRow.tsx` 那句 reason + 出口）共用的唯一判据，两边不会各判一套。
    expect(sectionOf(row("v", "detached"), undefined, NOW)).toBe("unclear");
    expect(unclearStatus("unknown")).toBe(true);
    expect(unclearStatus("detached")).toBe(true);
    expect(unclearStatus("idle")).toBe(false);
    for (const status of ["running", "starting", "idle"]) {
      expect(sectionOf(row(`w-${status}`, status), undefined, NOW)).toBe("working");
    }
    // 看过的 TURN_DONE 降进 WORKING 段（agora-5gg.21 的落点）：段名换了，归属一个字没换。
    const done = row("d", "turn_done", { origin: "agora", status_since: 10 });
    expect(sectionOf(done, undefined, NOW)).toBe("attention");
    expect(sectionOf(done, new Set([seenKey(done)]), NOW)).toBe("working");
    // UNKNOWN 不因人手而离开 UNCLEAR：分数 40 低于 FINISHED 80，所以既不进 NEEDS ATTENTION，
    // 也不因来源 / 记号进折叠区（折叠区是一键清理的删除名单，说不清的行不能被顺手删掉）。
    expect(needsAttention(row("u", "unknown"), undefined, NOW)).toBe(false);
    expect(finishedCollapsed(row("u", "unknown", { origin: "external" }), undefined, NOW)).toBe(false);
    expect(sectionOf(row("u", "unknown", { origin: "external" }), new Set(["u@"]), NOW)).toBe("unclear");
    // 反过来：需要人的行不因四段化改掉归属。
    expect(sectionOf(row("f", "failed"), undefined, NOW)).toBe("attention");
    expect(sectionOf(row("w", "waiting"), undefined, NOW)).toBe("attention");
    expect(sectionOf(row("x", "finished", { origin: "external" }), undefined, NOW)).toBe("finished");
  });

  // 验收：Alt/Option+N 的序号跨段连续——分段只改归属、不产生空位也不重复计数（`Sidebar.tsx` 的
  // ordinal={i+1} 取的是拼好的 rows 下标，标题是插在段首的 <li>、不占序号）。
  it("keeps Alt/Option+N ordinals continuous across all four sections (agora-5gg.11)", () => {
    const rows = [
      row("fail", "failed"),
      row("wait", "waiting"),
      row("unk", "unknown"),
      row("idle", "idle"),
      row("start", "starting"),
      row("run", "running"),
      row("ext", "finished", { origin: "external" }),
    ];
    const shown = partitionByAttention(sortByAttention(rows), undefined, NOW);
    expect(shown.map((r) => r.id)).toEqual(["fail", "wait", "unk", "idle", "start", "run", "ext"]);
    // 序号就是下标 +1：1…7 连续，UNCLEAR 段插在中间不会把后面的序号顶乱或留空洞。
    expect(shown.map((_r, i) => i + 1)).toEqual([1, 2, 3, 4, 5, 6, 7]);
    // 每种状态各占一段、段与段之间不交叉（Sidebar 的表头按 indexOf 推，全靠这个连续性）。
    expect(shown.map((r) => sectionOf(r, undefined, NOW))).toEqual([
      "attention",
      "attention",
      "unclear",
      "working",
      "working",
      "working",
      "finished",
    ]);
    // 折叠区收着时行不画，序号照数：收起与展开的第 N 条是同一条（A46）。
    expect(shown.length).toBe(rows.length);
  });

  // agora-5gg.21 的可选项目 A（决策 agora-5gg.10）：TURN_DONE 段内按完成时间倒序，WAITING / FAILED 仍升序。
  it("orders TURN_DONE by newest completion first while WAITING and FAILED keep waiting-longest first (agora-5gg.21)", () => {
    const rows = [
      row("w-old", "waiting", { status_since: 100 }),
      row("w-new", "waiting", { status_since: 300 }),
      row("f-old", "failed", { status_since: 100 }),
      row("f-new", "failed", { status_since: 300 }),
      row("t-old", "turn_done", { status_since: 100 }),
      row("t-mid", "turn_done", { status_since: 200 }),
      row("t-new", "turn_done", { status_since: 300 }),
      row("t-none", "turn_done"), // 旧节点 / 测试桩：不知道何时完成，两个方向都排最后
    ];
    expect(sortByAttention(rows).map((r) => r.id)).toEqual([
      "f-old",
      "f-new",
      "w-old",
      "w-new",
      "t-new",
      "t-mid",
      "t-old",
      "t-none",
    ]);
    // 优先级仍在时长之前：P0 的旧完成不会让 P4 的新完成压住（只改方向不改三层顺序）。
    const byPriority = [
      row("t-p4-new", "turn_done", { task: { id: "x-4", title: "t", priority: 4 }, status_since: 900 }),
      row("t-p0-old", "turn_done", { task: { id: "x-0", title: "t", priority: 0 }, status_since: 100 }),
    ];
    expect(sortByAttention(byPriority).map((r) => r.id)).toEqual(["t-p0-old", "t-p4-new"]);
  });

  it("four-way partition keeps sortByAttention order inside every segment (Alt/Option+N invariant, A46)", () => {
    // 故意乱序传入：sortByAttention 先排，partition 只分段不换序。
    const rows = [
      row("run", "running", { status_since: 5 }),
      row("ext-fin", "finished", { origin: "external", status_since: 1 }),
      row("seen-fin", "finished", { origin: "agora", status_since: 2 }),
      row("wait", "waiting"),
      row("unk", "unknown"),
      row("new-fin", "finished", { origin: "adopted", status_since: 3 }),
      row("idle", "idle"),
      row("ext-fin-2", "finished", { origin: "external", status_since: 4 }),
      row("fail", "failed"),
    ];
    const seen = new Set(["seen-fin@2"]);
    const sorted = sortByAttention(rows);
    const shown = partitionByAttention(sorted, seen, NOW);
    // 四段各自等于 sortByAttention 顺序按段过滤的结果，拼起来就是显示顺序。
    const by = (section: string) => sorted.filter((r) => sectionOf(r, seen, NOW) === section).map((r) => r.id);
    expect(shown.map((r) => r.id)).toEqual([...by("attention"), ...by("unclear"), ...by("working"), ...by("finished")]);
    expect(shown.map((r) => r.id)).toEqual(["fail", "wait", "new-fin", "unk", "idle", "run", "ext-fin", "seen-fin", "ext-fin-2"]);
    // 一行不多一行不少：折叠区的行仍在序列里，序号照数。
    expect(shown.length).toBe(rows.length);
    // 过滤（只删不换序）与分段可交换：过滤后再分段 = 分段后再过滤，Alt/Option+N 在过滤后仍指向眼睛看到的第 N 条。
    const keep = (r: SessionRow) => r.id !== "wait" && r.id !== "ext-fin";
    expect(partitionByAttention(sorted.filter(keep), seen, NOW).map((r) => r.id)).toEqual(shown.filter(keep).map((r) => r.id));
    // 不传 seen：agora 来源的 FINISHED 全在 NEEDS ATTENTION，external 的仍收起。
    expect(partitionByAttention(sorted, undefined, NOW).map((r) => r.id)).toEqual(["fail", "wait", "seen-fin", "new-fin", "unk", "idle", "run", "ext-fin", "ext-fin-2"]);
  });

  it("persists the seen set defensively: bad JSON, missing storage and throwing storage all read as empty", () => {
    const mem = new Map<string, string>();
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
    expect(loadSeen(storage).size).toBe(0);
    storeSeen(new Set(["a", "b"]), storage);
    expect(mem.get(SEEN_STORAGE_KEY)).toBe(JSON.stringify(["a", "b"]));
    expect([...loadSeen(storage)]).toEqual(["a", "b"]);
    mem.set(SEEN_STORAGE_KEY, "{not json");
    expect(loadSeen(storage).size).toBe(0);
    mem.set(SEEN_STORAGE_KEY, JSON.stringify(["x", 3, null]));
    expect([...loadSeen(storage)]).toEqual(["x"]);
    expect(loadSeen(null).size).toBe(0);
    const throwing = {
      getItem: (): string | null => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadSeen(throwing).size).toBe(0);
    expect(() => storeSeen(new Set(["a"]), throwing)).not.toThrow();
  });

  it("breaks ties by bd priority, then by how long it has waited, then keeps order (A23)", () => {
    const rows = [
      row("p2-late", "waiting", { status_since: 200 }),
      row("nobd", "waiting", { status_since: 100 }), // 无 bd 视为 P2
      row("p3", "waiting", { task: { id: "x-3", title: "t", priority: 3 }, status_since: 1 }),
      row("p0", "waiting", { task: { id: "x-0", title: "t", priority: 0 }, status_since: 300 }),
      row("p2-early", "waiting", { task: { id: "x-2", title: "t", priority: 2 }, status_since: 50 }),
      row("p2-same", "waiting", { status_since: 100 }),
    ];
    expect(sortByAttention(rows).map((r) => r.id)).toEqual(["p0", "p2-early", "nobd", "p2-same", "p2-late", "p3"]);
  });

  it("partition keeps NEEDS ATTENTION first without reordering inside a group", () => {
    const rows = [row("a", "running"), row("b", "waiting"), row("c", "running"), row("d", "failed")];
    expect(partitionByAttention(rows, undefined, NOW).map((r) => r.id)).toEqual(["b", "d", "a", "c"]);
  });

  it("labels a row by task: issue id + title > prompt summary > display name", () => {
    expect(taskLabel(row("a", "running", { task: { id: "agora-1", title: "写 MISSION", priority: 1 }, task_ref: "agora-1", name: "s" }))).toBe(
      "agora-1 写 MISSION",
    );
    expect(taskLabel(row("a", "running", { task_ref: "修 migration 回滚", name: "s" }))).toBe("修 migration 回滚");
    expect(taskLabel(row("a", "running", { name: "sglog", display_name: "x" }))).toBe("sglog");
    expect(taskLabel(row("n:abc", "running"))).toBe("n:abc");
  });

  it("status line carries the wait duration from status_since", () => {
    expect(statusLine(row("a", "waiting", { status_since: 1000 }), 1000 + 3 * 60 + 5)).toBe("waiting 3m");
    expect(statusLine(row("a", "turn_done", { status_since: 1000 }), 1000 + 30)).toBe("turn done");
    expect(statusLine(row("a", "running"), 5)).toBe("working");
    expect(formatAgo(59)).toBe("");
    expect(formatAgo(3600 * 5)).toBe("5h");
    expect(formatAgo(3600 * 72)).toBe("3d");
  });

  it("marks a peer row's wait as the lower bound it is, and leaves local rows exact (agora-5gg.12)", () => {
    // peer 行的起点是本机打的（第一次看见这个状态的那一刻），真实起点可能更早 → 只能写 ≥N。
    expect(
      statusLine(row("zuan:1", "starting", { status_since: 1000, stale: false }), 1000 + 8 * 3600),
    ).toBe("starting ≥8h");
    // stale 的 peer 行同样：只多一个离线标记，时长仍是下界。
    expect(
      statusLine(row("zuan:2", "waiting", { status_since: 1000, stale: true, last_seen: "x" }), 1000 + 40 * 60),
    ).toBe("waiting ≥40m");
    // 本机行：起点是自己打的、精确值，不许带 ≥（否则用户以为自己的机器也说不准话）。
    expect(statusLine(row("mac:1", "waiting", { status_since: 1000 }), 1000 + 3 * 60)).toBe("waiting 3m");
    // 不到一分钟本来就不画时长，也就没有 ≥。
    expect(statusLine(row("zuan:3", "running", { status_since: 1000, stale: false }), 1030)).toBe("working");
    // 判据是 stale 键在不在，不是 node 等于谁（本机行也带 node）。
    expect(isPeerRow(row("mac:1", "waiting", { node: "mac" }))).toBe(false);
    expect(isPeerRow(row("zuan:1", "waiting", { node: "zuan", stale: false }))).toBe(true);
  });

  it("counts by status for the header (headless rows are the one override, agora-2x3z)", () => {
    const c = countByStatus([row("a", "running"), row("b", "starting"), row("c", "waiting"), row("d", "turn_done"), row("e", "weird")]);
    expect(c).toEqual({ running: 2, needsInput: 1, turnDone: 1, finished: 0, failed: 0, idle: 0, unknown: 1 });
  });
});

// agora-no5：没有 status_since 的行（旧节点 / 测试桩）退化为 `<id>@` 时，同一行的 FINISHED 记号与
// TURN_DONE 记号会撞同一个键、互相顶用。退化键必须带上状态，两种记号各认各的（真实 daemon 的行总带
// status_since，本条只服务于退化行）。
describe("需要我的新鲜度窗口（agora-82x7 的另一半，2026-10-08 用户拍板）", () => {
  // 「需要我」回答的是**现在**需要你：现场 8 行里有 6 行是 9/20 与 10/6 的旧账，最老 15 天，把今天
  // 真正的事压下去了。窗口只淘汰"等你回看"的两种状态（turn_done / finished），waiting / failed 不设
  // 上限——那是真的卡着人。降段不等于消失：turn_done 落到在跑段，finished 落到折叠的已完成区。
  const at = (secsAgo: number) => NOW - secsAgo;

  it("turn_done 超过窗口降到 WORKING，窗口内仍在需要我，边界取 <=", () => {
    const none = new Set<string>();
    expect(needsAttention(row("d", "turn_done", { origin: "agora", status_since: at(3600) }), none, NOW)).toBe(true);
    expect(sectionOf(row("d", "turn_done", { origin: "agora", status_since: at(3600) }), none, NOW)).toBe("attention");
    expect(
      needsAttention(row("d", "turn_done", { origin: "agora", status_since: at(ATTENTION_WINDOW_SECS) }), none, NOW),
      "正好等于窗口：还算新鲜",
    ).toBe(true);
    const stale = row("d", "turn_done", { origin: "agora", status_since: at(ATTENTION_WINDOW_SECS + 1) });
    expect(needsAttention(stale, none, NOW)).toBe(false);
    expect(sectionOf(stale, none, NOW), "降段不消失：落到在跑段").toBe("working");
    expect(partitionByAttention([stale], none, NOW).map((r) => r.id), "行不丢").toEqual(["d"]);
  });

  it("finished 超过窗口进折叠区（同时也是 Finished N 清理名单的口径）", () => {
    const none = new Set<string>();
    const stale = row("f", "finished", { origin: "agora", status_since: at(ATTENTION_WINDOW_SECS + 1) });
    expect(finishedCollapsed(stale, none, NOW)).toBe(true);
    expect(sectionOf(stale, none, NOW)).toBe("finished");
    const freshFin = row("f", "finished", { origin: "agora", status_since: at(60) });
    expect(finishedCollapsed(freshFin, none, NOW), "窗口内的还是要人回看（A46）").toBe(false);
  });

  it("waiting / failed 不设上限：卡着人的事，七天前也还是现在的事", () => {
    const none = new Set<string>();
    const long = at(7 * 24 * 3600);
    expect(needsAttention(row("w", "waiting", { status_since: long }), none, NOW)).toBe(true);
    expect(needsAttention(row("f", "failed", { status_since: long }), none, NOW)).toBe(true);
  });

  it("peer 行按 peer 自己那只钟算年龄（与 seenKey 同一口径）", () => {
    // 本机这只钟是重启后重写的（可能差得很远），拿它当年龄会误判；peer_status_since 才是"这一次完成"。
    const peer = row("b:1", "turn_done", {
      node: "b",
      peer_status_since: at(60),
      status_since: at(30 * 24 * 3600),
    });
    expect(needsAttention(peer, new Set(), NOW)).toBe(true);
  });

  it("不知道何时完成的行不判：旧节点 / 测试桩不该被当成'很久以前完成的'", () => {
    expect(needsAttention(row("d", "turn_done", { origin: "agora" }), new Set(), NOW)).toBe(true);
    expect(sectionOf(row("f", "finished", { origin: "agora" }), new Set(), NOW)).toBe("attention");
  });
});

describe("seenKey without status_since (agora-no5)", () => {
  const fin = row("same", "finished", { origin: "agora" });
  const done = row("same", "turn_done", { origin: "agora" });

  it("keeps FINISHED and TURN_DONE marks apart", () => {
    expect(seenKey(fin)).toBe("same@finished");
    expect(seenKey(done)).toBe("same@turn_done");
    const seenFin = new Set([seenKey(fin)]);
    expect(needsAttention(done, seenFin, NOW)).toBe(true);
    expect(finishedCollapsed(fin, seenFin, NOW)).toBe(true);
    const seenDone = new Set([seenKey(done)]);
    expect(finishedCollapsed(fin, seenDone, NOW)).toBe(false);
    expect(needsAttention(done, seenDone, NOW)).toBe(false);
  });

  it("still expires the mark when the row carries status_since", () => {
    // 反向不变量：带锚的行换了 status_since 就是新一次完成，退化路径不得把它一起钉死。
    const first = row("same", "turn_done", { origin: "agora", status_since: 10 });
    const second = row("same", "turn_done", { origin: "agora", status_since: 40 });
    const seen = new Set([seenKey(first)]);
    expect(seen.has(seenKey(second))).toBe(false);
    expect(needsAttention(second, seen, NOW)).toBe(true);
  });
});

// agora-cjv（E）：peer 行的记号锚是 peer 自己那只表的 status_since——本机重启后行上的本地起点会被
// 重算（钟差读数是整数秒，两次测量可能差 1），锚不动记号就不作废；peer 真的又完成一次（锚变了）
// 记号才作废。老节点的行不发这个字段，退回本地起点。
describe("seenKey on peer rows (agora-cjv)", () => {
  const peerRow = (extra: Record<string, unknown> = {}) =>
    row("b:1", "finished", { node: "b", origin: "agora", status_since: 1000, peer_status_since: 500, ...extra });

  it("anchors on the peer's own status_since, not the locally rewritten one", () => {
    expect(seenKey(peerRow())).toBe("b:1@500");
    const restamped = peerRow({ status_since: 1001 });
    expect(seenKey(restamped)).toBe(seenKey(peerRow()));
    const seen = new Set([seenKey(peerRow())]);
    expect(sectionOf(restamped, seen, NOW)).toBe("finished");
    const again = peerRow({ status_since: 2000, peer_status_since: 1500 });
    expect(seen.has(seenKey(again))).toBe(false);
    expect(needsAttention(again, seen, NOW)).toBe(true);
  });

  it("falls back to the local anchor when an old peer does not send the field", () => {
    expect(seenKey(row("b:2", "finished", { node: "b", status_since: 1000 }))).toBe("b:2@1000");
  });
});
