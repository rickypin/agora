// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import { ChangesApiContext } from "./Changes";
import type { SessionRow } from "./events";
import { RowResult } from "./RowResult";

afterEach(cleanup);

function row(extra: Partial<SessionRow> = {}): SessionRow {
  return { id: "n:a", node: "n", status: "turn_done", alive: true, agent_type: "claude", ...extra };
}

const ACCEPTANCE = "tests/task_info.rs::acceptance_is_read_not_stored（库里无该字段、API 有）；\n前端 vitest：展开显示与折叠。";

const TASK = { id: "agora-h1k.3", title: "验收标准", priority: 2, acceptance: ACCEPTANCE };

/** 不给 api：改动列表那半边不显示，只看验收标准（Changes 自己的用例在 Changes.test.tsx）。 */
function mount(r: SessionRow) {
  render(<RowResult row={r} />);
}

/** 给一个假 /changes 的 api：两段都在，用来看面板里的顺序。 */
function mountWithApi(r: SessionRow) {
  const f: FetchLike = async () =>
    new Response(JSON.stringify({ files: [{ path: "web/src/RowResult.tsx", status: "modified" }], branch: "agora-4yr.3", reason: null }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  render(
    <ChangesApiContext.Provider value={sessionApi(f)}>
      <RowResult row={r} />
    </ChangesApiContext.Provider>,
  );
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

it("an active row shows the task's acceptance criteria in full (A40) (moved to main area, A50)", () => {
  // MISSION §6.3 看结果：选中一行就该看到"做完算什么"，全文、多行原样，读自 beads。
  // 2026-09-10 从侧栏行的展开区搬进主区的看结果面板（agora-4yr.3）：文本与 testid 一个字没变，
  // 变的只是它长在哪儿——这条用例连断言都不用改，正好说明搬家没动语义。
  mount(row({ task: TASK }));
  expect(screen.getByTestId("acceptance-body-n:a").textContent).toBe(ACCEPTANCE);
  expect(screen.getByTestId("acceptance-toggle-n:a").textContent).toContain("agora-h1k.3");
});

it("the acceptance block folds and unfolds without touching the row (moved to main area, A50)", () => {
  mount(row({ task: TASK }));
  const toggle = screen.getByTestId("acceptance-toggle-n:a");
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  fireEvent.click(toggle);
  expect(screen.queryByTestId("acceptance-body-n:a")).toBeNull();
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(toggle);
  expect(screen.getByTestId("acceptance-body-n:a").textContent).toBe(ACCEPTANCE);
  // 「不碰行」在主区的意思变了：面板已经不在 <li> 里，折叠不可能再点开某一行；
  // 剩下的事实是折叠只吞 body，summary 与它所属的面板都还在。
  expect(screen.getByTestId("result-panel-n:a")).toBeTruthy();
});

it("no task, no acceptance text, or an inactive row: the block takes no space (moved to main area, A50)", () => {
  // 前三档原样：没有任务 / 验收标准是空白 / 任务里根本没写这个字段。第四档「未选中的行」在主区
  // 没有对应形态——面板只为选中行挂一次（Workspace），所以换成等价的那件事：这一行既没有验收
  // 标准、状态也不在 Changes 的集合里时，整个 result-panel 一格都不占（空面板会白留一条边线）。
  mount(row());
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ task: { id: "agora-x", title: "没写验收", priority: 2, acceptance: "  " } }));
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ task: { id: "agora-x", title: "没写验收", priority: 2 } }));
  expect(screen.queryByTestId("acceptance-n:a")).toBeNull();
  cleanup();
  mount(row({ status: "waiting" }));
  expect(screen.queryByTestId("result-panel-n:a")).toBeNull();
});

it("the result panel is 验收标准 then 改动列表, in that order (A50)", async () => {
  // agora-h1k.3 定的顺序：对照"做完算什么"看"改了什么"。
  mountWithApi(row({ task: TASK }));
  await flush();
  const panel = screen.getByTestId("result-panel-n:a");
  expect(panel.tagName).toBe("SECTION");
  expect(Array.from(panel.children).map((el) => el.className)).toEqual(["acceptance", "changes"]);
  expect(screen.getByTestId("changes-list-n:a").textContent).toContain("M web/src/RowResult.tsx");
});
