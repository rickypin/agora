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
  };
}

export type SessionApi = ReturnType<typeof sessionApi>;
