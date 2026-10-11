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
  /** `status` 是 HTTP 状态码：老节点没有某个端点时只有它说得清（405，正文不是 JSON）。 */
  | { ok: false; needsConfirmation: false; error: ApiErrorBody; status?: number };

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

/** `GET /api/presets` 的一项（agora-prdg.4）：桌面 / 终端里 `agora preset` 定义好的"一键启动"。 */
export interface PresetInfo {
  name: string;
  agent_type: string;
  /** CLI 存的是 canonical 过的绝对路径（add 时校验过存在；之后被删就在起会话时报 400）。 */
  working_directory: string;
  /** 启动参数字符串，原样（经 shell 接在裸命令名之后）；没有是 null。 */
  args: string | null;
  /** 固定首句（A43 的首条 prompt）；没有是 null。 */
  prompt: string | null;
  updated_at: string;
}

interface CreateSessionBodyExplicit {
  /** 在哪个节点起（A45）：不发 = 本机；peer 名 → 节点经一跳转发在那台机器上起，响应的 id 带它的前缀。 */
  node?: string;
  display_name: string;
  agent_type: string;
  working_directory: string;
  worktree?: string | null;
  task_ref?: string | null;
  command?: string;
  /** 首条 prompt（A43）：只进这一代的启动命令、不进库、Restart 不重发；非空才发。 */
  prompt?: string;
}

/**
 * `POST /api/sessions` 的 body（agora-prdg.4 起二选一）：显式字段（New Agent 对话框 / 侧栏 shell）
 * 或预设（手机「新建」，只给 preset）。两种不能混——预设就是完整的启动定义，混给节点回 400 不猜。
 */
export type CreateSessionBody =
  | {
      /** 预设起的会话也只支持本机（发 node 就是 400）；预留字段是为了两个形态的键集一致。 */
      node?: string;
      /**
       * 用预设起会话（agora-prdg.4）：节点展开成 agent_type / 目录 / 启动参数（launch_args）/ 首句，
       * display_name 缺省是预设名。
       */
      preset: string;
    }
  | CreateSessionBodyExplicit;

export interface RestartResult {
  restart?: { resumed: boolean; agent_session_id?: string; reason?: string };
}

/** `GET /api/sessions/:id/changes`（MISSION §6.3 看结果；A41，agora-h1k.5）：会话工作目录的 git status，只读。 */
export interface ChangesInfo {
  files: { path: string; status: string }[];
  branch: string | null;
  /** 列表为空的类型化原因（not_a_repo / no_directory / no_git / timeout / git，docs/spec/api.md）；正常 null。 */
  reason: string | null;
}

/**
 * `GET /api/sessions/:id/turns`（A53，agora-2mff；docs/spec/api.md「轮次」）的一轮：节点从 hook 事件记下的
 * 人话与最终回复。`prompt` 为 null 是注入的一轮（`injected`）或没有 prompt 的半轮；`outcome` 的
 * `no_reply` = 下一轮已开始、这一轮没收到结束事件。时刻是 unix 秒。
 */
export interface TurnInfo {
  prompt: string | null;
  injected: boolean;
  reply: string | null;
  outcome: "open" | "done" | "failed" | "no_reply";
  failure: string | null;
  started_at: number | null;
  ended_at: number | null;
}

async function call<T>(
  fetchImpl: FetchLike,
  method: string,
  path: string,
  body?: unknown,
): Promise<WriteResult<T>> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  // 2026-10-11: a disconnected response must leave the busy state too. Never auto-retry a write:
  // the server may have accepted it before the connection was lost (agora-fmsd).
  try {
    const request = async (): Promise<WriteResult<T>> => {
      const resp = await fetchImpl(path, {
        signal: controller.signal,
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (resp.status === 204) return { ok: true, value: undefined as T };
      let parsed: unknown = null;
      try {
        parsed = await resp.json();
      } catch (error) {
        if (resp.ok || controller.signal.aborted) throw error;
        parsed = null;
      }
      if (resp.ok) return { ok: true, value: parsed as T };
      const err = (parsed as ApiErrorBody | null) ?? { error: "unknown", message: `HTTP ${resp.status}` };
      if (resp.status === 409 && err.error === "needs_confirmation") {
        return { ok: false, needsConfirmation: true };
      }
      return { ok: false, needsConfirmation: false, error: err, status: resp.status };
    };
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        controller.abort();
        reject(new Error("request_timeout"));
      }, 30_000);
    });
    return await Promise.race([request(), deadline]);
  } catch {
    return {
      ok: false, needsConfirmation: false,
      error: {
        error: controller.signal.aborted ? "request_timeout" : "network_error",
        message: method === "GET"
          ? "网络连接中断或请求超时，请检查连接后重试。"
          : "连接中断或请求超时，无法确认操作是否完成。请先查看会话状态，再决定是否重试，以免重复执行。",
      },
    };
  } finally {
    clearTimeout(timer);
  }
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
    /**
     * 附图（agora-lmz2）：图落进会话工作目录，201 交回绝对路径，由调用方把引用接进下一条 text。
     * 没有这个端点的老节点回 405（SPA 兜底只收 GET）——调用方据此说「那台节点版本旧」，不静默丢图。
     */
    uploadImage: (id: string, data: string) => call<{ path: string }>(fetchImpl, "POST", `${enc(id)}/images`, { data }),
    /** New Agent 对话框的创建（§6.4）；201 的响应体就是新会话那一行。 */
    create: (body: CreateSessionBody) =>
      call<{ id: string }>(fetchImpl, "POST", "/api/sessions", body),
    /**
     * 手机「新建」的预设按钮（agora-prdg.4）：**只读**——预设的增删改只在桌面终端的
     * `agora preset`（S1），这里没有写端点（非 GET 由节点回 405）。预设属于承载节点这一份。
     */
    presets: () => call<{ presets: PresetInfo[] }>(fetchImpl, "GET", "/api/presets"),
    /** 采纳未登记的运行时会话（§5.5）；201 的响应体就是新会话那一行。 */
    adopt: (body: AdoptBody) => call<{ id: string }>(fetchImpl, "POST", "/api/sessions/adopt", body),
    /**
     * 最近 `limit` 轮（旧在前，节点夹到 1..=20；A53，agora-2mff）。没有这个端点的老节点 GET 落到 SPA 兜底，
     * 回空 body 的 404——调用方对任何非 ok 都按「没有上文」处理，不报错。
     */
    turns: (id: string, limit: number) =>
      call<{ turns: TurnInfo[]; keep: number }>(fetchImpl, "GET", `${enc(id)}/turns?limit=${limit}`),
    /** 该会话工作目录的改动文件（§6.3 看结果；A41，agora-h1k.5）：只读的 git status，200 + 类型原因而非错误。 */
    changes: (id: string) => call<ChangesInfo>(fetchImpl, "GET", `${enc(id)}/changes`),
  };
}

export type SessionApi = ReturnType<typeof sessionApi>;

/**
 * New Agent 对话框的数据源（§6.4）：三个只读的，加一个写——新建 worktree（agora 只管"生"，
 * A44）。与会话的写端点分开：这里没有确认语义，401 之外的失败只影响下拉框。
 */
export function catalogApi(fetchImpl: FetchLike = apiFetch) {
  // 五个端点都多一个可选的 node（A45，docs/spec/api.md「在 peer 上起会话」）：不给 = 本机；peer 名 →
  // 节点经一跳转发到那台机器上答。查询串里只在给了时才出现，老节点不认识它也照常答本机的。
  const q = (pairs: Record<string, string | undefined>) => {
    const parts = Object.entries(pairs)
      .filter((kv): kv is [string, string] => kv[1] !== undefined && kv[1] !== "")
      .map(([k, v]) => `${k}=${encodeURIComponent(v)}`);
    return parts.length ? `?${parts.join("&")}` : "";
  };
  return {
    projects: (node?: string) =>
      call<{ projects: ProjectInfo[] }>(fetchImpl, "GET", `/api/projects${q({ node })}`),
    worktrees: (path: string, node?: string) =>
      call<{ worktrees: WorktreeInfo[] }>(fetchImpl, "GET", `/api/projects/worktrees${q({ path, node })}`),
    /**
     * `POST /api/projects/worktrees`（MISSION §6.4「只管"生"」；docs/spec/api.md）：201 的响应体就是
     * GET 会列出的那一项。`base` 不传由节点按缺省链定（主 worktree 当前分支 → origin/HEAD → main）；
     * `node` 给了就在那台机器的仓库里建（A45）。
     */
    createWorktree: (path: string, name: string, base?: string, node?: string) =>
      call<WorktreeInfo>(fetchImpl, "POST", "/api/projects/worktrees", {
        path,
        name,
        ...(base ? { base } : {}),
        ...(node ? { node } : {}),
      }),
    agents: (node?: string) => call<{ agents: AgentInfo[] }>(fetchImpl, "GET", `/api/agents${q({ node })}`),
    /**
     * `GET /api/projects/tasks?path=`（MISSION §6.4 从就绪任务起会话；A43）：该仓库 `bd ready` 的
     * 就绪任务。永远 200——没装 bd / 没有 beads 是空列表 + 类型化的 `reason`，不是错误。
     */
    tasks: (path: string, node?: string) =>
      call<{ tasks: ReadyTask[]; reason: ReadyTasksReason | string | null }>(
        fetchImpl,
        "GET",
        `/api/projects/tasks${q({ path, node })}`,
      ),
    system: () => call<{ node: string }>(fetchImpl, "GET", "/api/system"),
  };
}

export type CatalogApi = ReturnType<typeof catalogApi>;
