// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike, type ReadyTask } from "./api";
import type { NodeStatus } from "./Header";
import { NewAgentDialog, nodeOptionLabel } from "./NewAgentDialog";
import { taskPrompt } from "./taskPrompt";
import { NEW_WORKTREE } from "./WorktreeSelect";

afterEach(cleanup);

const PROJECTS = {
  projects: [
    { path: "/Users/r/code/agora", name: "agora", last_used_at: "2026-09-03T10:00:00Z" },
    { path: "/Users/r/code/other", name: "other", last_used_at: null },
  ],
};
// a1 接受首条 prompt（Claude / Codex 那类），a2 不接受（Grok / shell 那类）——标志由节点给。
const AGENTS = {
  agents: [
    { name: "a1", command: "a1", prompt: true },
    { name: "a2", command: "a2-bin", prompt: false },
  ],
};
const WORKTREES = {
  worktrees: [
    { path: "/Users/r/code/agora", branch: "main", head: "abc", main: true, locked: false },
    { path: "/Users/r/code/agora-wt/x", branch: "feat/x", head: "def", main: false, locked: false },
  ],
};
const TASKS: { tasks: ReadyTask[]; reason: string | null } = {
  tasks: [
    { id: "agora-h1k.2", title: "从 bd ready 选任务起会话", priority: 2, type: "task" },
    { id: "agora-q8x", title: "hook_recovery 假阴性", priority: 1, type: "bug" },
  ],
  reason: null,
};
/** 没装 bd 的机器：旧用例都走这条路，对话框形态与 A43 之前一样。 */
const NO_BD = { tasks: [], reason: "no_bd" };

// zuan 上的目录（A45）：带 `node=zuan` 的请求答这一套，与本机的一项都不重合。
const ZUAN_PROJECTS = {
  projects: [{ path: "/home/z/code/beta", name: "beta", last_used_at: "2026-09-07T02:00:00Z" }],
};
const ZUAN_AGENTS = { agents: [{ name: "zb", command: "zb-bin", prompt: false }] };
const ZUAN_WORKTREES = {
  worktrees: [{ path: "/home/z/code/beta", branch: "main", head: "f00", main: true, locked: false }],
};
const ZUAN_NEW_WORKTREE = { path: "/home/z/code/beta-wt/beta", branch: "beta", head: "f00", main: false, locked: false };

const peer = (name: string, extra: Partial<NodeStatus> = {}): NodeStatus => ({
  name,
  online: true,
  last_seen: null,
  retrying: false,
  last_error: null,
  ...extra,
});
/** Header 给的那份：本机在线、zuan 在线、old 断线（stale）、v2 版本不兼容。 */
const NODES: NodeStatus[] = [
  peer("mac", { local: true }),
  peer("zuan"),
  peer("old", { online: false, last_seen: "2026-09-07T01:00:00Z", retrying: true, last_error: "unreachable" }),
  peer("v2", { online: false, retrying: true, last_error: "incompatible_version" }),
];

function setup(tasks: { tasks: ReadyTask[]; reason: string | null } = NO_BD, nodes?: NodeStatus[]) {
  const requests: { url: string; method: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    requests.push({ url, method: init.method ?? "GET", body: init.body as string | undefined });
    const method = init.method ?? "GET";
    const onZuan = url.includes("node=zuan") || (init.body as string | undefined)?.includes('"node":"zuan"');
    const body = url.startsWith("/api/projects/tasks")
      ? tasks
      : url.startsWith("/api/projects/worktrees")
        ? method === "POST"
          ? ZUAN_NEW_WORKTREE
          : onZuan
            ? ZUAN_WORKTREES
            : WORKTREES
        : url.startsWith("/api/projects")
          ? onZuan
            ? ZUAN_PROJECTS
            : PROJECTS
          : url.startsWith("/api/agents")
            ? onZuan
              ? ZUAN_AGENTS
              : AGENTS
            : url.startsWith("/api/system")
              ? { node: "mac" }
              : { id: onZuan ? "zuan:new7" : "mac:new1" };
    return new Response(JSON.stringify(body), {
      status: method === "POST" ? 201 : 200,
      headers: { "content-type": "application/json" },
    });
  };
  const onCreated = vi.fn();
  const onClose = vi.fn();
  render(
    <NewAgentDialog api={sessionApi(f)} catalog={catalogApi(f)} nodes={nodes} onClose={onClose} onCreated={onCreated} />,
  );
  return { requests, onCreated, onClose };
}

const field = (id: string) => document.getElementById(id) as HTMLInputElement | HTMLSelectElement;
const created = (requests: { url: string; method: string; body?: string }[]) =>
  JSON.parse(requests.find((r) => r.method === "POST" && r.url === "/api/sessions")!.body!);

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
  // 没填 prompt 就不发这个键：节点那边"没有"与"空串"是两回事。
  expect(created(requests)).not.toHaveProperty("prompt");
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
  // 就绪任务与 worktree 并行拉，同一个仓库路径。
  expect(requests.some((r) => r.url === "/api/projects/tasks?path=%2Ftmp%2Fscratch")).toBe(true);
});

it("picking a ready task prefills task_ref, Name, the worktree default name and the prompt", async () => {
  // MISSION §6.4「从就绪任务起会话」/ A43：选一个就绪任务，自动填名字、worktree 名，并把任务 id
  // 与纪律提示写进首条 prompt；task_ref 是 issue id。
  const { requests, onCreated } = setup(TASKS);
  await waitFor(() => expect(screen.getByRole("option", { name: /agora-h1k\.2 · 从 bd ready/ })).toBeTruthy());
  expect(field("na-task").tagName).toBe("SELECT");
  expect(field("na-task").value).toBe("");
  expect(document.getElementById("na-task-text"), "「一句话…」选中时有自由文本框").toBeTruthy();

  fireEvent.change(field("na-task"), { target: { value: "agora-h1k.2" } });
  expect(document.getElementById("na-task-text")).toBeNull();
  expect(field("na-name").value).toBe("从 bd ready 选任务起会话");
  const expectedPrompt = taskPrompt("agora-h1k.2", "从 bd ready 选任务起会话");
  expect((document.getElementById("na-prompt") as HTMLTextAreaElement).value).toBe(expectedPrompt);

  // Worktree「新建…」的名字默认 issue id（h1k.1 留给 A43 的入口），不再是 Name。
  await waitFor(() => expect(screen.getByRole("option", { name: "新建…" })).toBeTruthy());
  fireEvent.change(field("na-worktree"), { target: { value: NEW_WORKTREE } });
  expect(field("na-worktree-name").value).toBe("agora-h1k.2");
  fireEvent.click(screen.getByTestId("worktree-cancel"));

  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalled());
  expect(created(requests)).toMatchObject({
    display_name: "从 bd ready 选任务起会话",
    agent_type: "a1",
    task_ref: "agora-h1k.2",
    prompt: expectedPrompt,
  });
});

it("a hand-edited Name or Prompt survives picking a task; going back to free text resets the defaults", async () => {
  // 沿用现有的 nameEdited 规则：用户改过就是用户的；没改过的默认值随任务来、随任务走。
  setup(TASKS);
  await waitFor(() => expect(screen.getByRole("option", { name: /agora-q8x/ })).toBeTruthy());
  fireEvent.change(field("na-name"), { target: { value: "我的名字" } });
  fireEvent.change(field("na-task"), { target: { value: "agora-q8x" } });
  expect(field("na-name").value).toBe("我的名字");
  expect((document.getElementById("na-prompt") as HTMLTextAreaElement).value).toBe(
    taskPrompt("agora-q8x", "hook_recovery 假阴性"),
  );
  fireEvent.change(document.getElementById("na-prompt")!, { target: { value: "自己写的 prompt" } });
  fireEvent.change(field("na-task"), { target: { value: "agora-h1k.2" } });
  expect((document.getElementById("na-prompt") as HTMLTextAreaElement).value).toBe("自己写的 prompt");

  // 回到「一句话…」：没改过的 Name 会回项目名；改过的 prompt 留着。
  fireEvent.change(field("na-task"), { target: { value: "" } });
  expect(document.getElementById("na-task-text")).toBeTruthy();
  expect(field("na-name").value).toBe("我的名字");
  expect((document.getElementById("na-prompt") as HTMLTextAreaElement).value).toBe("自己写的 prompt");
});

it("without bd the Task stays a one-liner with a hint chosen by reason type", async () => {
  // 没装 bd：Task 还是一句话输入（旧形态不变），旁边一行灰字按 reason 类型给文案。
  const { requests, onCreated } = setup();
  await waitFor(() => expect(screen.getByTestId("tasks-hint").textContent).toBe("没装 bd"));
  expect(field("na-task").tagName).toBe("INPUT");
  fireEvent.change(field("na-task"), { target: { value: "把 sidebar 换掉" } });
  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalled());
  expect(created(requests)).toMatchObject({ task_ref: "把 sidebar 换掉" });
  expect(created(requests)).not.toHaveProperty("prompt");
});

it("a repository without beads gets its own hint, and unknown reasons get none", async () => {
  // 文案按类型、不按文本（MISSION §2.3 规则 10）：no_beads 与 no_bd 不同句；不认识的类型不提示。
  setup({ tasks: [], reason: "no_beads" });
  await waitFor(() => expect(screen.getByTestId("tasks-hint").textContent).toBe("该仓库没有 beads"));
  cleanup();
  setup({ tasks: [], reason: "something_new" });
  await waitFor(() => expect(field("na-project").value).toBe("/Users/r/code/agora"));
  await waitFor(() => expect(field("na-task").tagName).toBe("INPUT"));
  expect(screen.queryByTestId("tasks-hint")).toBeNull();
});

it("agents without the prompt flag get no Prompt box and the body carries no prompt", async () => {
  // 节点说这个 agent 不接受首条 prompt：不显示框、也不发——发了节点会回 400。
  const { requests, onCreated } = setup(TASKS);
  await waitFor(() => expect(screen.getByRole("option", { name: /agora-h1k\.2/ })).toBeTruthy());
  fireEvent.change(field("na-task"), { target: { value: "agora-h1k.2" } });
  expect(document.getElementById("na-prompt")).toBeTruthy();
  fireEvent.change(field("na-agent"), { target: { value: "a2" } });
  expect(document.getElementById("na-prompt")).toBeNull();
  fireEvent.change(field("na-agent"), { target: { value: "custom" } });
  expect(document.getElementById("na-prompt")).toBeNull();
  fireEvent.change(field("na-agent"), { target: { value: "a2" } });

  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalled());
  const body = created(requests);
  expect(body).toMatchObject({ agent_type: "a2", task_ref: "agora-h1k.2" });
  expect(body).not.toHaveProperty("prompt");
});

it("lists the local node and the peers; offline or incompatible peers are listed but not selectable", async () => {
  // A45：本机 + 已配置的 peer 都在下拉里，离线 / 版本不兼容的标出原因类型并 disabled——选了只会得到 502，
  // 不如在下拉里就说清楚；没给 nodes（单机）只有本机。
  setup(NO_BD, NODES);
  await waitFor(() => expect(field("na-node").value).toBe("mac"));
  const options = Array.from((field("na-node") as HTMLSelectElement).options).map((o) => ({
    value: o.value,
    text: o.textContent,
    disabled: o.disabled,
  }));
  expect(options).toEqual([
    { value: "mac", text: "mac", disabled: false },
    { value: "zuan", text: "zuan", disabled: false },
    { value: "old", text: "old · stale", disabled: true },
    { value: "v2", text: "v2 · 版本不兼容", disabled: true },
  ]);
  expect(nodeOptionLabel(peer("fresh", { online: false, retrying: true, last_error: "unreachable" }))).toBe(
    "fresh · 未连接",
  );
});

it("picking a peer re-lists everything from it and creates the session and the worktree there", async () => {
  // MISSION §1 第 3 步在 Mac 上选 zuan：Project / Worktree / Task / Agent 四个下拉换成 zuan 的
  //（五个 catalog 端点带 node=zuan），「新建 worktree」与 Create 的 body 带 node: "zuan"，
  // 响应的 id 带 zuan 前缀；换回本机后 body 不再有 node。
  const { requests, onCreated } = setup(NO_BD, NODES);
  await waitFor(() => expect(field("na-project").value).toBe("/Users/r/code/agora"));

  fireEvent.change(field("na-node"), { target: { value: "zuan" } });
  await waitFor(() => expect(field("na-project").value).toBe("/home/z/code/beta"));
  expect(field("na-name").value).toBe("beta");
  expect(field("na-agent").value).toBe("zb");
  expect(field("na-command").value).toBe("zb-bin");
  await waitFor(() => expect(screen.getByRole("option", { name: "新建…" })).toBeTruthy());
  const urls = requests.map((r) => r.url);
  expect(urls).toContain("/api/projects?node=zuan");
  expect(urls).toContain("/api/agents?node=zuan");
  expect(urls).toContain(`/api/projects/worktrees?path=${encodeURIComponent("/home/z/code/beta")}&node=zuan`);
  expect(urls).toContain(`/api/projects/tasks?path=${encodeURIComponent("/home/z/code/beta")}&node=zuan`);
  // 上一台机器的项目一个都不能留在下拉里：路径在那边。
  expect(Array.from((document.getElementById("na-projects") as HTMLDataListElement).options).map((o) => o.value)).toEqual([
    "/home/z/code/beta",
  ]);

  // 新建 worktree 也在 zuan 上建。Name 栏 = 项目名 beta，主 worktree 目录也是 beta，
  // 默认换成 beta-2（agora-h5x），否则节点回 409 worktree_exists。
  fireEvent.change(field("na-worktree"), { target: { value: NEW_WORKTREE } });
  expect(field("na-worktree-name").value).toBe("beta-2");
  fireEvent.click(screen.getByTestId("worktree-create"));
  await waitFor(() => expect(field("na-worktree").value).toBe("/home/z/code/beta-wt/beta"));
  const wt = requests.find((r) => r.method === "POST" && r.url === "/api/projects/worktrees")!;
  expect(JSON.parse(wt.body!)).toEqual({ path: "/home/z/code/beta", name: "beta-2", node: "zuan" });

  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("zuan:new7"));
  expect(created(requests)).toMatchObject({
    node: "zuan",
    display_name: "beta",
    agent_type: "zb",
    working_directory: "/home/z/code/beta-wt/beta",
    worktree: "beta",
  });

  // 换回本机：列表回到本机的，body 不带 node。
  fireEvent.change(field("na-node"), { target: { value: "mac" } });
  await waitFor(() => expect(field("na-project").value).toBe("/Users/r/code/agora"));
  fireEvent.click(screen.getByTestId("create"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("mac:new1"));
  const last = JSON.parse(requests.filter((r) => r.method === "POST" && r.url === "/api/sessions").pop()!.body!);
  expect(last).not.toHaveProperty("node");
});
