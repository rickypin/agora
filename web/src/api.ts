/**
 * Session Settings 用的写端点（docs/spec/api.md；MISSION §4.6 §8）。
 *
 * Kill / Restart 的确认跟着"杀"走：先不带 `confirmed` 发，节点判断会杀而未确认 →
 * 409 `needs_confirmation`，前端这时才弹确认框，确认后带 `confirmed: true` 重发。
 * 不会杀的（FINISHED / FAILED）直接执行，不多问一次。
 */

export interface ApiErrorBody {
  error: string;
  message: string;
}

import { apiFetch, type FetchLike } from "./net";

export type WriteResult<T = unknown> =
  | { ok: true; value: T }
  | { ok: false; needsConfirmation: true }
  | { ok: false; needsConfirmation: false; error: ApiErrorBody };

export type { FetchLike } from "./net";

/** `GET /api/projects` 的一项（MISSION §6.4：扫描发现 + 最近使用）。 */
export interface ProjectInfo {
  path: string;
  name: string;
  last_used_at: string | null;
}

/** `GET /api/projects/worktrees` 的一项；detached HEAD 没有 branch。 */
export interface WorktreeInfo {
  path: string;
  branch: string | null;
  head: string | null;
  main: boolean;
  locked: boolean;
}

/** `GET /api/agents` 的一项：名字与默认命令都来自 Adapter，前端不写死（ADR-002 D2）。 */
export interface AgentInfo {
  name: string;
  command: string;
  /** 接不接受首条 prompt（A43）：只对 true 的 agent 显示 Prompt 框并随 body 发 `prompt`。 */
  prompt: boolean;
}

/** `GET /api/projects/tasks` 的一项：`bd ready --json` 里可起会话的任务（epic 节点已滤掉）。 */
export interface ReadyTask {
  id: string;
  title: string;
  /** bd 的 P0–P4。 */
  priority: number;
  /** bd 的 issue_type：task / bug / feature / chore… */
  type: string;
}

/**
 * `GET /api/projects/tasks` 没给出列表的原因，按类型（docs/spec/api.md「从就绪任务起会话」）；
 * 成功时 null。节点将来可能加新值，所以字段类型放宽到 string，文案表不认识的不提示。
 */
export type ReadyTasksReason = "no_bd" | "no_beads" | "timeout" | "bad_output";

/** `POST /api/sessions/:id/input`（MISSION §7.3；ADR-002 D5）。 */
export type InputBody =
  | { kind: "decision"; decision: "allow" | "deny"; message?: string; tool_use_id?: string; request_id?: string }
  | { kind: "text"; data: string };

/** `POST /api/sessions/adopt`（MISSION §5.5）：用户填的 agent_type 优先于进程树给的 hint。 */
export interface AdoptBody {
  runtime_ref: string;
  display_name?: string;
  project?: string;
  agent_type?: string;
}

export interface CreateSessionBody {
  display_name: string;
  agent_type: string;
  working_directory: string;
  worktree?: string | null;
  task_ref?: string | null;
  command?: string;
  /** 首条 prompt（A43）：只进这一代的启动命令、不进库、Restart 不重发；非空才发。 */
  prompt?: string;
}

export interface RestartResult {
  restart?: { resumed: boolean; agent_session_id?: string; reason?: string };
}

async function call<T>(
  fetchImpl: FetchLike,
  method: string,
  path: string,
  body?: unknown,
): Promise<WriteResult<T>> {
  const resp = await fetchImpl(path, {
    method,
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (resp.status === 204) return { ok: true, value: undefined as T };
  let parsed: unknown = null;
  try {
    parsed = await resp.json();
  } catch {
    parsed = null;
  }
  if (resp.ok) return { ok: true, value: parsed as T };
  const err = (parsed as ApiErrorBody | null) ?? { error: "unknown", message: `HTTP ${resp.status}` };
  if (resp.status === 409 && err.error === "needs_confirmation") {
    return { ok: false, needsConfirmation: true };
  }
  return { ok: false, needsConfirmation: false, error: err };
}

const enc = (id: string) => `/api/sessions/${encodeURIComponent(id)}`;

// 路径全部由 enc() 生成；真正发请求的是 net.ts 里唯一的出口（tests/arch_boundary.rs）。
export function sessionApi(fetchImpl: FetchLike = apiFetch) {
  return {
    /** 改名：改成同名字符串也发——同名也落锁（MISSION §4.5）。 */
    rename: (id: string, display_name: string) =>
      call(fetchImpl, "PATCH", enc(id), { display_name }),
    kill: (id: string, confirmed = false) =>
      call(fetchImpl, "POST", `${enc(id)}/kill`, confirmed ? { confirmed: true } : {}),
    /** 响应多一个 `restart`：节点说这次是 resume 了哪个对话，还是退化为原命令与原因（ADR-002 D7）。 */
    restart: (id: string, confirmed = false) =>
      call<RestartResult>(fetchImpl, "POST", `${enc(id)}/restart`, confirmed ? { confirmed: true } : {}),
    /** 只删 metadata，不 kill（DELETE ≠ kill，MISSION §7.3）。 */
    deleteMetadata: (id: string) => call(fetchImpl, "DELETE", enc(id)),
    /** 就地 respond：decision 经挂起的 hook 返回，text 经 PTY（MISSION §7.3）。 */
    input: (id: string, body: InputBody) => call(fetchImpl, "POST", `${enc(id)}/input`, body),
    /** New Agent 对话框的创建（§6.4）；201 的响应体就是新会话那一行。 */
    create: (body: CreateSessionBody) =>
      call<{ id: string }>(fetchImpl, "POST", "/api/sessions", body),
    /** 采纳未登记的运行时会话（§5.5）；201 的响应体就是新会话那一行。 */
    adopt: (body: AdoptBody) => call<{ id: string }>(fetchImpl, "POST", "/api/sessions/adopt", body),
  };
}

export type SessionApi = ReturnType<typeof sessionApi>;

/**
 * New Agent 对话框的数据源（§6.4）：三个只读的，加一个写——新建 worktree（agora 只管"生"，
 * A44）。与会话的写端点分开：这里没有确认语义，401 之外的失败只影响下拉框。
 */
export function catalogApi(fetchImpl: FetchLike = apiFetch) {
  return {
    projects: () => call<{ projects: ProjectInfo[] }>(fetchImpl, "GET", "/api/projects"),
    worktrees: (path: string) =>
      call<{ worktrees: WorktreeInfo[] }>(
        fetchImpl,
        "GET",
        `/api/projects/worktrees?path=${encodeURIComponent(path)}`,
      ),
    /**
     * `POST /api/projects/worktrees`（MISSION §6.4「只管"生"」；docs/spec/api.md）：201 的响应体就是
     * GET 会列出的那一项。`base` 不传由节点按缺省链定（主 worktree 当前分支 → origin/HEAD → main）。
     */
    createWorktree: (path: string, name: string, base?: string) =>
      call<WorktreeInfo>(
        fetchImpl,
        "POST",
        "/api/projects/worktrees",
        base ? { path, name, base } : { path, name },
      ),
    agents: () => call<{ agents: AgentInfo[] }>(fetchImpl, "GET", "/api/agents"),
    /**
     * `GET /api/projects/tasks?path=`（MISSION §6.4 从就绪任务起会话；A43）：该仓库 `bd ready` 的
     * 就绪任务。永远 200——没装 bd / 没有 beads 是空列表 + 类型化的 `reason`，不是错误。
     */
    tasks: (path: string) =>
      call<{ tasks: ReadyTask[]; reason: ReadyTasksReason | string | null }>(
        fetchImpl,
        "GET",
        `/api/projects/tasks?path=${encodeURIComponent(path)}`,
      ),
    system: () => call<{ node: string }>(fetchImpl, "GET", "/api/system"),
  };
}

export type CatalogApi = ReturnType<typeof catalogApi>;
