// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { partitionByAttention, sortByAttention, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { Sidebar } from "./Sidebar";

afterEach(cleanup);

function row(id: string, status: string, extra: Partial<SessionRow> = {}): SessionRow {
  return { id, node: "n", status, alive: true, agent_type: "claude", display_name: id.slice(2), ...extra };
}

/** 与 Workspace 同一条拼法：sortByAttention → partitionByAttention(seen)，Sidebar 只在交界处插标题。 */
function mount(rows: SessionRow[], seen: SeenSet = new Set(), active: string | null = null) {
  const onOpen = vi.fn();
  const visible = partitionByAttention(sortByAttention(rows), seen);
  render(<Sidebar rows={visible} seen={seen} all={rows} total={rows.length} active={active} onOpen={onOpen} filter="" onFilter={() => {}} />);
  return { onOpen, visible };
}

/** 列表里标题与行的先后：只看 section-* 与 row-* 两类 testid。 */
function listOrder(): string[] {
  return Array.from(document.querySelectorAll("[data-testid]"))
    .map((el) => el.getAttribute("data-testid")!)
    .filter((id) => id.startsWith("section-") || id.startsWith("row-"));
}

const ROWS = [
  row("n:run", "running"),
  row("n:ext1", "finished", { origin: "external", status_since: 10 }),
  row("n:wait", "waiting"),
  row("n:own", "finished", { origin: "agora", status_since: 20 }),
  row("n:ext2", "finished", { origin: "external", status_since: 30 }),
];

it("collapses the Finished section by default with a count, and NEEDS ATTENTION holds no external FINISHED row (A46)", () => {
  mount(ROWS);
  // 两条 external FINISHED 收起来了：NEEDS ATTENTION 里只有 waiting 与 agora 来源的 FINISHED；折叠区标题带计数。
  expect(listOrder()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished"]);
  const head = screen.getByTestId("section-finished");
  expect(head.textContent).toBe("▸ FINISHED 2");
  expect(head.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByTestId("row-n:ext1")).toBeNull();
  expect(screen.queryByTestId("row-n:ext2")).toBeNull();
  // Header 的计数行不变：Finished 仍按状态数（变按钮是 agora-j4w.2）。
  expect(screen.getByTestId("counts").textContent).toBe("Running 1 · Needs Input 1 · Finished 3");
});

it("expands the Finished section on click; rows keep attention order, stay selectable and keep their ordinals", () => {
  const { onOpen, visible } = mount(ROWS);
  fireEvent.click(screen.getByTestId("section-finished"));
  const head = screen.getByTestId("section-finished");
  expect(head.textContent).toBe("▾ FINISHED 2");
  expect(head.getAttribute("aria-expanded")).toBe("true");
  // 展开后的顺序 = sortByAttention 的顺序（status_since 早的在前），排在 RUNNING 之后。
  expect(listOrder()).toEqual(["section-attention", "row-n:wait", "row-n:own", "section-running", "row-n:run", "section-finished", "row-n:ext1", "row-n:ext2"]);
  // 序号是整条显示顺序里的位置（折叠与否都一样）：ext1 是第 4 条、ext2 第 5 条，与 Alt/Option+N 用的 visible 一致。
  expect(visible.map((r) => r.id)).toEqual(["n:wait", "n:own", "n:run", "n:ext1", "n:ext2"]);
  expect(screen.getByTestId("row-n:ext1").querySelector(".ord")?.textContent).toBe("4");
  expect(screen.getByTestId("row-n:ext2").querySelector(".ord")?.textContent).toBe("5");
  // 折叠区里的行能选中。
  fireEvent.click(screen.getByTestId("row-n:ext1"));
  expect(onOpen).toHaveBeenCalledWith("n:ext1");
  // 再点标题收回去，行不在 DOM 里，标题还在。
  fireEvent.click(screen.getByTestId("section-finished"));
  expect(screen.queryByTestId("row-n:ext1")).toBeNull();
  expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 2");
});

it("moves an agora FINISHED row into the Finished section once it is in the seen set; running-only lists get no Finished head", () => {
  mount(ROWS, new Set(["n:own"]));
  expect(listOrder()).toEqual(["section-attention", "row-n:wait", "section-running", "row-n:run", "section-finished"]);
  expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 3");
  cleanup();
  // 没有 FINISHED 行就没有折叠区标题；没有 NEEDS ATTENTION 行也没有 RUNNING 标题（原行为不变）。
  mount([row("n:a", "running"), row("n:b", "idle")]);
  expect(listOrder()).toEqual(["row-n:b", "row-n:a"]);
  cleanup();
  // 只有 external FINISHED：没有 NEEDS ATTENTION 标题、没有 RUNNING 标题、只有折叠区。
  mount([row("n:e", "finished", { origin: "external" })]);
  expect(listOrder()).toEqual(["section-finished"]);
  expect(screen.getByTestId("section-finished").textContent).toBe("▸ FINISHED 1");
});

it("auto-expands the collapsed Finished section when the active row lives in it (agora-4nk)", () => {
  // Alt/Option+N、finished 通知点击、命令面板都是从外面改 active：收起时选中折叠区里的行，该行必须画出来。
  const seen: SeenSet = new Set();
  const visible = partitionByAttention(sortByAttention(ROWS), seen);
  const props = { rows: visible, seen, all: ROWS, total: ROWS.length, onOpen: vi.fn(), filter: "", onFilter: () => {} };
  const expanded = (r: SessionRow) => <div data-testid={`expanded-${r.id}`} />;
  const { rerender } = render(<Sidebar {...props} active="n:wait" renderExpanded={expanded} />);
  expect(screen.queryByTestId("row-n:ext2")).toBeNull();
  rerender(<Sidebar {...props} active="n:ext2" renderExpanded={expanded} />);
  const li = screen.getByTestId("row-n:ext2").closest("li")!;
  expect(li.classList.contains("selected")).toBe(true);
  expect(screen.getByTestId("expanded-n:ext2")).toBeTruthy();
  expect(screen.getByTestId("section-finished").getAttribute("aria-expanded")).toBe("true");
  // 序号不随展开变：ext2 仍是第 5 条。
  expect(screen.getByTestId("row-n:ext2").querySelector(".ord")?.textContent).toBe("5");
  // 人可以再手动收起（active 还在里面也收得起来）；换到折叠区里另一行又展开。
  fireEvent.click(screen.getByTestId("section-finished"));
  expect(screen.queryByTestId("row-n:ext2")).toBeNull();
  rerender(<Sidebar {...props} active="n:ext1" renderExpanded={expanded} />);
  expect(screen.getByTestId("row-n:ext1").closest("li")!.classList.contains("selected")).toBe(true);
});
