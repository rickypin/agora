import { useEffect, useState } from "react";
import type { SessionApi } from "./api";
import type { SessionRow } from "./events";

interface Props {
  row: SessionRow;
  api: SessionApi;
  /** "打开终端"：切到该会话的终端 Tab。 */
  onOpenTerminal: (id: string) => void;
}

/**
 * 就地 respond（MISSION §6.3 §7.3；ADR-002 D5）：WAITING 行展开显示问题与 allow / deny /
 * 打开终端；TURN_DONE 行展开是"下一条指令"输入框。
 *
 * 三种 WAITING：权限且 agent 的 hook 能替用户批准（`respond_via = hook`，reason `permission`）
 * → allow / deny 经挂起的 hook 返回，不注入键击；权限但 hook 不能批准（Grok）→ 只有"打开终端"；
 * 提问（AskUserQuestion 类，reason `question`）→ 选项渲染在 TUI 里，只显示问题文本与"打开终端"。
 * `respond_within_secs` 是宿主的挂起上限：Codex 挂起期间终端答不了、上限只有几十秒，超时提示
 * 交回终端——短于 5 分钟就把它写出来，免得人以为 Allow 按钮坏了。
 */
const SHORT_HOLD_SECS = 300;
export function Respond({ row, api, onOpenTerminal }: Props) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setError(null);
  }, [row.id, row.status, row.reason, row.pending_decision?.request_id]);

  const pending = row.pending_decision;
  const detail = typeof row.detail === "string" && row.detail ? row.detail : null;
  const waiting = row.status === "waiting";
  const turnDone = row.status === "turn_done";
  if (!waiting && !turnDone) return null;
  const canDecide = waiting && row.reason === "permission" && row.respond_via === "hook" && !!pending;
  const within = typeof row.respond_within_secs === "number" ? row.respond_within_secs : null;
  const shortHold = canDecide && within !== null && within < SHORT_HOLD_SECS;

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
    <div className="respond" data-testid={`respond-${row.id}`} onClick={(e) => e.stopPropagation()}>
      {waiting && <p className="respond-question">{canDecide ? pending.summary : detail ?? String(row.reason ?? "等待你")}</p>}
      {shortHold && (
        <p className="respond-hint muted" data-testid="respond-within">
          {within} 秒内没答会交回终端（挂起期间终端看不到提示）
        </p>
      )}
      {waiting && (
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
          <button data-testid="open-terminal" onClick={() => onOpenTerminal(row.id)}>
            打开终端
          </button>
        </div>
      )}
      {turnDone && (
        <>
          {detail && <p className="respond-last muted">↳ {detail}</p>}
          <form
            className="respond-next"
            onSubmit={(e) => {
              e.preventDefault();
              void send();
            }}
          >
            <input
              value={text}
              placeholder="下一条指令"
              aria-label="下一条指令"
              data-testid="next-input"
              disabled={busy}
              onChange={(e) => setText(e.target.value)}
            />
            <button type="submit" disabled={busy || !text.trim()} data-testid="next-send">
              发送
            </button>
          </form>
        </>
      )}
      {error && <p className="respond-error">{error}</p>}
    </div>
  );
}
