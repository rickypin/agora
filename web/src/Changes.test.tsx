// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import { Changes } from "./Changes";
import type { SessionRow } from "./events";

afterEach(cleanup);

function row(extra: Partial<SessionRow> = {}): SessionRow {
  return { id: "n:a", node: "n", status: "turn_done", alive: true, agent_type: "claude", ...extra };
}

/** 假的 /changes：记下请求的 URL，按 `answer` 应答。 */
function fakeApi(answer: () => { status?: number; body: unknown }) {
  const urls: string[] = [];
  const f: FetchLike = async (url) => {
    urls.push(url);
    const a = answer();
    return new Response(JSON.stringify(a.body), { status: a.status ?? 200, headers: { "content-type": "application/json" } });
  };
  return { api: sessionApi(f), urls };
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

it("lists changed files as <letter> <path> and the diff button opens the row's diff (A41)", async () => {
  // MISSION §6.3 看结果：TURN_DONE 的行展开列出该 worktree 的改动文件，旁边一键看 diff。
  const { api, urls } = fakeApi(() => ({
    body: {
      files: [
        { path: "a.txt", status: "modified" },
        { path: "new.txt", status: "untracked" },
        { path: "staged.txt", status: "added" },
        { path: "gone.txt", status: "deleted" },
      ],
      branch: "agora-h1k.5",
      reason: null,
    },
  }));
  const onOpenDiff = vi.fn();
  render(<Changes row={row()} api={api} onOpenDiff={onOpenDiff} />);
  await flush();
  expect(urls).toEqual(["/api/sessions/n%3Aa/changes"]);
  const items = Array.from(screen.getByTestId("changes-list-n:a").querySelectorAll("li")).map((li) => li.textContent);
  expect(items).toEqual(["M a.txt", "? new.txt", "A staged.txt", "D gone.txt"]);
  expect(screen.getByTestId("changes-n:a").textContent).toContain("agora-h1k.5");
  const btn = screen.getByTestId("diff-n:a") as HTMLButtonElement;
  expect(btn.disabled).toBe(false);
  fireEvent.click(btn);
  expect(onOpenDiff).toHaveBeenCalledWith("n:a");
});

it("an empty list says 无改动 and a typed reason gets its own line, by type not by text", async () => {
  let body: unknown = { files: [], branch: "main", reason: null };
  const { api } = fakeApi(() => ({ body }));
  const { unmount } = render(<Changes row={row({ status: "finished" })} api={api} />);
  await flush();
  expect(screen.getByTestId("changes-empty-n:a").textContent).toBe("无改动");
  expect(screen.queryByTestId("changes-reason-n:a")).toBeNull();
  unmount();

  // reason 按类型（docs/spec/api.md）：不是仓库 → 文案；按钮禁用（没有可 diff 的仓库）。
  body = { files: [], branch: null, reason: "not_a_repo" };
  render(<Changes row={row({ status: "failed" })} api={api} />);
  await flush();
  const reason = screen.getByTestId("changes-reason-n:a");
  expect(reason.getAttribute("data-reason")).toBe("not_a_repo");
  expect(reason.textContent).toBe("不是 git 仓库");
  expect(screen.queryByTestId("changes-empty-n:a")).toBeNull();
  expect((screen.getByTestId("diff-n:a") as HTMLButtonElement).disabled).toBe(true);
  cleanup();

  body = { files: [], branch: null, reason: "no_directory" };
  render(<Changes row={row()} api={api} />);
  await flush();
  expect(screen.getByTestId("changes-reason-n:a").textContent).toBe("工作目录不存在");
});

it("fetches once per status change and not at all for states where the result is not the point", async () => {
  const { api, urls } = fakeApi(() => ({ body: { files: [], branch: "main", reason: null } }));
  const { rerender } = render(<Changes row={row({ status: "running" })} api={api} />);
  await flush();
  expect(urls.length).toBe(1); // RUNNING 也拉一次
  rerender(<Changes row={row({ status: "running", progress: "Edit x" })} api={api} />);
  await flush();
  expect(urls.length).toBe(1); // 同一状态下别的字段变了：不重拉，不轮询
  rerender(<Changes row={row({ status: "turn_done" })} api={api} />);
  await flush();
  expect(urls.length).toBe(2); // 状态变了：再拉一次
  rerender(<Changes row={row({ status: "waiting" })} api={api} />);
  await flush();
  expect(urls.length).toBe(2);
  expect(screen.queryByTestId("changes-n:a")).toBeNull(); // WAITING：不占位，此刻该做的是回答问题
});

it("folding the list keeps the fetched state: unfolding does not re-pull /changes (agora-4yr.3)", async () => {
  // 折叠（agora-4yr.3，与验收标准同一副按钮）只藏 body，不把整个 Changes 从 DOM 里摘掉：
  // 拉取挂在 mount 的 useEffect，组件一卸载 state 就没了，"收起来再打开"会白白重发一次请求。
  const { api, urls } = fakeApi(() => ({ body: { files: [{ path: "a.txt", status: "modified" }], branch: "main", reason: null } }));
  render(<Changes row={row()} api={api} />);
  await flush();
  expect(urls.length).toBe(1);
  const toggle = screen.getByTestId("changes-toggle-n:a");
  expect(toggle.getAttribute("aria-expanded")).toBe("true"); // 默认展开：看结果就是为了看改了什么
  expect(toggle.textContent).toBe("▾ 改动 · main");
  fireEvent.click(toggle);
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByTestId("changes-list-n:a")).toBeNull();
  // 「看 diff」留在头上、不随折叠消失：收起列表是嫌它长，不是不想看 diff。
  expect(screen.getByTestId("diff-n:a")).toBeTruthy();
  fireEvent.click(toggle);
  await flush();
  expect(screen.getByTestId("changes-list-n:a").textContent).toContain("M a.txt");
  expect(urls.length).toBe(1);
});
