// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { SessionRow } from "./events";
import { SidebarRow } from "./SessionRow";

afterEach(cleanup);

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

it("labels the node only on rows from another node, and only once the local node is known (agora-7ku.5)", () => {
  // MISSION §3.5 每行标明节点：本机的行不标（满屏 @ mac 是噪音），peer 的行标 @ zuan。
  mount(row(), true, "n");
  expect(screen.queryByTestId("row-node-n:a")).toBeNull();
  cleanup();
  mount(row({ id: "zuan:7", node: "zuan" }), true, "n");
  expect(screen.getByTestId("row-node-zuan:7").textContent).toBe("@ zuan");
  cleanup();
  // 还不知道本机是谁（/api/system 没回）：谁都不标，免得先满屏 @ 再消失。
  mount(row({ id: "zuan:7", node: "zuan" }), true);
  expect(screen.queryByTestId("row-node-zuan:7")).toBeNull();
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
