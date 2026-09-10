import { useEffect, useRef, useState } from "react";
import type { SessionApi } from "./api";
import type { SessionRow } from "./events";
import { MarkdownView } from "./MarkdownView";

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
/**
 * 最后一条回复默认露出的行数（agora-4yr.2；docs/spec/ux.md「回答面板」写的就是这个 12 行）。
 *
 * 按**源文本**的 `\n` 数截，而不是 CSS 限高：限高会随字号 / 表格 / 代码块飘，同一段回复在
 * 不同机器上折叠位置不一样，也没法拿一个数字跟 spec 对账（agora-4yr.5 的验收要按 ux.md 里的
 * 「12 行」grep 代码）。截断落在围栏 / 表格中间的处理在 markdown.ts。
 *
 * 与 `.respond-last { max-height: 40vh }`（agora-4yr.1）不冲突：这里管默认露多少，40vh 管
 * 展开之后最多占多高、超了自己滚，面板不会把终端挤没。
 */
const FOLD_LINES = 12;
export function RespondPanel({ row, api, onOpenTerminal, focusRequest, onFocusHandled }: Props) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const rootRef = useRef<HTMLElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    setError(null);
  }, [row.id, row.status, row.reason, row.pending_decision?.request_id]);
  // 折叠态只活在"这一条回复"上：换一行、或者同一行来了新回复，都回到折叠。**不持久化**
  // （ux.md 写明，agora-4yr.5 按 ux.md 逐项对账）——"我看没看完这一条"是即时状态，而 detail
  // 每次 TURN_DONE 都换，存下来的键下一次只会让人对着一段没读过的长文默认展开。
  useEffect(() => {
    setExpanded(false);
  }, [row.id, row.detail]);
  useEffect(() => {
    if (!focusRequest) return;
    // 推迟一个宏任务再抢焦点，不能在 effect 里直接 focus。TerminalView 的挂载 effect 末尾有一句
    // term.focus()，而它在树里排在面板后面——同一次 commit 里面板先聚焦、终端后聚焦，最后焦点
    // 还是终端的。2026-09-10 agent-browser 0.33.2 代检实测：点通知切到另一行，crumb 换成了那一行、
    // 面板也画出来了，document.activeElement 仍是 xterm 的 helper TEXTAREA。jsdom 里 TerminalView
    // 是个不会 focus 的替身，这条测不出来，别因为单测绿就把 setTimeout 拿掉。
    // 之后 WS「attached」到达时那一次 focus 有 TerminalView 自己的 typingElsewhere 挡着（活动元素
    // 是别处的可交互控件就不抢——TURN_DONE 的 input 与 WAITING 的 Allow 按钮都算，agora-y3h），
    // 所以只需要赢下挂载这一次。
    const t = setTimeout(() => {
      // TURN_DONE 落在输入框，WAITING 落在第一个按钮（Allow，或只有"打开终端"时就是它）。
      (inputRef.current ?? rootRef.current?.querySelector("button"))?.focus();
      onFocusHandled?.();
    }, 0);
    return () => clearTimeout(t);
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
  const lastLines = detail === null ? [] : detail.split("\n");
  // 展开是单向的：展开后按钮消失，不变「收起」。回复是"看结果"的东西，看完就该给下一条指令
  // （输入框在面板最上面），再折回去没有用；40vh 的限高保证展开也不会把终端挤没。
  const folded = !expanded && lastLines.length > FOLD_LINES;
  const lastShown = folded ? lastLines.slice(0, FOLD_LINES).join("\n") : detail;

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
      {waiting &&
        (canDecide ? (
          // 权限请求的 summary 是**一行命令**（`src/adapter/hooks.rs permission_summary`：工具名 +
          // tool_input 主参数首行），不是 markdown。批准之前看到的文本必须逐字符等于真实命令
          // ——这是"respond 不经终端完成"（MISSION §1.2）的信任基础。走 markdown 会把命令里的元
          // 字符当语法吃掉（2026-09-10 实测：`sed -i 's/_a_/_b_/'` 的 `_a_` 两侧是 `/`、在词边界
          // 规则之外，渲染成斜体 a、下划线整个消失；`ls **/*.ts` 的 `**` 同理），人看到的与要
          // 批准的就不是同一串了（agora-k1s）。别为了"统一"把它改回 MarkdownView。
          <p className="respond-question respond-command" data-testid="respond-question">
            {pending.summary}
          </p>
        ) : (
          // 提问（AskUserQuestion 类）与兜底文案是 agent 写的散文，没有"逐字符批准"的语义
          // ——选项在 TUI 里，这里只是让人读懂发生了什么，排版有收益，保留 markdown。
          <MarkdownView className="respond-question" text={detail ?? String(row.reason ?? "等待你")} />
        ))}
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
          {lastShown !== null && (
            <>
              <div className="respond-last muted" data-testid="respond-last">
                <span className="respond-last-mark" aria-hidden="true">
                  ↳
                </span>
                <MarkdownView text={lastShown} />
              </div>
              {folded && (
                <button className="respond-more" data-testid="respond-last-more" onClick={() => setExpanded(true)}>
                  展开全文（{lastLines.length} 行）
                </button>
              )}
            </>
          )}
        </>
      )}
      {error && <p className="respond-error">{error}</p>}
    </section>
  );
}
