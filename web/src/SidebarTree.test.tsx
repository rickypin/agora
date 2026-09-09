// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionRow } from "./events";
import type { NodeStatus } from "./Header";
import { SidebarTree } from "./SidebarTree";
import { COLLAPSED_STORAGE_KEY, treeOrder } from "./sidebarTreeModel";

afterEach(cleanup);
beforeEach(() => localStorage.clear());

const AGORA = "/Users/ricky/code/agora";
const WT3 = "/Users/ricky/code/agora-wt/agora-uvd.3";
const main = { repo: AGORA, name: "agora", worktree: AGORA, branch: "main", main: true };
const wt3 = { repo: AGORA, name: "agora", worktree: WT3, branch: "agora-uvd.3", main: false };
const detached = { repo: "/code/sglog", name: "sglog", worktree: "/code/sglog", branch: null, main: true };

function row(id: string, node: string, created: string, project: SessionRow["project"], extra: Partial<SessionRow> = {}): SessionRow {
  return { id, node, status: "running", alive: true, created_at: created, project, agent_type: "claude", display_name: id, ...extra };
}

const node = (name: string, over: Partial<NodeStatus> = {}): NodeStatus => ({ name, online: true, last_seen: null, retrying: false, last_error: null, ...over });
const NODES = [node("mac", { local: true }), node("zuan", { online: false, last_seen: "2026-09-09T01:00:00Z" })];

const ROWS = [
  row("mac:a", "mac", "2026-09-08T12:00:01Z", main),
  row("mac:fin", "mac", "2026-09-08T12:00:02Z", main, { status: "finished", origin: "agora", status_since: 20 }),
  row("mac:w", "mac", "2026-09-08T12:00:03Z", wt3, { status: "waiting" }),
  row("mac:sg", "mac", "2026-09-08T12:00:04Z", detached),
  row("mac:sh", "mac", "2026-09-08T12:00:05Z", null, { agent_type: "shell" }),
  row("zuan:z", "zuan", "2026-09-08T12:00:06Z", main, { status: "turn_done" }),
];

function mount(extra: Partial<Parameters<typeof SidebarTree>[0]> = {}, rows = ROWS) {
  const onOpen = vi.fn();
  const visible = treeOrder(rows, NODES, "mac");
  const props = { rows: visible, all: rows, nodes: NODES, localNode: "mac", active: null, onOpen, now: 0, ...extra };
  const ui = render(<SidebarTree {...props} />);
  return { ...ui, onOpen, props };
}

/** 组头与行的先后：只看 tree-group-* 与 row-* 两类 testid。 */
function order(): string[] {
  return Array.from(document.querySelectorAll("[data-testid]"))
    .map((el) => el.getAttribute("data-testid")!)
    .filter((id) => id.startsWith("tree-group-") || id.startsWith("row-"))
    .filter((id) => !id.startsWith("tree-group-attention-") && !id.startsWith("row-node-"));
}

it("group headers show branch, main marker and attention count when collapsed (A48)", () => {
  mount();
  expect(order()).toEqual([
    "tree-group-node:mac",
    `tree-group-repo:mac:${AGORA}`,
    `tree-group-wt:mac:${AGORA}`,
    "row-mac:a",
    "row-mac:fin",
    `tree-group-wt:mac:${WT3}`,
    "row-mac:w",
    "tree-group-repo:mac:/code/sglog",
    "tree-group-wt:mac:/code/sglog",
    "row-mac:sg",
    "tree-group-other:mac",
    "row-mac:sh",
    "tree-group-node:zuan",
    `tree-group-repo:zuan:${AGORA}`,
    `tree-group-wt:zuan:${AGORA}`,
    "row-zuan:z",
  ]);
  // 节点组头：Header 同源的点（在线 ok ●，掉线 stale ○）+ 名字。
  const mac = screen.getByTestId("tree-group-node:mac");
  expect(mac.querySelector(".node-status.ok .dot")?.textContent).toBe("●");
  expect(mac.querySelector(".node-name")?.textContent).toBe("mac");
  expect(screen.getByTestId("tree-group-node:zuan").querySelector(".node-status.stale .dot")?.textContent).toBe("○");
  // 仓库组头：name，title 是仓库路径。
  const repo = screen.getByTestId(`tree-group-repo:mac:${AGORA}`);
  expect(repo.textContent).toBe("▾agora");
  expect(repo.getAttribute("title")).toBe(AGORA);
  // worktree 组头：label ⎇ branch [主]；detached 显示 ⎇ detached。
  expect(screen.getByTestId(`tree-group-wt:mac:${AGORA}`).textContent).toBe("▾agora⎇ main主");
  expect(screen.getByTestId(`tree-group-wt:mac:${WT3}`).textContent).toBe("▾agora-uvd.3⎇ agora-uvd.3");
  expect(screen.getByTestId(`tree-group-wt:mac:${WT3}`).getAttribute("title")).toBe(WT3);
  expect(screen.getByTestId("tree-group-wt:mac:/code/sglog").textContent).toBe("▾sglog⎇ detached主");
  // 序号是 DFS 序：a=1 … zuan:z=6。
  expect(screen.getByTestId("row-mac:w").querySelector(".ord")?.textContent).toBe("3");
  expect(screen.getByTestId("row-zuan:z").querySelector(".ord")?.textContent).toBe("6");
  // 展开时不显示计数。
  expect(screen.queryByTestId("tree-group-attention-node:mac")).toBeNull();
  // 折叠 mac 节点组：它下面的组头与行都不画，序号照数（zuan:z 仍是 6）；组头右侧「2 需要关注」——
  // waiting 的 w 与没看过的 agora FINISHED fin；running 的 a 与 sg 不算。
  fireEvent.click(screen.getByTestId("tree-group-node:mac"));
  expect(screen.getByTestId("tree-group-node:mac").getAttribute("aria-expanded")).toBe("false");
  expect(order()).toEqual(["tree-group-node:mac", "tree-group-node:zuan", `tree-group-repo:zuan:${AGORA}`, `tree-group-wt:zuan:${AGORA}`, "row-zuan:z"]);
  expect(screen.getByTestId("tree-group-attention-node:mac").textContent).toBe("2 需要关注");
  expect(screen.getByTestId("row-zuan:z").querySelector(".ord")?.textContent).toBe("6");
  // 折叠只有 running 行的 sglog worktree 组：0 不显示。
  fireEvent.click(screen.getByTestId("tree-group-node:mac"));
  fireEvent.click(screen.getByTestId("tree-group-wt:mac:/code/sglog"));
  expect(screen.queryByTestId("row-mac:sg")).toBeNull();
  expect(screen.queryByTestId("tree-group-attention-wt:mac:/code/sglog")).toBeNull();
  // 计数按过滤前的 all 算：rows 过滤到只剩 sg，折叠的 agora-uvd.3 组仍报 1。
  cleanup();
  const filtered = ROWS.filter((r) => r.id === "mac:sg" || r.id === "mac:w");
  localStorage.setItem(COLLAPSED_STORAGE_KEY, JSON.stringify([`wt:mac:${WT3}`]));
  mount({ rows: treeOrder(filtered, NODES, "mac") });
  expect(screen.getByTestId(`tree-group-attention-wt:mac:${WT3}`).textContent).toBe("1 需要关注");
  // 看过的 agora FINISHED 不算需要关注。
  cleanup();
  localStorage.setItem(COLLAPSED_STORAGE_KEY, JSON.stringify(["node:mac"]));
  mount({ seen: new Set(["mac:fin@20"]) });
  expect(screen.getByTestId("tree-group-attention-node:mac").textContent).toBe("1 需要关注");
});

it("a finished row stays in place with the done class (A48)", () => {
  const { rerender, props } = mount();
  const before = order();
  expect(screen.getByTestId("row-mac:fin").closest("li.done")).not.toBeNull();
  expect(screen.getByTestId("row-mac:a").closest("li.done")).toBeNull();
  // 状态大换血：a 完成、fin 又跑起来、w 失败——DOM 顺序一个字不变，只有 done 类换了主人。
  const changed = ROWS.map((r) => {
    if (r.id === "mac:a") return { ...r, status: "finished", status_since: 99 };
    if (r.id === "mac:fin") return { ...r, status: "running", status_since: 100 };
    if (r.id === "mac:w") return { ...r, status: "failed" };
    return r;
  });
  rerender(<SidebarTree {...props} rows={treeOrder(changed, NODES, "mac")} all={changed} />);
  expect(order()).toEqual(before);
  expect(screen.getByTestId("row-mac:a").closest("li.done")).not.toBeNull();
  expect(screen.getByTestId("row-mac:fin").closest("li.done")).toBeNull();
  expect(screen.getByTestId("row-mac:w").querySelector(".dot")?.textContent).toBe("✗");
  // 没有 Finished 折叠区、没有三段标题。
  expect(screen.queryByTestId("section-finished")).toBeNull();
});

it("tree rows draw no project line: the group header already says repo and branch (agora-uvd.8 / agora-s7o)", () => {
  mount();
  expect(document.querySelector("[data-testid^='project-']")).toBeNull();
  expect(screen.getByTestId("row-mac:a")).toBeTruthy();
});

it("clicking a group header toggles it and persists to localStorage", () => {
  mount();
  const head = screen.getByTestId(`tree-group-wt:mac:${WT3}`);
  expect(head.getAttribute("aria-expanded")).toBe("true");
  fireEvent.click(head);
  expect(screen.getByTestId(`tree-group-wt:mac:${WT3}`).getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByTestId("row-mac:w")).toBeNull();
  expect(JSON.parse(localStorage.getItem(COLLAPSED_STORAGE_KEY)!)).toEqual([`wt:mac:${WT3}`]);
  // 折叠的组头自己还在，能再点开。
  fireEvent.click(screen.getByTestId(`tree-group-wt:mac:${WT3}`));
  expect(screen.getByTestId("row-mac:w")).toBeTruthy();
  expect(JSON.parse(localStorage.getItem(COLLAPSED_STORAGE_KEY)!)).toEqual([]);
  // 重新挂载：折叠记忆从 localStorage 回来。
  fireEvent.click(screen.getByTestId("tree-group-node:zuan"));
  cleanup();
  mount();
  expect(screen.getByTestId("tree-group-node:zuan").getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByTestId("row-zuan:z")).toBeNull();
  expect(screen.getByTestId("tree-group-node:mac").getAttribute("aria-expanded")).toBe("true");
});

it("the worktree + button opens the dialog prefilled with node/project/worktree without toggling the group (A48)", () => {
  // MISSION §6.4「常用项目最多 2–3 次操作」：树已经站在这个 worktree 上，「+」把三个下拉都带过去，
  // 对话框里只剩选 Agent。点按钮不折叠这一组（它在组头按钮外面，且显式挡了冒泡）。
  const onNewAgent = vi.fn();
  mount({ onNewAgent, nodes: [node("mac", { local: true }), node("zuan")] });
  fireEvent.click(screen.getByTestId(`tree-new-agent-wt:mac:${WT3}`));
  expect(onNewAgent).toHaveBeenCalledWith({ node: null, project: AGORA, worktree: WT3 });
  expect(screen.getByTestId(`tree-group-wt:mac:${WT3}`).getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByTestId("row-mac:w")).toBeTruthy();
  // peer 上的 worktree 同样支持：node 带过去，POST 由节点一跳转发（A45）。
  fireEvent.click(screen.getByTestId(`tree-new-agent-wt:zuan:${AGORA}`));
  expect(onNewAgent).toHaveBeenLastCalledWith({ node: "zuan", project: AGORA, worktree: AGORA });
  // 节点组头只预填 Node；「其它目录」组头没有按钮（不是一个能在里面干活的目录）。
  fireEvent.click(screen.getByTestId("tree-new-agent-node-mac"));
  expect(onNewAgent).toHaveBeenLastCalledWith({ node: null });
  fireEvent.click(screen.getByTestId("tree-new-agent-node-zuan"));
  expect(onNewAgent).toHaveBeenLastCalledWith({ node: "zuan" });
  expect(screen.queryByTestId("tree-new-agent-other:mac")).toBeNull();
  expect(screen.queryByTestId("tree-new-shell-other:mac")).toBeNull();
  expect(screen.queryByTestId(`tree-new-shell-repo:mac:${AGORA}`)).toBeNull();
});

it("the shell button posts one shell session in that worktree and reports failure inline (A48)", async () => {
  // 「shell」不开对话框、不问名字（取舍）：名字 = worktree 目录名，cwd = 该 worktree，
  // worktree 字段是分支名（docs/spec/api.md）。成功交给 onCreated，失败在组头下一行说清楚。
  const create = vi.fn().mockResolvedValue({ ok: true, value: { id: "mac:new9" } });
  const onCreated = vi.fn();
  mount({ api: { create }, onCreated });
  fireEvent.click(screen.getByTestId(`tree-new-shell-wt:mac:${WT3}`));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("mac:new9"));
  expect(create).toHaveBeenCalledTimes(1);
  expect(create).toHaveBeenCalledWith({
    display_name: "agora-uvd.3",
    agent_type: "shell",
    working_directory: WT3,
    worktree: "agora-uvd.3",
  });
  // 主 worktree：worktree 字段留空（仓库本身），与 New Agent 对话框同一条规则。
  fireEvent.click(screen.getByTestId(`tree-new-shell-wt:mac:${AGORA}`));
  await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
  expect(create).toHaveBeenLastCalledWith({
    display_name: "agora",
    agent_type: "shell",
    working_directory: AGORA,
    worktree: null,
  });
  expect(screen.queryByTestId(`tree-shell-error-wt:mac:${AGORA}`)).toBeNull();

  // 失败：按 WriteResult 的错误类型 + message 在组头下一行显示，不弹窗、不静默。
  create.mockResolvedValue({ ok: false, needsConfirmation: false, error: { error: "runtime", message: "tmux 没起来" } });
  fireEvent.click(screen.getByTestId(`tree-new-shell-wt:mac:${WT3}`));
  const err = await screen.findByTestId(`tree-shell-error-wt:mac:${WT3}`);
  expect(err.textContent).toBe("runtime: tmux 没起来");
  expect(onCreated).toHaveBeenCalledTimes(2);
});

it("the shell button is disabled while its own POST is in flight and a second click posts nothing (A48)", async () => {
  // 受控 pending promise 把 in-flight 状态钉住：真实时序下这个窗口比任何断言的调度都短
  // （本机 POST 只有 22–42 ms，agora-x1k 2026-09-09 实测），只有手动 release 才测得到。
  let release!: (v: unknown) => void;
  const create = vi.fn().mockReturnValue(new Promise((r) => { release = r; }));
  mount({ api: { create }, onCreated: vi.fn() });
  const btn = () => screen.getByTestId(`tree-new-shell-wt:mac:${WT3}`) as HTMLButtonElement;
  fireEvent.click(btn());
  expect(create).toHaveBeenCalledTimes(1);
  expect(btn().disabled).toBe(true);
  fireEvent.click(btn());
  expect(create).toHaveBeenCalledTimes(1);
  release({ ok: true, value: { id: "mac:new1" } });
  await waitFor(() => expect(btn().disabled).toBe(false));
});

it("a shell POST in flight does not disable the other worktrees' shell buttons (A48)", async () => {
  // 同时对两个 worktree 起 shell 是正当用法。in-flight 状态按组 key 存（不是全局标志），
  // 否则在途期间别的组头一起灰、点了还会被 openShell 的早退静默吞掉——本机 22–42 ms 看不见，
  // peer 一跳转发时是「按了没反应」。
  let release!: (v: unknown) => void;
  const create = vi.fn().mockReturnValue(new Promise((r) => { release = r; }));
  mount({ api: { create }, onCreated: vi.fn() });
  fireEvent.click(screen.getByTestId(`tree-new-shell-wt:mac:${WT3}`));
  const other = screen.getByTestId(`tree-new-shell-wt:mac:${AGORA}`) as HTMLButtonElement;
  expect(other.disabled).toBe(false);
  fireEvent.click(other);
  expect(create).toHaveBeenCalledTimes(2);
  release({ ok: true, value: { id: "mac:new1" } });
  await waitFor(() => expect(other.disabled).toBe(false));
});

it("group header buttons on a stale node are disabled (A48)", () => {
  // zuan 离线（stale）：一跳转发到不了，按了只会得到 502——按钮就是灰的，不弹错误。本机的照常。
  const onNewAgent = vi.fn();
  mount({ onNewAgent, api: { create: vi.fn() } });
  const disabled = (id: string) => (screen.getByTestId(id) as HTMLButtonElement).disabled;
  expect(disabled("tree-new-agent-node-zuan")).toBe(true);
  expect(disabled(`tree-new-agent-wt:zuan:${AGORA}`)).toBe(true);
  expect(disabled(`tree-new-shell-wt:zuan:${AGORA}`)).toBe(true);
  expect(screen.getByTestId(`tree-new-shell-wt:zuan:${AGORA}`).getAttribute("title")).toBe("节点离线");
  expect(disabled("tree-new-agent-node-mac")).toBe(false);
  expect(disabled(`tree-new-agent-wt:mac:${WT3}`)).toBe(false);
  expect(screen.getByTestId(`tree-new-shell-wt:mac:${WT3}`).getAttribute("title")).toBe("在此开 shell");
  fireEvent.click(screen.getByTestId(`tree-new-agent-wt:zuan:${AGORA}`));
  expect(onNewAgent).not.toHaveBeenCalled();
});

it("the active row inside a collapsed group expands it once", () => {
  // Alt/Option+N、通知点击、命令面板都能从外面选中折叠组里的行（agora-4nk 同一条规则）。
  localStorage.setItem(COLLAPSED_STORAGE_KEY, JSON.stringify(["node:zuan", `wt:mac:${WT3}`]));
  const expanded = (r: SessionRow) => <div data-testid={`expanded-${r.id}`} />;
  const { rerender, props } = mount({ active: "mac:a", renderExpanded: expanded });
  expect(screen.queryByTestId("row-zuan:z")).toBeNull();
  expect(screen.queryByTestId("row-mac:w")).toBeNull();
  rerender(<SidebarTree {...props} active="zuan:z" renderExpanded={expanded} />);
  // 那条路径上的组都展开了；别的折叠组不动。
  expect(screen.getByTestId("tree-group-node:zuan").getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByTestId("row-zuan:z").closest("li.selected")).not.toBeNull();
  expect(screen.getByTestId("expanded-zuan:z")).toBeTruthy();
  expect(screen.queryByTestId("row-mac:w")).toBeNull();
  expect(JSON.parse(localStorage.getItem(COLLAPSED_STORAGE_KEY)!)).toEqual([`wt:mac:${WT3}`]);
  // 人再手动收起：active 还在里面也收得起来（只展开一次，不派生）。
  fireEvent.click(screen.getByTestId("tree-group-node:zuan"));
  expect(screen.queryByTestId("row-zuan:z")).toBeNull();
  // 换到另一个折叠组里的行又展开那一组。
  rerender(<SidebarTree {...props} active="mac:w" renderExpanded={expanded} />);
  expect(screen.getByTestId("row-mac:w").closest("li.selected")).not.toBeNull();
  expect(screen.queryByTestId("row-zuan:z")).toBeNull();
});
