/**
 * /m 的会话卡（agora-thc.10；MISSION §6.9；docs/spec/ux.md「移动端交互收件箱」第三屏）。
 *
 * 交互语法是即时消息：最近一轮两个气泡（`❯` 你最后一句 / `↳` agent 最后回复）、固定在底部的
 * composer、乐观发送、回复默认折 6 行且**只允许最后一条展开一次**——不做更早消息的翻页（消息流
 * 是 §11 的 Conversation indexing，手机不做，A52）。等待决定时决策原文等宽逐字内联在 composer
 * 上方：照它批准是「respond 不经终端」的信任基础。
 *
 * 动作全部走与桌面相同的节点 API（allow/deny 带 pending_decision.request_id、text 经 PTY、
 * Kill / Restart 先不带 confirmed 发、节点说要杀才弹框——MISSION §8）。
 */
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { agentBadge } from "./agentBadge";
import type { SessionApi } from "./api";
import { isHandleless, seenKey, seenRelevant, statusLine, taskLabel } from "./attention";
import { ConfirmDialog } from "./ConfirmDialog";
import type { SessionRow } from "./events";
import { MarkdownView } from "./MarkdownView";
import { nodeHue } from "./nodeColor";
import { KILL_BODY, RESTART_BODY, restartNoteOf } from "./SessionSettings";
import { rowName, statusSymbol, str } from "./SessionRow";

/** 回复默认露出的行数（docs/spec/ux.md：手机端默认折 6 行）。 */
export const FOLD_LINES = 6;
/** `respond_within_secs` 短于这个值才提示会交回终端（与桌面 RespondPanel 同一个阈值）。 */
const SHORT_HOLD_SECS = 300;

interface Props {
  row: SessionRow;
  api: SessionApi;
  /** unix 秒；"waiting 3m" 的基准。 */
  now: number;
  onBack: () => void;
  /** 「看过」记号（打开卡片 = 看到这一条结果）：写进每设备 localStorage 的集合。 */
  onSeen?: (key: string) => void;
  /** 推送点击进来且这一行可发送（turn_done / idle）：把焦点放进底部 composer（agora-thc.7）。 */
  focusComposer?: boolean;
}

type Pending = { kind: "kill" | "restart" } | null;
type Sent = { text: string; phase: "sending" | "sent" | "failed" };

export function MobileCard({ row, api, now, onBack, onSeen, focusComposer }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [more, setMore] = useState(false);
  const [pending, setPending] = useState<Pending>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [sent, setSent] = useState<Sent | null>(null);
  const composerRef = useRef<HTMLInputElement>(null);

  // 打开卡片就是"看到结果"：手机没有并存的列表可以"离开"，记在打开这一刻才不作废记号。
  const markSeen = useCallback(() => {
    if (seenRelevant(row.status)) onSeen?.(seenKey(row));
  }, [row, onSeen]);
  useEffect(() => {
    markSeen();
  }, [markSeen]);

  // 折叠只活在"这一条回复"上：换行 / 同一行来了新回复都回到折叠（与桌面 RespondPanel 同一条纪律）。
  useEffect(() => {
    setExpanded(false);
  }, [row.id, row.detail]);
  // 换一行：乐观气泡、草稿、错误、更多菜单全清。
  useEffect(() => {
    setSent(null);
    setDraft("");
    setError(null);
    setNote(null);
    setMore(false);
  }, [row.id]);
  // 服务端把这句话收进 prompt（首行相等）之后，乐观气泡让位给真实投影；失败的留着给重试。
  useEffect(() => {
    if (!sent || sent.phase === "failed") return;
    const first = str(row.prompt).split("\n")[0]?.trim();
    if (first && first === sent.text.split("\n")[0]?.trim()) setSent(null);
  }, [row.prompt, sent]);

  const badge = agentBadge(String(row.agent_type ?? ""));
  const waiting = row.status === "waiting";
  const running = row.status === "running" || row.status === "starting";
  const handleless = isHandleless(row);
  const decision = row.pending_decision;
  const canDecide = waiting && row.respond_via === "hook" && row.reason === "permission" && !!decision;
  const within = typeof row.respond_within_secs === "number" ? row.respond_within_secs : null;
  const canCompose = (row.status === "turn_done" || row.status === "idle") && !handleless;
  // 推送点击进来：可发送的行把焦点直接放进 composer（ux.md「行为」；焦点只在打开那一下要，之后别抢）。
  useEffect(() => {
    if (focusComposer && canCompose) composerRef.current?.focus();
  }, [focusComposer, canCompose]);
  const canRestart = !handleless && typeof row.command === "string" && row.command.trim() !== "";
  const detail = str(row.detail);
  const prompt = str(row.prompt).split("\n")[0] ?? "";
  const userText = sent?.text ?? prompt;

  const replyLines = detail.split("\n");
  const folded = !expanded && replyLines.length > FOLD_LINES;
  const replyShown = folded ? replyLines.slice(0, FOLD_LINES).join("\n") : detail;

  async function sendText(text: string) {
    setError(null);
    setSent({ text, phase: "sending" });
    const r = await api.input(row.id, { kind: "text", data: `${text}\n` });
    if (r.ok) {
      setSent({ text, phase: "sent" });
      return;
    }
    setSent({ text, phase: "failed" });
    if (!r.needsConfirmation) setError(r.error.error === "no_runtime" ? "这一行没有可写的运行时" : r.error.message);
  }

  function submit() {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    void sendText(text);
  }

  async function decide(kind: "allow" | "deny") {
    if (!decision) return;
    setBusy(true);
    setError(null);
    const r = await api.input(row.id, { kind: "decision", decision: kind, request_id: decision.request_id });
    setBusy(false);
    if (!r.ok && !r.needsConfirmation) {
      // no_pending_decision：终端已经答了或超时了；状态事件马上会把这一行改掉。
      setError(r.error.error === "no_pending_decision" ? "已在终端回答或已过期" : r.error.message);
    }
  }

  async function run(kind: "kill" | "restart", confirmed: boolean) {
    setBusy(true);
    setError(null);
    setNote(null);
    const r = await (kind === "kill" ? api.kill(row.id, confirmed) : api.restart(row.id, confirmed));
    setBusy(false);
    if (r.ok) {
      setPending(null);
      if (kind === "restart") setNote(restartNoteOf(r.value));
      return;
    }
    if (r.needsConfirmation) {
      setPending({ kind });
      return;
    }
    setPending(null);
    setError(`${r.error.error}: ${r.error.message}`);
  }

  return (
    <section className="mobile-card" data-testid={`mobile-card-${row.id}`} aria-label="会话卡">
      <header className="mobile-card-head">
        <button className="mobile-back" data-testid="mobile-back" aria-label="返回收件箱" onClick={onBack}>
          ←
        </button>
        <span className="mobile-symbol" aria-hidden="true">
          {statusSymbol(row.status)}
        </span>
        <span className="mobile-row-name">{rowName(row)}</span>
        <span className="mobile-agent" style={{ "--hue": badge.hue } as CSSProperties}>
          {badge.glyph} {badge.label}
        </span>
        <span className="mobile-node-chip peer" data-testid="mobile-card-node" data-node={row.node} style={{ "--hue": nodeHue(row.node) } as CSSProperties}>
          @{row.node}
        </span>
        <span className="mobile-status">{statusLine(row, now)}</span>
      </header>
      <p className="mobile-card-task">{taskLabel(row)}</p>

      <div className="mobile-thread">
        {waiting ? (
          <div className="mobile-decision" data-testid="mobile-decision">
            {canDecide && decision ? (
              <>
                {/* 权限请求的 summary 是**一行命令**，批准前看到的必须逐字符等于真实命令（agora-k1s）。 */}
                <p className="mobile-command" data-testid="mobile-decision-text">
                  {decision.summary}
                </p>
                <div className="mobile-decision-actions">
                  <button data-testid="mobile-allow" disabled={busy} onClick={() => void decide("allow")}>
                    Allow
                  </button>
                  <button data-testid="mobile-deny" className="danger" disabled={busy} onClick={() => void decide("deny")}>
                    Deny
                  </button>
                </div>
                {within !== null && within < SHORT_HOLD_SECS && (
                  <p className="mobile-note">{within} 秒内没答会交回终端（挂起期间终端看不到提示）</p>
                )}
              </>
            ) : (
              <>
                {row.reason === "permission" ? (
                  <p className="mobile-command" data-testid="mobile-decision-text">
                    {detail || "等待你"}
                  </p>
                ) : (
                  <MarkdownView className="mobile-question" text={detail || "等待你"} />
                )}
                {/* terminal-only（Grok 权限、AskUserQuestion）不注入键击、不提供打开终端（MISSION §1.2、A52）。 */}
                <p className="mobile-note" data-testid="mobile-needs-desktop">
                  需要到桌面
                </p>
              </>
            )}
          </div>
        ) : (
          <>
            {userText !== "" && (
              <div className="mobile-bubble user" data-testid="mobile-bubble-user">
                <span className="mobile-bubble-mark" aria-hidden="true">
                  ❯
                </span>
                <span>{userText}</span>
                {sent && sent.phase !== "failed" && (
                  <span className="mobile-sent-state" data-testid="mobile-sent-state">
                    {sent.phase === "sending" ? "发送中…" : "已发送"}
                  </span>
                )}
                {sent?.phase === "failed" && (
                  <button className="mobile-retry" data-testid="mobile-retry" onClick={() => void sendText(sent.text)}>
                    重试
                  </button>
                )}
              </div>
            )}
            {detail !== "" && (
              <div className="mobile-bubble agent" data-testid="mobile-bubble-agent">
                <span className="mobile-bubble-mark" aria-hidden="true">
                  ↳
                </span>
                <div className="mobile-bubble-body">
                  <MarkdownView text={replyShown} />
                  {folded && (
                    <button className="mobile-more-line" data-testid="mobile-expand" onClick={() => setExpanded(true)}>
                      展开这一条（{replyLines.length} 行）
                    </button>
                  )}
                </div>
              </div>
            )}
            {/* 空 thread（登记即空闲的 pi 行就是这样：没有 ❯ 也没有 ↳）：卡片不能只剩个壳，
                无句柄行还得说清手机发不了（agora-sd0b）。在跑的行走下面那条 running note。 */}
            {userText === "" && detail === "" && !running && (
              <p className="mobile-note" data-testid="mobile-empty-thread">
                {handleless
                  ? "这一行没有挂起，也没有最近一轮回复；回复要到桌面终端。"
                  : "还没有可显示的内容；回一条就会出现在这里。"}
              </p>
            )}
          </>
        )}
      </div>

      {note && <p className="mobile-note" data-testid="mobile-card-note">{note}</p>}
      {error && <p className="mobile-error" data-testid="mobile-card-error">{error}</p>}

      {!handleless && (
        <div className="mobile-actions">
          <button className="mobile-more-toggle" data-testid="mobile-more" aria-expanded={more} onClick={() => setMore((v) => !v)}>
            更多 {more ? "▾" : "▸"}
          </button>
          {more && (
            <div className="mobile-more-actions">
              {canRestart && (
                <button data-testid="mobile-restart" disabled={busy} onClick={() => void run("restart", false)}>
                  Restart
                </button>
              )}
              <button data-testid="mobile-kill" className="danger" disabled={busy} onClick={() => void run("kill", false)}>
                Kill
              </button>
            </div>
          )}
        </div>
      )}

      {canCompose && (
        <form
          className="mobile-composer"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <input
            ref={composerRef}
            value={draft}
            placeholder="下一条指令"
            aria-label="下一条指令"
            data-testid="mobile-next-input"
            disabled={busy}
            onChange={(e) => setDraft(e.target.value)}
          />
          <button type="submit" data-testid="mobile-send" disabled={!draft.trim() || sent?.phase === "sending"}>
            发送
          </button>
        </form>
      )}
      {running && (
        <p className="mobile-note" data-testid="mobile-running-note">
          它还在跑，等它停下来或回完这一轮再发。
        </p>
      )}
      {pending && (
        <ConfirmDialog
          title={pending.kind === "kill" ? "Kill 这个会话？" : "Restart 这个会话？"}
          body={pending.kind === "kill" ? KILL_BODY : RESTART_BODY}
          confirmLabel={pending.kind === "kill" ? "Kill" : "Restart"}
          onCancel={() => setPending(null)}
          onConfirm={() => void run(pending.kind, true)}
        />
      )}
    </section>
  );
}
