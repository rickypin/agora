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

function mount(r: SessionRow, active = true) {
  const onOpen = vi.fn();
  render(
    <ul>
      <SidebarRow row={r} active={active} ordinal={1} onOpen={onOpen} now={0} expanded={<div data-testid="respond-n:a">respond</div>} />
    </ul>,
  );
  return { onOpen };
}

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
