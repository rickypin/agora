// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike, type WorktreeInfo } from "./api";
import { NewAgentDialog } from "./NewAgentDialog";
import { describeWorktreeError, NEW_WORKTREE, WorktreeSelect } from "./WorktreeSelect";

afterEach(cleanup);

const MAIN: WorktreeInfo = {
  path: "/Users/r/code/agora",
  branch: "main",
  head: "abc",
  main: true,
  locked: false,
};
const NEW: WorktreeInfo = {
  path: "/Users/r/code/agora-wt/h1k",
  branch: "h1k",
  head: "abc",
  main: false,
  locked: false,
};

const field = (id: string) => document.getElementById(id) as HTMLInputElement | HTMLSelectElement;

function renderSelect(create: (path: string, name: string) => Promise<Awaited<ReturnType<ReturnType<typeof catalogApi>["createWorktree"]>>>) {
  const onChange = vi.fn();
  const onCreated = vi.fn();
  const onCreatingChange = vi.fn();
  render(
    <WorktreeSelect
      worktrees={[MAIN]}
      value={MAIN.path}
      onChange={onChange}
      disabled={false}
      project={MAIN.path}
      defaultName="agora-h1k.1"
      create={create}
      onCreated={onCreated}
      onCreatingChange={onCreatingChange}
    />,
  );
  return { onChange, onCreated, onCreatingChange };
}

it("「新建…」打开名字框、默认取 Name，确认后建并把新项交回父组件", async () => {
  // MISSION §6.4：对话框可新建 worktree，名字默认 issue id（无 bd 用 Name）。
  const create = vi.fn(async () => ({ ok: true as const, value: NEW }));
  const { onChange, onCreated, onCreatingChange } = renderSelect(create);
  expect(document.getElementById("na-worktree-name")).toBeNull();

  fireEvent.change(field("na-worktree"), { target: { value: NEW_WORKTREE } });
  // 「新建…」不是一个可选的 worktree：不告诉父组件"选中了什么"，只是打开名字框。
  expect(onChange).not.toHaveBeenCalled();
  expect(onCreatingChange).toHaveBeenLastCalledWith(true);
  expect(field("na-worktree").value).toBe(NEW_WORKTREE);
  expect(field("na-worktree-name").value).toBe("agora-h1k.1");

  fireEvent.change(field("na-worktree-name"), { target: { value: "h1k" } });
  fireEvent.click(screen.getByTestId("worktree-create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith(NEW));
  expect(create).toHaveBeenCalledWith(MAIN.path, "h1k");
  expect(onCreatingChange).toHaveBeenLastCalledWith(false);
  expect(document.getElementById("na-worktree-name")).toBeNull();
});

it("失败按错误类型给文案，名字框留着改；取消回到原选项", async () => {
  // §2.3 规则 10：前端按 error 类型分支，不做字符串匹配。
  const create = vi.fn(async () => ({
    ok: false as const,
    needsConfirmation: false as const,
    error: { error: "branch_exists", message: "分支已存在: h1k" },
  }));
  const { onCreated } = renderSelect(create);
  fireEvent.change(field("na-worktree"), { target: { value: NEW_WORKTREE } });
  fireEvent.click(screen.getByTestId("worktree-create"));
  await waitFor(() => expect(screen.getByTestId("worktree-error").textContent).toBe("同名分支已存在"));
  expect(onCreated).not.toHaveBeenCalled();
  expect(field("na-worktree-name")).toBeTruthy();

  fireEvent.click(screen.getByTestId("worktree-cancel"));
  expect(document.getElementById("na-worktree-name")).toBeNull();
  expect(field("na-worktree").value).toBe(MAIN.path);

  expect(describeWorktreeError({ error: "worktree_exists", message: "x" })).toBe("同名 worktree 已存在");
  expect(describeWorktreeError({ error: "path_exists", message: "x" })).toBe("目录已存在");
  expect(describeWorktreeError({ error: "bad_request", message: "名字不能含空白" })).toBe("名字不能含空白");
  expect(describeWorktreeError({ error: "git", message: "fatal: x" })).toBe("git 失败：fatal: x");
});

it("不是仓库（列表为空）就没有「新建…」", () => {
  render(
    <WorktreeSelect
      worktrees={[]}
      value=""
      onChange={vi.fn()}
      disabled={false}
      project="/tmp/scratch"
      defaultName="scratch"
      create={vi.fn()}
      onCreated={vi.fn()}
    />,
  );
  expect(screen.queryByRole("option", { name: "新建…" })).toBeNull();
});

it("对话框里：新建成功后重拉列表并选中新项，起会话就落在新 worktree 里", async () => {
  // A44 的完整路径：POST { path, name } → 201 → 再 GET 一次 → 下拉选中它 → Create 用它当 cwd。
  let created = false;
  const requests: { url: string; method: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const headers = { "content-type": "application/json" };
    if (url === "/api/projects/worktrees" && method === "POST") {
      created = true;
      return new Response(JSON.stringify(NEW), { status: 201, headers });
    }
    const body = url.startsWith("/api/projects/worktrees")
      ? { worktrees: created ? [MAIN, NEW] : [MAIN] }
      : url.startsWith("/api/projects")
        ? { projects: [{ path: MAIN.path, name: "agora", last_used_at: null }] }
        : url.startsWith("/api/agents")
          ? { agents: [{ name: "a1", command: "a1" }] }
          : url.startsWith("/api/system")
            ? { node: "mac" }
            : { id: "mac:new1" };
    return new Response(JSON.stringify(body), { status: method === "POST" ? 201 : 200, headers });
  };
  const onCreated = vi.fn();
  render(<NewAgentDialog api={sessionApi(f)} catalog={catalogApi(f)} onClose={vi.fn()} onCreated={onCreated} />);
  await waitFor(() => expect(screen.getByRole("option", { name: "新建…" })).toBeTruthy());

  fireEvent.change(field("na-worktree"), { target: { value: NEW_WORKTREE } });
  // 默认名字 = Name 栏（此时是项目名）。
  expect(field("na-worktree-name").value).toBe("agora");
  // 名字框开着时 Create 禁用：别在 worktree 还没建好时把会话起在仓库本身。
  expect((screen.getByTestId("create") as HTMLButtonElement).disabled).toBe(true);

  fireEvent.change(field("na-worktree-name"), { target: { value: "h1k" } });
  fireEvent.click(screen.getByTestId("worktree-create"));
  await waitFor(() => expect(field("na-worktree").value).toBe(NEW.path));
  const post = requests.find((r) => r.method === "POST" && r.url === "/api/projects/worktrees")!;
  expect(JSON.parse(post.body!)).toEqual({ path: MAIN.path, name: "h1k" });
  expect(requests.filter((r) => r.method === "GET" && r.url.startsWith("/api/projects/worktrees")).length).toBe(2);
  expect(screen.getByRole("option", { name: "h1k ↗" })).toBeTruthy();
  expect((screen.getByTestId("create") as HTMLButtonElement).disabled).toBe(false);

  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("mac:new1"));
  const session = requests.find((r) => r.method === "POST" && r.url === "/api/sessions")!;
  expect(JSON.parse(session.body!)).toMatchObject({ working_directory: NEW.path, worktree: "h1k" });
});
