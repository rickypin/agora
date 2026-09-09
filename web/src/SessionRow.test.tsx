// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { SessionRow } from "./events";
import { clockText } from "./Header";
import { RowIdentity, projectLine } from "./RowIdentity";
import { SidebarRow, staleSeen } from "./SessionRow";

afterEach(cleanup);

/** 节点按本机时钟打的 UTC 文本（docs/spec/api.md「peer 视图」的 last_seen 形态）。 */
const SEEN = "2026-09-02T23:10:00Z";

function row(extra: Partial<SessionRow> = {}): SessionRow {
  return { id: "n:a", node: "n", status: "turn_done", alive: true, agent_type: "claude", ...extra };
}

const ACCEPTANCE = "tests/task_info.rs::acceptance_is_read_not_stored（库里无该字段、API 有）；\n前端 vitest：展开显示与折叠。";

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
        expanded={<div data-testid="respond-n:a">respond</div>}
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

it("an active row shows the task's acceptance criteria in full, below the respond area (A40)", () => {
  // MISSION §6.3 看结果：展开一行就该看到"做完算什么"，全文、多行原样，读自 beads。
  mount(row({ task: { id: "agora-h1k.3", title: "验收标准", priority: 2, acceptance: ACCEPTANCE } }));
  const block = screen.getByTestId("acceptance-n:a");
  expect(screen.getByTestId("acceptance-body-n:a").textContent).toBe(ACCEPTANCE);
  expect(screen.getByTestId("acceptance-toggle-n:a").textContent).toContain("agora-h1k.3");
  // 位置：就地 respond 之下（docs/spec/ux.md）。
  const respond = screen.getByTestId("respond-n:a");
  expect(respond.compareDocumentPosition(block) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

it("the acceptance block folds and unfolds without touching the row", () => {
  const { onOpen } = mount(row({ task: { id: "agora-h1k.3", title: "验收标准", priority: 2, acceptance: ACCEPTANCE } }));
  const toggle = screen.getByTestId("acceptance-toggle-n:a");
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  fireEvent.click(toggle);
  expect(screen.queryByTestId("acceptance-body-n:a")).toBeNull();
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(toggle);
  expect(screen.getByTestId("acceptance-body-n:a").textContent).toBe(ACCEPTANCE);
  // 折叠 / 展开不是"打开这一行"。
  expect(onOpen).not.toHaveBeenCalled();
});

it("no task, no acceptance text, or an inactive row: the block takes no space", () => {
  mount(row());
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ task: { id: "agora-x", title: "没写验收", priority: 2, acceptance: "  " } }));
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ task: { id: "agora-x", title: "没写验收", priority: 2 } }));
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ task: { id: "agora-h1k.3", title: "验收标准", priority: 2, acceptance: ACCEPTANCE } }), false);
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  expect(screen.queryByTestId("respond-n:a")).toBeNull();
});
