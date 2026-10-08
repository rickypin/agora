/**
 * /m 的会话卡（agora-thc.10；MISSION §6.9；docs/spec/ux.md「移动端交互收件箱」第三屏）。
 *
 * 交互语法是即时消息：最近一轮两个气泡（`❯` 你最后一句 / `↳` agent 最后回复）、固定在底部的
 * composer、乐观发送、回复默认折 6 行且**只允许最后一条展开一次**——不做更早消息的翻页（消息流
 * 是 §11 的 Conversation indexing，手机不做，A52）。等待决定时决策原文等宽逐字内联在 composer
 * 上方：照它批准是「respond 不经终端」的信任基础。
 *
 * 动作全部走与桌面相同的节点 API（allow/deny 带 pending_decision.request_id、text 经 PTY、
 * Kill / Restart 先不带 confirmed 发、节点说要杀才弹框——MISSION §8）。发送失败时草稿回填回
 * 输入框、原因按错误码说人话、重试留在失败气泡上（agora-jidm；文案表见 [`sendFailureText`]）。
 * 只读行（采纳 socket / 死 pane）不给 composer 与「更多」：可写性看 `writableRuntime`
 * （agora-prdg.2）。
 */
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { agentBadge } from "./agentBadge";
import type { ApiErrorBody, SessionApi } from "./api";
import { isHandleless, seenKey, seenRelevant, taskLabel } from "./attention";
import { textVia } from "./events";
import { mobileStatusLine } from "./mobileStatus";
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
  /** 本设备看过这一行了吗（`seen.has(seenKey(row))`，MobileApp 算好传进来）：头里的「回完了 / 已看过」。 */
  seen?: boolean;
  /** 推送点击进来且这一行可发送（turn_done / idle）：把焦点放进底部 composer（agora-thc.7）。 */
  focusComposer?: boolean;
  initialDraft?: string;
  onDraftChange?: (text: string) => void;
  onNext?: () => void;
}

type Pending = { kind: "kill" | "restart" } | null;
type Sent = { text: string; phase: "sending" | "sent" | "failed"; queued?: boolean; failure?: string };

/**
 * 发送失败给手机看的一句话（agora-jidm）：按错误码分开说，不把内部话直接扔给用户。
 *
 * - `no_runtime` / `runtime_session_not_found` 是**两种不同的处境**，文案也要分开：前者是这一行
 *   根本没有可写通道（与 `mobile-terminal-only` 同一句人话），后者是 `text_via = runtime` 但 pane
 *   已经不在（降级 / 陈旧行）——"到桌面看它"与"只能看"是两回事（agora-jidm 的审查注记）。
 * - `read_only` 是采纳行（agora-prdg.2 修 text_via 之前 / 混版本时的残留）：这一行只能看。
 * - `host_timeout`（504）与其余未知码：把服务端 / 宿主那句话原样显示，**不加壳**——
 *   504 的"宿主没来取这条输入…去它的终端里看看"与 502 `host_rejected` 的 `.failed` 原话
 *   （例如 `pi.sendUserMessage 失败：…`）都比转述准确。出口是失败气泡上的重试：504 时队列里
 *   那件已被 daemon 删掉，重试不会跑两遍（src/api/sessions.rs 的超时路径）。
 */
export function sendFailureText(error: ApiErrorBody): string {
  switch (error.error) {
    case "no_runtime":
      return "这一行没有可写的运行时；回复要到桌面终端。";
    case "runtime_session_not_found":
      return "这个会话的终端已经不在了；到桌面看它。";
    case "read_only":
      return "这一行只能看不能写。";
    default:
      return error.message;
  }
}

export function MobileCard({ row, api, now, onBack, onSeen, seen = false, focusComposer, initialDraft = "", onDraftChange, onNext }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [more, setMore] = useState(false);
  const [pending, setPending] = useState<Pending>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [draft, setDraft] = useState(initialDraft);
  useEffect(() => { onDraftChange?.(draft); }, [draft, onDraftChange]);
  const [sent, setSent] = useState<Sent | null>(null);
  // 「等它接手…」20 s 还没动静的兜底（agora-o975.2 审查修订，2026-10-08）：ack 回来了、
  // row.prompt 没回显、状态也没离开 turn_done/idle——再等下去没有新信息，补一句出口话，
  // 别让一句进度提示永远挂着。
  const [takeoverStalled, setTakeoverStalled] = useState(false);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  // 卡片内的心跳（agora-o975.3）：这一行在跑时每秒走一格，刚发出去的几十秒看得见在动；收件箱
  // 列表保持 30 s 一格（别让整屏每秒重排）。心跳只在本地加秒，父级的 now（服务端锚定的节点钟）
  // 一到就对齐——两边都与同一条基准走。发送在途也算「在动」（agora-o975.2）。
  const [nowSeconds, setNowSeconds] = useState(now);
  useEffect(() => setNowSeconds(now), [now]);

  /**
   * iOS 键盘弹起时只缩**视觉**视口（Safari 不认 interactive-widget），composer 在粘性底部，
   * 不主动滚一下就可能被键盘盖住。等 300ms 让键盘动画走起来再滚，block:'nearest' 不会把已经
   * 在可见区的元素又推来推去（agora-x70t.ten0；2026-10-08 iPhone 16 Pro 实测：聚焦后 composer
   * 仍在屏内，但软键盘高度一变就不一定——这条是保险，不是主修复；主修复是字号 ≥16px 不放大）。
   * jsdom 里 scrollIntoView 可能不在（可选调用，测试造一个假的也能断言）。
   */
  const keepAboveKeyboard = (el: HTMLTextAreaElement) => {
    window.setTimeout(() => el.scrollIntoView?.({ block: "nearest" }), 300);
  };

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
  // 换一行：清掉临时反馈，恢复这条会话的未发草稿。
  useEffect(() => {
    setSent(null);
    setDraft(initialDraft);
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
  // 无句柄、但宿主自己收文本的（pi 的扩展，ADR-002 D11）：手机上照旧能给下一条；
  // 真正的只读是 `textVia == "none"` 那些（MISSION §5.5）。
  const hostText = textVia(row) === "host";
  // 卡片里这一行「在动」吗：running / starting，或发送在途（那一段也在跑心跳）。
  const live = running || sent?.phase === "sending";
  useEffect(() => {
    if (!live) return;
    const timer = setInterval(() => setNowSeconds((v) => v + 1), 1000);
    return () => clearInterval(timer);
  }, [live]);
  // 可写的运行时（agora-prdg.2）：服务端只把"活着的、由 agora 管的运行时"报成 `runtime`
  // ——采纳行（只读 socket，写操作 409 read_only）与死 pane 的托管行都是 `none`、没有可写的
  // PTY，两种都不能给 composer / Kill。`row.managed === false` 是否决旧 peer 的谎报：修前的
  // 服务端把采纳行报成 runtime，而 wire 上的 managed 一直是对的；缺键（老节点 / 测试桩）不否决。
  const writableRuntime = !handleless && textVia(row) === "runtime" && row.managed !== false;
  const decision = row.pending_decision;
  const canDecide = waiting && row.respond_via === "hook" && row.reason === "permission" && !!decision;
  const within = typeof row.respond_within_secs === "number" ? row.respond_within_secs : null;
  const idleOrDone = row.status === "turn_done" || row.status === "idle";
  // 排队门（agora-shze）：host 通道在跑也开放——扩展用 `pi.sendUserMessage(text, { deliverAs:
  // "followUp" })` 收下这句话，pi 在**当前这一轮跑完（不再有工具调用）之后**自动交进去，
  // 不 settle 直接继续处理（ADR-002 D11）。**不是**"做完当前工具批次就插进去"——那是 pi 的
  // `steer` 语义（`dist/core/agent-session.d.ts`：steer = after the current assistant turn
  // finishes its tool calls；followUp = delivered only when agent has no more tool calls）。
  // 反例（2026-10-08，pi 1.0.4 实测，隔离 daemon + 真 TUI）：一轮里两个工具批次（sleep 60 →
  // echo），运行中注入的 followUp 在第一个工具结果之后 60 s 都没出现，而是在本轮最终回复
  // （assistant "OK"，09:28:54.438）之后 2 ms 才进 transcript（09:28:54.440）；hook 侧只有
  // 末尾一条 agent_settled（last=QUEUED-OK）、**没有**第二条 before_agent_start。所以文案说
  // "等它跑完这一轮"，说"这一步"就是 steer 的时机、不诚实。runtime（PTY）没有队列，running
  // 仍禁用（键击直接落进 TUI 的输入区，各家解释不同）；waiting 也沿用状态门（pi 没有权限问句，
  // 但别的宿主将来会有，不给排队）。
  const queueWhileRunning = hostText && running;
  const canCompose = (idleOrDone && (writableRuntime || hostText)) || queueWhileRunning;
  // 推送点击进来：可发送的行把焦点直接放进 composer（ux.md「行为」；焦点只在打开那一下要，之后别抢）。
  useEffect(() => {
    if (focusComposer && canCompose) composerRef.current?.focus();
  }, [focusComposer, canCompose]);
  const canRestart = writableRuntime && typeof row.command === "string" && row.command.trim() !== "";
  const detail = str(row.detail);
  const prompt = str(row.prompt).split("\n")[0] ?? "";
  const userText = sent?.text ?? prompt;

  // 发送三段（agora-o975.2，2026-10-08 真机反馈第 2 条）：宿主 ack 最长等 10 s，ack 回来时 hook
  // 往往还没到（行状态仍停在 turn_done / idle）——「已发出，等它接手…」要撑到 **row.prompt 首行
  // 回显**（服务端把这句话收下了）或 **row.status 离开 turn_done / idle**（它真的动起来了）为止。
  // 已在跑的行不发这条：它走「已排队」+ 原有的排队文案（等它跑完这一轮就交进去）。
  const promptEchoed = (() => {
    if (!sent || sent.phase !== "sent") return false;
    const first = str(row.prompt).split("\n")[0]?.trim();
    return first !== "" && first === sent.text.split("\n")[0]?.trim();
  })();
  const awaitingTakeover = sent?.phase === "sent" && idleOrDone && !promptEchoed;
  // 20 s 兜底用墙上钟的 setTimeout（这一段行已经不在跑，没有 1 s 心跳可借）：提示一撤
  // （prompt 回显 / 状态离开 turn_done/idle / 新一次发送）就清掉，下一次发送重新计时。
  useEffect(() => {
    if (!awaitingTakeover) {
      setTakeoverStalled(false);
      return;
    }
    const timer = setTimeout(() => setTakeoverStalled(true), 20_000);
    return () => clearTimeout(timer);
  }, [awaitingTakeover]);

  const replyLines = detail.split("\n");
  const folded = !expanded && replyLines.length > FOLD_LINES;
  const replyShown = folded ? replyLines.slice(0, FOLD_LINES).join("\n") : detail;

  async function sendText(text: string) {
    if (row.stale) return;
    setError(null);
    // 「排队 / 没排队」按**发出去的那一刻**这一行是否在跑：ack 回来时 hook 可能已经把它改成别的
    // 状态，标签要跟着这一次发送走，不回头看。
    const queued = hostText && running;
    setSent({ text, phase: "sending" });
    const r = await api.input(row.id, { kind: "text", data: `${text}\n` });
    if (r.ok) {
      setSent({ text, phase: "sent", queued });
      return;
    }
    setSent({ text, phase: "failed", failure: r.needsConfirmation ? undefined : sendFailureText(r.error) });
    if (!r.needsConfirmation) {
      // 草稿回填（agora-jidm ①）：失败后想改一个字不用重打整句。只在输入框为空时放回去，
      // 不覆盖请求在途时用户已经打上的新字；失败气泡里的重试仍拿着原来那份文本。
      setDraft((d) => (d.trim() === "" ? text : d));
    }
  }

  function submit() {
    const text = draft.trim();
    if (!text || row.stale) return;
    // 在途防重（agora-o975.2 审查修订，2026-10-08）：输入框在途仍可编辑（能接着打下一句），
    // 所以 Enter 提交还会进门——这里挡住第二次；挡的时候草稿留在框里（等 ack 回来还能发）。
    if (sent?.phase === "sending") return;
    setDraft("");
    void sendText(text);
  }

  async function decide(kind: "allow" | "deny") {
    if (!decision || row.stale) return;
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
    if (row.stale) return;
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
        <div className="mobile-card-heading">
          <div className="mobile-title-row">
            <span className={`mobile-symbol${running ? " live" : ""}`} aria-hidden="true">
              {statusSymbol(row.status)}
            </span>
            <span className="mobile-row-name">{rowName(row)}</span>
          </div>
          <div className="mobile-card-meta">
            <span className="mobile-agent" style={{ "--hue": badge.hue } as CSSProperties}>
              <span className="mobile-agent-glyph" aria-hidden="true">{badge.label.slice(0, 1).toUpperCase()}</span> {badge.label}
            </span>
            <span className="mobile-node-chip peer" data-testid="mobile-card-node" data-node={row.node} style={{ "--hue": nodeHue(row.node) } as CSSProperties}>
              @{row.node}
            </span>
            <span className="mobile-status">{mobileStatusLine(row, seen, nowSeconds)}</span>
          </div>
        </div>
      </header>
      <div className="mobile-detail-intro"><span className="mobile-eyebrow">{waiting ? "需要你的决定" : running ? "工作进行中" : "最近一轮"}</span><p className="mobile-card-task">{taskLabel(row) || rowName(row)}</p></div>

      {row.stale && <p className="mobile-note warning" role="status">节点离线，当前是上次收到的内容。重新连接后即可操作。</p>}
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
                  <button data-testid="mobile-allow" disabled={busy || row.stale === true} onClick={() => void decide("allow")}>
                    允许
                  </button>
                  <button data-testid="mobile-deny" className="danger" disabled={busy || row.stale === true} onClick={() => void decide("deny")}>
                    拒绝
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
                  你
                </span>
                <span>{userText}</span>
                {sent && sent.phase !== "failed" && (
                  <span className="mobile-sent-state" data-testid="mobile-sent-state">
                    {sent.phase === "sending" ? "发送中…" : sent.queued ? "已排队" : "已发出"}
                  </span>
                )}
                {sent?.phase === "failed" && (
                  <button className="mobile-retry" data-testid="mobile-retry" disabled={row.stale === true} onClick={() => void sendText(sent.text)}>
                    重试
                  </button>
                )}
              </div>
            )}
            {awaitingTakeover && (
              // 过渡提示（aria-live，agora-o975.2）：动态点是 CSS 脉冲（reduced-motion 下关掉）；
              // 它只说「已交给它」，它真的动起来由状态词与符号脉冲接棒。
              <p className="mobile-note mobile-await-takeover" data-testid="mobile-await-takeover" aria-live="polite">
                <span className="mobile-pending-dot" aria-hidden="true">
                  ●
                </span>
                已发出，等它接手…
              </p>
            )}
            {awaitingTakeover && takeoverStalled && (
              // 20 s 兜底（审查修订 ③）：不是错误（它可能还在忙），只是一句出口——手机上看不到
              // 动静时，桌面是唯一能看到它到底怎么了的地方。
              <p className="mobile-note mobile-await-stalled" data-testid="mobile-await-stalled">
                它还没接手——一直没动静就去桌面看看
              </p>
            )}
            {sent?.phase === "failed" && sent.failure && (
              // 失败的原因贴着失败气泡放（不在卡片底部）：重试按钮就在这一团里，为什么失败
              // 与"再试一次"是同一个决定的两半（agora-jidm）。.mobile-error 的红色与可换行
              // 沿用卡 ERROR 槽的旧语义（index.css 里 .mobile-error 已在 anywhere 名单内）。
              <p className="mobile-error mobile-send-failure" data-testid="mobile-send-failure">
                {sent.failure}
              </p>
            )}
            {detail !== "" && (
              <div className="mobile-bubble agent" data-testid="mobile-bubble-agent">
                <span className="mobile-bubble-mark" aria-hidden="true">
                  ↳
                </span>
                <div className="mobile-bubble-body">
                  <span className="mobile-reply-label">最新回复</span>
                  <MarkdownView text={replyShown} />
                  {folded && (
                    <button className="mobile-more-line" data-testid="mobile-expand" onClick={() => setExpanded(true)}>
                      展开这一条（{replyLines.length} 行）
                    </button>
                  )}
                </div>
              </div>
            )}
            {/* 发不了的行（无句柄且宿主也不收文本：Claude / Codex / Grok、旧扩展；或句柄在而写
                不了：采纳 socket、死 pane——agora-prdg.2）：说明必须显式给（agora-71p2；空
                thread 的用例见 agora-sd0b）。带宿主通道的行走下面那条 host-running note，有
                可写运行时又在中途的走更下面那条 running note。 */}
            {!writableRuntime && !hostText && (
              <p className="mobile-note" data-testid="mobile-terminal-only">
                {running
                  ? "它还在跑；这一行只能在桌面终端里回复。"
                  : "这一行没有可写的运行时；回复要到桌面终端。"}
              </p>
            )}
            {writableRuntime && userText === "" && detail === "" && !running && (
              <p className="mobile-note" data-testid="mobile-empty-thread">
                还没有可显示的内容；回一条就会出现在这里。
              </p>
            )}
          </>
        )}
      </div>

      {note && <p className="mobile-note" data-testid="mobile-card-note">{note}</p>}
      {error && <p className="mobile-error" data-testid="mobile-card-error">{error}</p>}

      {writableRuntime && (
        <div className="mobile-actions">
          <button className="mobile-more-toggle" data-testid="mobile-more" aria-expanded={more} onClick={() => setMore((v) => !v)}>
            更多 {more ? "▾" : "▸"}
          </button>
          {more && (
            <div className="mobile-more-actions">
              {canRestart && (
                <button data-testid="mobile-restart" disabled={busy || row.stale === true} onClick={() => void run("restart", false)}>
                  重新启动
                </button>
              )}
              <button data-testid="mobile-kill" className="danger" disabled={busy || row.stale === true} onClick={() => void run("kill", false)}>
                结束会话
              </button>
            </div>
          )}
        </div>
      )}

      {onNext && <button type="button" className="mobile-next-task" onClick={onNext}>下一件待处理 <span aria-hidden="true">→</span></button>}
      {canCompose && (
        <form
          className="mobile-composer"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <textarea
            rows={2}
            ref={composerRef}
            value={draft}
            placeholder="下一条指令"
            aria-label="下一条指令"
            data-testid="mobile-next-input"
            disabled={busy || row.stale === true}
            onChange={(e) => setDraft(e.target.value)}
            onFocus={(e) => keepAboveKeyboard(e.currentTarget)}
          />
          <button type="submit" data-testid="mobile-send" disabled={!draft.trim() || sent?.phase === "sending" || row.stale === true}>
            {sent?.phase === "sending" && <span className="mobile-spinner" aria-hidden="true" />}
            {sent?.phase === "sending" ? "发送中…" : "发送"}
          </button>
        </form>
      )}
      {running && hostText && (
        <p className="mobile-note" data-testid="mobile-host-running-note">
          它还在跑；发出去会排队，等它跑完这一轮就交进去。
        </p>
      )}
      {running && !hostText && writableRuntime && (
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
