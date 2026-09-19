// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { SessionRow } from "./events";
import { clockText } from "./Header";
import { RowIdentity, projectLine } from "./RowIdentity";
import rawCss from "./index.css?raw";
import { SidebarRow, staleSeen } from "./SessionRow";

afterEach(cleanup);

/** 节点按本机时钟打的 UTC 文本（docs/spec/api.md「peer 视图」的 last_seen 形态）。 */
const SEEN = "2026-09-02T23:10:00Z";

function row(extra: Partial<SessionRow> = {}): SessionRow {
  return { id: "n:a", node: "n", status: "turn_done", alive: true, agent_type: "claude", ...extra };
}

function mount(r: SessionRow, active = true, localNode?: string) {
  const onOpen = vi.fn();
  render(
    <ul>
      <SidebarRow
        row={r}
        active={active}
        ordinal={1}
        onOpen={onOpen}
        now={0}
        localNode={localNode}
      />
    </ul>,
  );
  return { onOpen };
}

it("labels the node on every row, local ones uncolored (A49; reverses agora-7ku.5)", () => {
  // 两台机常态并行：本机也标，muted 边框不着色；peer 按 nodeHue 着色。还不知道本机是谁时
  // 谁都不标（分不出 local / peer）。
  mount(row(), true, "n");
  const local = screen.getByTestId("row-node-n:a");
  expect(local.textContent).toBe("@ n");
  expect(local.getAttribute("data-node")).toBe("n");
  expect(local.className).toBe("node local");
  expect(local.style.getPropertyValue("--hue")).toBe("");
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan" }), true, "n");
  const peer = screen.getByTestId("row-node-zuan:7");
  expect(peer.textContent).toBe("@ zuan");
  expect(peer.className).toBe("node peer");
  expect(peer.style.getPropertyValue("--hue")).not.toBe("");
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan" }), true);
  expect(screen.queryByTestId("row-node-zuan:7")).toBeNull();
});

it("shows an agent badge with data-agent and a colored node chip with data-node (A49)", () => {
  mount(row({ agent_type: "claude" }), true, "n");
  const badge = document.querySelector("[data-agent]") as HTMLElement | null;
  expect(badge).toBeTruthy();
  expect(badge?.getAttribute("data-agent")).toBe("claude");
  expect(badge?.textContent).toMatch(/Claude/);
  expect(badge?.style.getPropertyValue("--hue")).toBe("30");
  expect(screen.getByTestId("row-node-n:a").getAttribute("data-node")).toBe("n");
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan", agent_type: "codex" }), true, "n");
  const chip = screen.getByTestId("row-node-zuan:7");
  expect(chip.getAttribute("data-node")).toBe("zuan");
  expect(chip.className).toBe("node peer");
  expect(chip.style.getPropertyValue("--hue")).not.toBe("");
  const peerBadge = document.querySelector("[data-agent]") as HTMLElement | null;
  expect(peerBadge?.getAttribute("data-agent")).toBe("codex");
  expect(peerBadge?.style.getPropertyValue("--hue")).toBe("200");
});

/** index.css 去掉注释、空白压成单空格，方便按选择器取规则体。 */
const CSS = rawCss.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\s+/g, " ");

/** 取 `selector { … }` 的声明体；找不到就让调用方的断言报出是哪条选择器丢了。 */
function cssRule(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  // 前面钉住 `}`/`;`/开头，否则 `.row .badge` 会命中 `.row .meta > .badge` 的尾巴。
  const m = CSS.match(new RegExp(`(?:^|[;}]) ?${esc} \\{([^}]*)\\}`));
  expect(m, `index.css 里找不到规则 \`${selector}\``).toBeTruthy();
  return m![1];
}

it("splits the badge into a whole glyph and a truncatable label, full name in title (agora-8lb)", () => {
  // 2026-09-09 用户目检：侧栏行徽标成了「✧ Gro」，本机 external 行只剩半个 glyph。根因是 glyph 与
  // label 在同一个 span 里，.meta 的省略号从 label 一路吃到 glyph。jsdom 不做布局，"被裁了几像素"
  // 这里断不出来——本 bug 从 agora-uvd.7 合入到目检之间，上面那条徽标用例一直是绿的——所以这条钉的是
  // 让截断**不可能发生**的结构：两个独立元素，全名另有 title 兜底。像素由代检用 agent-browser 量。
  mount(row({ agent_type: "grok" }), true, "n");
  const badge = document.querySelector(".badge") as HTMLElement;
  const glyph = badge.querySelector(".badge-glyph") as HTMLElement | null;
  const label = badge.querySelector(".badge-label") as HTMLElement | null;
  expect(glyph, "glyph 必须是独立元素，不能和 label 共用一个 span").toBeTruthy();
  expect(label).toBeTruthy();
  expect(glyph).not.toBe(label);
  expect(glyph!.textContent).toBe("✧");
  expect(label!.textContent).toBe("Grok");
  // 窄侧栏下只看得见 glyph，全名得能 hover 出来。
  expect(badge.title).toContain("Grok");
});

it("lets only .badge-label carry the ellipsis; glyph and state never give way (agora-8lb)", () => {
  // CSS 守卫。根因那条规则按**位置**点名（谁排第一谁被截），uvd.7 把徽标放到第一位就中招了；
  // 它一旦回来，上面那条结构守卫照样绿、bug 照样复发，所以单独钉住。
  expect(CSS, "按位置点名的规则不许回来：谁排第一是布局的事，谁可以截断是语义的事").not.toMatch(
    /\.row \.meta > span:first-child/,
  );

  const glyph = cssRule(".row .badge-glyph");
  expect(glyph).toMatch(/flex: none/);
  expect(glyph).toMatch(/width: 1em/); // 与 .badge 的 min-width: 1em 是同一道护栏的两头
  expect(glyph, "glyph 上不许有省略号——宁可丢全名也不能丢这一个字符").not.toMatch(/text-overflow/);

  const label = cssRule(".row .badge-label");
  expect(label).toMatch(/text-overflow: ellipsis/);
  expect(label).toMatch(/min-width: 0/);

  const badge = cssRule(".row .meta > .badge");
  expect(badge, "徽标要能让位，否则行尾 state 被裁").toMatch(/flex: 0 100 auto/);
  expect(badge, "内容框下限护住 glyph").toMatch(/min-width: 1em/);
  // 这两条写上去，flex item 的 min-width: auto 会被解析成 0，护栏失效、glyph 被 badge 自己削掉。
  expect(badge).not.toMatch(/overflow: hidden/);
  expect(badge).not.toMatch(/min-width: 0/);

  expect(cssRule(".row .meta .state"), "state 永不让位").toMatch(/flex: none/);
});

it("splits the node chip into a whole @ and a truncatable name, full name in title (agora-yaf)", () => {
  // 2026-09-10 agora-8lb 代检：220px 侧栏下 @ workstation 被硬裁 51px，屏幕上是「@ works」。
  // 根因是 .node 是 inline-flex，text-overflow 在 flex 容器上不生效。jsdom 不做布局，像素由
  // 代检用 agent-browser 量——这条钉的是让硬裁**不可能发生**的结构：两个独立元素，全名另有 title。
  mount(row({ id: "workstation:1", node: "workstation" }), true, "n");
  const chip = screen.getByTestId("row-node-workstation:1");
  const at = chip.querySelector(".node-at") as HTMLElement | null;
  const name = chip.querySelector(".node-label") as HTMLElement | null;
  expect(at, "@ 必须是独立元素，不能和节点名共用一个 span").toBeTruthy();
  expect(name).toBeTruthy();
  expect(at).not.toBe(name);
  expect(at!.textContent).toBe("@");
  expect(name!.textContent).toBe("workstation");
  expect(chip.title).toContain("workstation");
  // 既有用例钉的是「@ zuan」整段文案，空白文本节点在 flex 里不占位、textContent 仍带空格。
  expect(chip.textContent).toBe("@ workstation");
});

it("lets only .node-label carry the ellipsis; @ never gives way (agora-yaf)", () => {
  // CSS 守卫。chip 自己一旦 overflow: hidden + min-width: 0，「@」会被自己削掉（agora-8lb 徽标
  // 同构的坑）；名字必须 block 化，inline-flex 上 text-overflow 不生效正是本 bug 的根因。
  expect(CSS, "节点 chip 不许再和 origin / stale 共用 overflow: hidden——那会把 @ 一起硬裁").not.toMatch(
    /\.row \.meta > \.node, \.row \.meta > \.origin/,
  );

  const at = cssRule(".row .node-at");
  expect(at).toMatch(/flex: none/);
  expect(at, "@ 上不许有省略号——宁可丢节点名也不能丢这个字符").not.toMatch(/text-overflow/);

  const name = cssRule(".row .node-label");
  expect(name).toMatch(/text-overflow: ellipsis/);
  expect(name).toMatch(/min-width: 0/);
  expect(name, "inline-flex 上 text-overflow 不生效，名字 span 必须 block 化").toMatch(/display: block/);

  const chip = cssRule(".row .meta > .node");
  expect(chip, "节点 chip 要能让位，否则行尾 state 被裁").toMatch(/flex: 0 1 auto/);
  expect(chip, "内容框下限护住 @").toMatch(/min-width: 1em/);
  expect(chip).not.toMatch(/overflow: hidden/);
  expect(chip).not.toMatch(/min-width: 0/);
});

const MAIN = { repo: "/Users/r/code/agora", name: "agora", worktree: "/Users/r/code/agora", branch: "main", main: true };
const LINKED = { ...MAIN, worktree: "/Users/r/code/agora-wt/agora-03k", branch: "agora-03k", main: false };

it("shows a repo ⎇ branch line in attention mode, with worktree name when not main (A49)", () => {
  // 主 worktree：`agora ⎇ main`；linked worktree 在名字后加 worktree 目录最后一段。只显示名字与分支，
  // 完整路径放 title。位置在 meta 之下（预览之上）。
  mount(row({ project: MAIN }), true, "n");
  const line = screen.getByTestId("project-n:a");
  expect(line.textContent).toBe("agora ⎇ main");
  expect(line.title).toBe("/Users/r/code/agora");
  expect(line.className).toBe("line-project");
  const meta = document.querySelector(".meta")!;
  expect(meta.compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  cleanup();
  mount(row({ project: LINKED }), true, "n");
  expect(screen.getByTestId("project-n:a").textContent).toBe("agora / agora-03k ⎇ agora-03k");
  expect(screen.getByTestId("project-n:a").title).toBe("/Users/r/code/agora-wt/agora-03k");
});

it("the project line falls back to the directory name when project is null, and is absent when showProject is false", () => {
  // 不是仓库（服务端给 null）但有工作目录：目录最后一段，title 放完整路径；两者都没有不占位；
  // 树视图（SidebarTree 传 showProject=false）组头已说明仓库与分支，行上不画。
  mount(row({ project: null, working_directory: "/tmp" }), true, "n");
  expect(screen.getByTestId("project-n:a").textContent).toBe("tmp");
  expect(screen.getByTestId("project-n:a").title).toBe("/tmp");
  cleanup();
  mount(row({ project: null }), true, "n");
  expect(screen.queryByTestId("project-n:a")).toBeNull();
  cleanup();
  render(<RowIdentity row={row({ project: MAIN })} localNode="n" now={0} showProject={false} />);
  expect(screen.queryByTestId("project-n:a")).toBeNull();
  expect(screen.getByTestId("row-node-n:a")).toBeTruthy();
  expect(projectLine(row())).toBeNull();
});

it("detached HEAD shows ⎇ detached", () => {
  // 取舍：字面 detached，不显示 commit 短 hash。
  mount(row({ project: { ...MAIN, branch: null } }), true, "n");
  expect(screen.getByTestId("project-n:a").textContent).toBe("agora ⎇ detached");
  expect(projectLine(row({ project: { ...LINKED, branch: null } }))?.text).toBe("agora / agora-03k ⎇ detached");
});

it("a stale peer row says when the node was last seen, in local HH:MM, and dims the whole row (A29; invariant 8)", () => {
  // MISSION §3.5 "peer 断线保留最后视图并标记（上次见到 23:10）"：行没消失，.meta 里多一段，
  // 时间与 Header 那一枚用同一个 clockText（本地时区），完整 UTC 放 title。
  mount(row({ id: "zuan:7", node: "zuan", stale: true, last_seen: SEEN }), false, "n");
  const seen = screen.getByTestId("row-stale-zuan:7");
  expect(seen.textContent).toBe(`○ 上次见到 ${clockText(SEEN)}`);
  expect(seen.title).toContain(SEEN);
  // 行还在、能点、整行淡显（li.stale，样式在 index.css）；@ node 照常。
  expect(screen.getByTestId("row-zuan:7")).toBeTruthy();
  expect(screen.getByTestId("row-zuan:7").closest("li")?.className).toContain("stale");
  expect(screen.getByTestId("row-node-zuan:7").textContent).toBe("@ zuan");
  // 选中与 stale 正交。
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan", stale: true, last_seen: SEEN }), true, "n");
  expect(screen.getByTestId("row-zuan:7").closest("li")?.className).toBe("selected stale");
});

it("a row that is not stale has no '上次见到' and no stale class", () => {
  // 在线的 peer 行（stale: false）与本机行（没有 stale 键）都不渲染这一段——它只属于离线的 peer。
  mount(row({ id: "zuan:7", node: "zuan", stale: false }), false, "n");
  expect(screen.queryByTestId("row-stale-zuan:7")).toBeNull();
  expect(screen.getByTestId("row-zuan:7").closest("li")?.className ?? "").not.toContain("stale");
  cleanup();
  mount(row(), true, "n");
  expect(screen.queryByTestId("row-stale-n:a")).toBeNull();
  expect(screen.getByTestId("row-n:a").closest("li")?.className).toBe("selected");
  // 只有 last_seen 没有 stale（不该发生）：不当 stale 画。
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan", last_seen: SEEN }), false, "n");
  expect(screen.queryByTestId("row-stale-zuan:7")).toBeNull();
});

it("a stale row without last_seen still renders (offline, no time) instead of crashing", () => {
  expect(() => mount(row({ id: "zuan:7", node: "zuan", stale: true }), false, "n")).not.toThrow();
  expect(screen.getByTestId("row-stale-zuan:7").textContent).toBe("○ 离线");
  expect(screen.getByTestId("row-zuan:7").closest("li")?.className).toContain("stale");
  // 解析不了的 last_seen：clockText 原样给回，不抛。
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan", stale: true, last_seen: "garbage" }), false, "n");
  expect(screen.getByTestId("row-stale-zuan:7").textContent).toBe("○ 上次见到 garbage");
  // 纯函数形态。
  expect(staleSeen(row())).toBeNull();
  expect(staleSeen(row({ stale: true, last_seen: SEEN }))?.text).toBe(`○ 上次见到 ${clockText(SEEN)}`);
});

// agora-5gg.11：侧栏四段化之后，UNCLEAR 段的名字只说"这一行的状态说不清"，看得懂得靠行上这一句——
// 服务端那句 reason 原样（不翻译成人的话：那已经是人写的句子）+ 按 origin 分的一句出口。
it("an UNKNOWN row carries the server reason plus an exit on the row itself (agora-5gg.11)", () => {
  mount(row({ id: "n:a", status: "unknown", origin: "agora", reason: "hooks silent; no process handle" }));
  const hint = screen.getByTestId("unclear-n:a");
  expect(hint.textContent).toBe("hooks silent; no process handle · 选中它，打开它的终端看一眼");
  // 长 reason 在 260 px 的窄列里会被省略号裁掉（.row .preview 是 nowrap），全文放 title 兜底。
  expect(hint.getAttribute("title")).toContain("hooks silent; no process handle");
  expect(hint.getAttribute("title")).toContain("选中它，打开它的终端看一眼");
  cleanup();
  // external 行 agora 手里没有运行时句柄、没有终端可开（`Workspace.tsx` 的 no-terminal 那段话）：
  // 出口不能写成"打开终端"，那是个点了什么都不会发生的承诺。
  mount(row({ id: "n:e", status: "unknown", origin: "external", reason: "hooks silent; no process handle" }));
  expect(screen.getByTestId("unclear-n:e").textContent).toBe("hooks silent; no process handle · 等下一条 hook，或去它自己的窗口看");
  cleanup();
  // 没有 reason 也照样给出口：比"什么都不知道"多一句能做的事。
  mount(row({ id: "n:u", status: "unknown" }));
  expect(screen.getByTestId("unclear-n:u").textContent).toBe("说不清它在做什么 · 选中它，打开它的终端看一眼");
  // 同一句原因只画一次：UNKNOWN 行不再走 lines 那条"两者都没有时退回状态理由"的退路。
  cleanup();
  mount(row({ id: "n:d", status: "unknown", origin: "agora", reason: "hooks silent; no process handle" }));
  expect((screen.getByTestId("row-n:d").textContent ?? "").match(/hooks silent; no process handle/g)?.length).toBe(1);
  cleanup();
  // 有 pane preview 的 UNKNOWN 行：预览照旧，原因 + 出口另起一行，两者不互相顶掉。
  mount(row({ id: "n:p", status: "unknown", preview: "git push origin main", reason: "runtime unavailable" }));
  expect(screen.getByTestId("preview-n:p").textContent).toBe("git push origin main");
  expect(screen.getByTestId("unclear-n:p").textContent).toContain("runtime unavailable");
  cleanup();
  // 我们不认识的状态名同一段（`unclearStatus` 是段与行共用的唯一判据）。
  mount(row({ id: "n:x", status: "detached" }));
  expect(screen.getByTestId("unclear-n:x")).toBeTruthy();
  cleanup();
  // 别的状态没有这一行：running / waiting 仍走原来的 pane preview 退路，不多占一行。
  mount(row({ id: "n:r", status: "running", reason: "activity" }));
  expect(screen.queryByTestId("unclear-n:r")).toBeNull();
  expect(screen.getByTestId("preview-n:r").textContent).toBe("activity");
});

// 验收标准三条（A40）2026-09-10 随 Acceptance 搬去 web/src/RowResult.test.tsx（agora-4yr.3）：
// 那两段进了主区的看结果面板，行里再没有它们。行的守卫改由 Sidebar.test.tsx 的
// 「the sidebar DOM never contains respond-, acceptance- or changes- testids」负责。

it("marks the rows whose process agora cannot speak of, and only those (Q4, agora-5gg.18)", () => {
  // 三值里只有 `unknown` 需要在行上补一句：`alive` 不用标（行本身就在跑），`gone` 由状态符号
  // （✓ / ✗）说。旧布尔把 unknown 压成 alive:false，于是 Codex Desktop 那 7 行 turn_done 看上去
  // 像"agent 说做完了、进程却没了"（盘点 B2）——这一条钉的就是那个区分还留在行上。
  mount(row({ status: "turn_done", alive: false, process: "unknown" }));
  const flag = screen.getByTestId("proc-n:a");
  expect(flag.textContent).toContain("进程未知");
  expect(flag.getAttribute("title")).toContain("进程号");
  expect(cssRule(".row .proc-flag")).toContain("var(--muted)");
  cleanup();

  mount(row({ status: "running", alive: true, process: "alive" }));
  expect(screen.queryByTestId("proc-n:a")).toBeNull();
  cleanup();

  mount(row({ status: "finished", alive: false, process: "gone" }));
  expect(screen.queryByTestId("proc-n:a")).toBeNull();
  cleanup();

  // 没升级的 peer 行不带 process：按 alive 投影读，不乱标。
  mount(row({ status: "finished", alive: false }) as SessionRow);
  expect(screen.queryByTestId("proc-n:a")).toBeNull();
});
