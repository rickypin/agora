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

// 验收标准三条（A40）2026-09-10 随 Acceptance 搬去 web/src/RowResult.test.tsx（agora-4yr.3）：
// 那两段进了主区的看结果面板，行里再没有它们。行的守卫改由 Sidebar.test.tsx 的
// 「the sidebar DOM never contains respond-, acceptance- or changes- testids」负责。
