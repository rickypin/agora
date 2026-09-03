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

export type WriteResult<T = unknown> =
  | { ok: true; value: T }
  | { ok: false; needsConfirmation: true }
  | { ok: false; needsConfirmation: false; error: ApiErrorBody };

export type FetchLike = (url: string, init: RequestInit) => Promise<Response>;

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
}

export interface CreateSessionBody {
  display_name: string;
  agent_type: string;
  working_directory: string;
  worktree?: string | null;
  task_ref?: string | null;
  command?: string;
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

// 只发往 /api/（路径全部由 enc() 生成；tests/arch_boundary.rs 逐行守卫前端只连 /api/）。
export function sessionApi(fetchImpl: FetchLike = (u, i) => fetch(u, i) /* /api/ */) {
  return {
    /** 改名：改成同名字符串也发——同名也落锁（MISSION §4.5）。 */
    rename: (id: string, display_name: string) =>
      call(fetchImpl, "PATCH", enc(id), { display_name }),
    kill: (id: string, confirmed = false) =>
      call(fetchImpl, "POST", `${enc(id)}/kill`, confirmed ? { confirmed: true } : {}),
    restart: (id: string, confirmed = false) =>
      call(fetchImpl, "POST", `${enc(id)}/restart`, confirmed ? { confirmed: true } : {}),
    /** 只删 metadata，不 kill（DELETE ≠ kill，MISSION §7.3）。 */
    deleteMetadata: (id: string) => call(fetchImpl, "DELETE", enc(id)),
    /** New Agent 对话框的创建（§6.4）；201 的响应体就是新会话那一行。 */
    create: (body: CreateSessionBody) =>
      call<{ id: string }>(fetchImpl, "POST", "/api/sessions", body),
  };
}

export type SessionApi = ReturnType<typeof sessionApi>;

/**
 * New Agent 对话框的三个只读数据源（§6.4）。与写端点分开：它们没有确认语义，
 * 401 之外的失败只影响下拉框，不该走 WriteResult 那套。
 */
export function catalogApi(fetchImpl: FetchLike = (u, i) => fetch(u, i) /* /api/ */) {
  return {
    projects: () => call<{ projects: ProjectInfo[] }>(fetchImpl, "GET", "/api/projects"),
    worktrees: (path: string) =>
      call<{ worktrees: WorktreeInfo[] }>(
        fetchImpl,
        "GET",
        `/api/projects/worktrees?path=${encodeURIComponent(path)}`,
      ),
    agents: () => call<{ agents: AgentInfo[] }>(fetchImpl, "GET", "/api/agents"),
    system: () => call<{ node: string }>(fetchImpl, "GET", "/api/system"),
  };
}

export type CatalogApi = ReturnType<typeof catalogApi>;
