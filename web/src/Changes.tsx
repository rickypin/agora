import { createContext, useContext, useEffect, useState } from "react";
import type { ChangesInfo, SessionApi } from "./api";
import type { SessionRow } from "./events";

/**
 * 展开区里的改动文件列表 + 「看 diff」（MISSION §6.3「看结果」；A41；agora-h1k.5）。位置在验收标准之后
 * （docs/spec/ux.md）：对照"做完算什么"看"改了什么"。数据是 `GET /api/sessions/:id/changes`——该会话
 * 工作目录的 `git status`，只读；「看 diff」只是开一个 `diff:<id>` 标签页，里面是 `WS /diff` 的只读终端。
 *
 * 什么时候拉：行展开且 status 是 TURN_DONE / FINISHED / FAILED（做完了、看结果）或 RUNNING（想瞄一眼
 * 进度也行）——每次 status 变化拉一次，**不轮询**：跑着的 agent 每秒都在改文件，轮询只会让列表抖。
 * 其它状态（WAITING / IDLE / STARTING / UNKNOWN）不占位：此刻该做的是回答问题，不是看结果。
 *
 * api 从 Context 拿：SessionRow 是 memo 的展示组件，Workspace 把自己的 `api` 放进 Provider，行组件不用
 * 为一个网络对象多一层 prop；组件单测直接传 `api`。两者都没有（SessionRow 自己的单测那样裸渲染）就什么
 * 也不做、不占位——绝不自己 `sessionApi()` 去真发 fetch：jsdom 里相对 URL 的 fetch 会以 unhandled
 * rejection 把整个测试文件标红（2026-09-06 实测），而且展示组件默默发网络请求本来就不对。
 */

export const ChangesApiContext = createContext<SessionApi | null>(null);

/** 这几种状态才拉列表。 */
const SHOW_FOR = new Set(["turn_done", "finished", "failed", "running"]);

/** 列表里的单字母（git status --short 的习惯）。不认识的状态原样给首字母，绝不空着。 */
export function statusLetter(status: string): string {
  switch (status) {
    case "modified":
      return "M";
    case "added":
      return "A";
    case "deleted":
      return "D";
    case "renamed":
      return "R";
    case "copied":
      return "C";
    case "typechange":
      return "T";
    case "unmerged":
      return "U";
    case "untracked":
      return "?";
    default:
      return status.slice(0, 1).toUpperCase() || "?";
  }
}

/** `reason` 按类型给文案（docs/spec/api.md「只读产出」），不解析任何消息文本（MISSION §2.3 规则 10）。 */
export function reasonText(reason: string): string {
  switch (reason) {
    case "not_a_repo":
      return "不是 git 仓库";
    case "no_directory":
      return "工作目录不存在";
    case "no_git":
      return "本机没有 git";
    case "timeout":
      return "git status 超时";
    case "git":
      return "git 失败";
    default:
      return reason;
  }
}

interface Props {
  row: SessionRow;
  /** 「看 diff」：开该会话的只读 diff 标签页。 */
  onOpenDiff?: (id: string) => void;
  /** 测试注入；缺省取 Context；两者都没有则不渲染。 */
  api?: SessionApi;
}

type Loaded = { kind: "loading" } | { kind: "data"; data: ChangesInfo } | { kind: "error"; error: string };

export function Changes({ row, onOpenDiff, api: given }: Props) {
  const ctx = useContext(ChangesApiContext);
  const api = given ?? ctx;
  const show = api !== null && SHOW_FOR.has(row.status);
  const [state, setState] = useState<Loaded>({ kind: "loading" });
  useEffect(() => {
    if (!show || !api) return;
    let alive = true;
    setState({ kind: "loading" });
    void api.changes(row.id).then((r) => {
      if (!alive) return;
      if (r.ok) {
        const v = r.value;
        // 老节点 / 假接口没有这个端点时 value 不成形：当空列表，不炸。
        setState({
          kind: "data",
          data: {
            files: Array.isArray(v?.files) ? v.files : [],
            branch: typeof v?.branch === "string" ? v.branch : null,
            reason: typeof v?.reason === "string" ? v.reason : null,
          },
        });
      } else if (!r.needsConfirmation) {
        setState({ kind: "error", error: r.error.error });
      }
    });
    return () => {
      alive = false;
    };
    // 只在会话 / 状态变化时拉（见文件头）；api 是稳定引用。
  }, [api, row.id, row.status, show]);
  if (!show || !api) return null;
  const data = state.kind === "data" ? state.data : null;
  const canDiff = data !== null && data.reason === null;
  return (
    <div className="changes" data-testid={`changes-${row.id}`} onClick={(e) => e.stopPropagation()}>
      <div className="changes-head">
        <span className="muted">
          改动{data?.branch ? ` · ${data.branch}` : ""}
        </span>
        <button
          type="button"
          data-testid={`diff-${row.id}`}
          disabled={!canDiff}
          title={canDiff ? "在该 worktree 开只读终端跑 git diff" : "没有可 diff 的仓库"}
          onClick={() => onOpenDiff?.(row.id)}
        >
          看 diff
        </button>
      </div>
      {state.kind === "loading" && <p className="muted changes-note">…</p>}
      {state.kind === "error" && (
        <p className="muted changes-note" data-testid={`changes-error-${row.id}`}>
          拉不到改动列表（{state.error}）
        </p>
      )}
      {data && data.reason !== null && (
        <p className="muted changes-note" data-testid={`changes-reason-${row.id}`} data-reason={data.reason}>
          {reasonText(data.reason)}
        </p>
      )}
      {data && data.reason === null && data.files.length === 0 && (
        <p className="muted changes-note" data-testid={`changes-empty-${row.id}`}>
          无改动
        </p>
      )}
      {data && data.files.length > 0 && (
        <ul className="changes-list" data-testid={`changes-list-${row.id}`}>
          {data.files.map((f) => (
            <li key={f.path} title={`${f.status}: ${f.path}`}>
              <span className={`chg chg-${f.status}`}>{statusLetter(f.status)}</span> {f.path}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
