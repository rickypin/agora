import { useEffect, useRef, useState } from "react";
import type { SessionApi } from "./api";
import type { SessionRow } from "./events";

interface Props {
  row: SessionRow;
  api: SessionApi;
  /**
   * 「打开终端」与输入框里的 Escape：把焦点交给下面的终端。面板搬进主区之后（agora-4yr.1）终端
   * 一直就在面板下方，这个回调不再是"切到那个标签页"，而是"焦点归 pane"；参数仍是会话 id，
   * Workspace 传的 focusTerminal 不看它。
   */
  onOpenTerminal: (id: string) => void;
  /**
   * true = 现在把焦点放进面板（Alt/Option+R 与通知点击两条路径，MISSION §6.5 / §6.6）。
   * 用"这一行的请求在不在"而不是一个裸计数：通知点击是「选中这一行」+「聚焦它的面板」同一批
   * state 更新，面板在那一帧才第一次挂载——挂载时看裸计数分不出"刚被请求"和"上一次请求留下的
   * 旧值"，后者会让之后每次切行都抢一次焦点（2026-09-10 设计时先写成计数，正是这个形状）。
   */
  focusRequest?: boolean;
  /** 聚焦做完了：请求只用一次，由 Workspace 清掉（稳定引用，否则每次渲染都会重新聚焦）。 */
  onFocusHandled?: () => void;
}

/**
 * 这一行有没有可回答的东西。Workspace 的 Alt/Option+R 用它判断"面板存在吗"——面板自己对
 * 别的状态返回 null，两处判据必须是同一个函数，不然键位会对着不存在的面板发聚焦请求。
 */
export function hasRespondPanel(row: SessionRow): boolean {
  return row.status === "waiting" || row.status === "turn_done";
}

/**
 * 回答面板（MISSION §6.3 §7.3；ADR-002 D5）：主区 crumb 与终端之间的那一段（A50，agora-4yr.1，
 * 兑现 agora-03k）——WAITING 显示问题与 allow / deny / 打开终端；TURN_DONE 是"下一条指令"输入框，
 * 最后一条回复排在输入框**下面**（用户明确要求：给下一条指令是先做的事，回顾全文是后做的事；
 * 全文限高 40vh、内部滚动，见 index.css 的 .respond-last）。
 *
 * 2026-09-08 之前它长在侧栏选中行的 <li> 里（Respond.tsx），260 px 的窄列装几百行回复不可读；
 * 搬进主区只改布局，回答的语义一个字没动。
 *
 * 三种 WAITING：权限且 agent 的 hook 能替用户批准（`respond_via = hook`，reason `permission`）
 * → allow / deny 经挂起的 hook 返回，不注入键击；权限但 hook 不能批准（Grok）→ 只有"打开终端"；
 * 提问（AskUserQuestion 类，reason `question`）→ 选项渲染在 TUI 里，只显示问题文本与"打开终端"。
 * `respond_within_secs` 是宿主的挂起上限：Codex 挂起期间终端答不了、上限只有几十秒，超时提示
 * 交回终端——短于 5 分钟就把它写出来，免得人以为 Allow 按钮坏了。
 */
const SHORT_HOLD_SECS = 300;
export function RespondPanel({ row, api, onOpenTerminal, focusRequest, onFocusHandled }: Props) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rootRef = useRef<HTMLElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    setError(null);
  }, [row.id, row.status, row.reason, row.pending_decision?.request_id]);
  useEffect(() => {
    if (!focusRequest) return;
    // TURN_DONE 落在输入框，WAITING 落在第一个按钮（Allow，或只有"打开终端"时就是它）。
    (inputRef.current ?? rootRef.current?.querySelector("button"))?.focus();
    onFocusHandled?.();
  }, [focusRequest, onFocusHandled]);

  const pending = row.pending_decision;
  const detail = typeof row.detail === "string" && row.detail ? row.detail : null;
  const waiting = row.status === "waiting";
  const turnDone = row.status === "turn_done";
  if (!waiting && !turnDone) return null;
  const canDecide = waiting && row.reason === "permission" && row.respond_via === "hook" && !!pending;
  const within = typeof row.respond_within_secs === "number" ? row.respond_within_secs : null;
  const shortHold = canDecide && within !== null && within < SHORT_HOLD_SECS;
  // external 会话没有运行时句柄（MISSION §5.5）：主区那一格是"没有终端"的说明文字，
  // 把焦点交给它没有意义，按钮直接不画。它仍然能经 hook 回答（Allow / Deny 照旧）。
  const hasTerminal = row.origin !== "external";

  async function decide(decision: "allow" | "deny") {
    if (!pending) return;
    setBusy(true);
    setError(null);
    const r = await api.input(row.id, { kind: "decision", decision, request_id: pending.request_id });
    setBusy(false);
    if (!r.ok && !r.needsConfirmation) {
      // no_pending_decision：终端已经答了或超时了；状态事件马上会把行改掉。
      setError(r.error.error === "no_pending_decision" ? "已在终端回答或已过期" : r.error.message);
    }
  }

  async function send() {
    const data = text.trim();
    if (!data) return;
    setBusy(true);
    setError(null);
    const r = await api.input(row.id, { kind: "text", data: `${data}\n` });
    setBusy(false);
    if (r.ok) setText("");
    else if (!r.needsConfirmation) setError(r.error.message);
  }

  return (
    <section className="respond-panel" data-testid={`respond-panel-${row.id}`} ref={rootRef}>
      {waiting && <p className="respond-question">{canDecide ? pending.summary : detail ?? String(row.reason ?? "等待你")}</p>}
      {shortHold && (
        <p className="respond-hint muted" data-testid="respond-within">
          {within} 秒内没答会交回终端（挂起期间终端看不到提示）
        </p>
      )}
      {waiting && (canDecide || hasTerminal) && (
        <div className="respond-actions">
          {canDecide && (
            <>
              <button disabled={busy} data-testid="allow" onClick={() => void decide("allow")}>
                Allow
              </button>
              <button disabled={busy} data-testid="deny" className="danger" onClick={() => void decide("deny")}>
                Deny
              </button>
            </>
          )}
          {hasTerminal && (
            <button data-testid="open-terminal" onClick={() => onOpenTerminal(row.id)}>
              打开终端
            </button>
          )}
        </div>
      )}
      {turnDone && (
        <>
          <form
            className="respond-next"
            onSubmit={(e) => {
              e.preventDefault();
              void send();
            }}
          >
            <input
              ref={inputRef}
              value={text}
              placeholder="下一条指令"
              aria-label="下一条指令"
              data-testid="next-input"
              disabled={busy}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                // Escape = 我不打字了，键盘还给终端（终端焦点规则 agora-p29 / agora-vcc 不变：
                // 点行仍然聚焦终端，面板只在 Alt/Option+R 与通知点击两条路径上抢焦点）。
                if (e.key !== "Escape") return;
                e.preventDefault();
                onOpenTerminal(row.id);
              }}
            />
            <button type="submit" disabled={busy || !text.trim()} data-testid="next-send">
              发送
            </button>
          </form>
          {detail && (
            <div className="respond-last muted" data-testid="respond-last">
              ↳ {detail}
            </div>
          )}
        </>
      )}
      {error && <p className="respond-error">{error}</p>}
    </section>
  );
}
