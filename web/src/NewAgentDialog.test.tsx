// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import { NewAgentDialog } from "./NewAgentDialog";

afterEach(cleanup);

const PROJECTS = {
  projects: [
    { path: "/Users/r/code/agora", name: "agora", last_used_at: "2026-09-03T10:00:00Z" },
    { path: "/Users/r/code/other", name: "other", last_used_at: null },
  ],
};
const AGENTS = { agents: [{ name: "a1", command: "a1" }, { name: "a2", command: "a2-bin" }] };
const WORKTREES = {
  worktrees: [
    { path: "/Users/r/code/agora", branch: "main", head: "abc", main: true, locked: false },
    { path: "/Users/r/code/agora-wt/x", branch: "feat/x", head: "def", main: false, locked: false },
  ],
};

function setup() {
  const requests: { url: string; method: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    requests.push({ url, method: init.method ?? "GET", body: init.body as string | undefined });
    const body = url.startsWith("/api/projects/worktrees")
      ? WORKTREES
      : url.startsWith("/api/projects")
        ? PROJECTS
        : url.startsWith("/api/agents")
          ? AGENTS
          : url.startsWith("/api/system")
            ? { node: "mac" }
            : { id: "mac:new1" };
    return new Response(JSON.stringify(body), {
      status: init.method === "POST" ? 201 : 200,
      headers: { "content-type": "application/json" },
    });
  };
  const onCreated = vi.fn();
  const onClose = vi.fn();
  render(
    <NewAgentDialog api={sessionApi(f)} catalog={catalogApi(f)} onClose={onClose} onCreated={onCreated} />,
  );
  return { requests, onCreated, onClose };
}

const field = (id: string) => document.getElementById(id) as HTMLInputElement | HTMLSelectElement;
const created = (requests: { url: string; method: string; body?: string }[]) =>
  JSON.parse(requests.find((r) => r.method === "POST")!.body!);

it("defaults to the most recently used project and the first agent's command", async () => {
  // §6.4：常用项目 2–3 次操作起会话——打开即选中最近用过的那个，Create 就能按。
  const { requests, onCreated } = setup();
  await waitFor(() => expect(field("na-project").value).toBe("/Users/r/code/agora"));
  expect(field("na-name").value).toBe("agora");
  expect(field("na-agent").value).toBe("a1");
  expect(field("na-command").value).toBe("a1");
  expect(field("na-node").value).toBe("mac");

  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("mac:new1"));
  expect(created(requests)).toMatchObject({
    display_name: "agora",
    agent_type: "a1",
    working_directory: "/Users/r/code/agora",
    worktree: null,
    task_ref: null,
  });
});

it("switching agent swaps the command, but not after the user edited it", async () => {
  // 默认值是便利不是主人：手改过 Command 之后换 agent 不许把它冲掉。
  setup();
  await waitFor(() => expect(field("na-command").value).toBe("a1"));
  fireEvent.change(field("na-agent"), { target: { value: "a2" } });
  expect(field("na-command").value).toBe("a2-bin");

  fireEvent.change(field("na-command"), { target: { value: "my-wrapper --x" } });
  fireEvent.change(field("na-agent"), { target: { value: "a1" } });
  expect(field("na-command").value).toBe("my-wrapper --x");
});

it("a linked worktree becomes the working directory; the main one does not", async () => {
  // 主 worktree 就是仓库本身，worktree 字段留空；linked worktree 才换 cwd 并记分支。
  const { requests, onCreated } = setup();
  
  await waitFor(() => expect(screen.getByRole("option", { name: /feat\/x/ })).toBeTruthy());
  fireEvent.change(field("na-worktree"), { target: { value: "/Users/r/code/agora-wt/x" } });
  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalled());
  expect(created(requests)).toMatchObject({
    working_directory: "/Users/r/code/agora-wt/x",
    worktree: "feat/x",
  });
});

it("custom needs a command before Create is enabled", async () => {
  // custom 没有 Adapter，命令只能由用户给；空着就创建会得到一个跑 "custom" 的会话。
  setup();
  await waitFor(() => expect(field("na-agent").value).toBe("a1"));
  fireEvent.change(field("na-agent"), { target: { value: "custom" } });
  expect(field("na-command").value).toBe("");
  expect((screen.getByTestId("create") as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(field("na-command"), { target: { value: "my-agent" } });
  expect((screen.getByTestId("create") as HTMLButtonElement).disabled).toBe(false);
});

it("a hand-typed project path is allowed and re-lists worktrees", async () => {
  // project_roots 默认是空的：只给下拉的话新装的 agora 一个会话都起不了。
  const { requests } = setup();
  await waitFor(() => expect(field("na-project").value).toBe("/Users/r/code/agora"));
  fireEvent.change(field("na-project"), { target: { value: "/tmp/scratch" } });
  expect(field("na-name").value).toBe("scratch");
  await waitFor(() =>
    expect(
      requests.some((r) => r.url === "/api/projects/worktrees?path=%2Ftmp%2Fscratch"),
    ).toBe(true),
  );
});
