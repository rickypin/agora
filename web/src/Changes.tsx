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

/**
 * 这一行现在有没有改动列表可看。RowResult 用它判断「看结果面板要不要占位」——面板与组件
 * 自己必须用同一个判据，不然会出现一个空面板画着边线、里面什么都没有（与 RespondPanel 的
 * hasRespondPanel 同一个形状）。api 为 null（没有 Provider 也没传）时组件什么都不做，也算不显示。
 */
export function showsChanges(row: SessionRow, api: SessionApi | null): boolean {
  return api !== null && SHOW_FOR.has(row.status);
}

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
  const show = showsChanges(row, api);
  const [state, setState] = useState<Loaded>({ kind: "loading" });
  // 折叠（agora-4yr.3，与验收标准同一副折叠按钮）：默认展开——看结果就是为了看"改了什么"。
  const [open, setOpen] = useState(true);
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
  // 折叠只藏 body、绝不把整个 Changes 从 DOM 里摘掉：拉取挂在 mount 的 useEffect（见文件头），
  // 组件一卸载 state 就没了，"收起来再打开"会白白重发一次 GET /changes（2026-09-10 设计 agora-4yr.3
  // 时先想把折叠写在外层做条件渲染，正是这个形状；同一条理由让 diff 视图保留 result-panel）。
  // 「看 diff」按钮留在头上、不随折叠消失：收起列表是嫌它长，不是不想看 diff。
  return (
    <div className="changes" data-testid={`changes-${row.id}`} onClick={(e) => e.stopPropagation()}>
      <div className="changes-head">
        <button
          type="button"
          className="changes-toggle muted"
          aria-expanded={open}
          data-testid={`changes-toggle-${row.id}`}
          onClick={() => setOpen((v) => !v)}
        >
          {open ? "▾" : "▸"} 改动{data?.branch ? ` · ${data.branch}` : ""}
        </button>
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
      {open && state.kind === "loading" && <p className="muted changes-note">…</p>}
      {open && state.kind === "error" && (
        <p className="muted changes-note" data-testid={`changes-error-${row.id}`}>
          拉不到改动列表（{state.error}）
        </p>
      )}
      {open && data && data.reason !== null && (
        <p className="muted changes-note" data-testid={`changes-reason-${row.id}`} data-reason={data.reason}>
          {reasonText(data.reason)}
        </p>
      )}
      {open && data && data.reason === null && data.files.length === 0 && (
        <p className="muted changes-note" data-testid={`changes-empty-${row.id}`}>
          无改动
        </p>
      )}
      {open && data && data.files.length > 0 && (
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
