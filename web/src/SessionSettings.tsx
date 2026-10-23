import { useEffect, useState } from "react";
import type { RestartResult, SessionApi } from "./api";
import { ConfirmDialog } from "./ConfirmDialog";
import { rowProcess, textVia, type SessionRow } from "./events";
import { rowName } from "./Sidebar";

interface Props {
  row: SessionRow;
  api: SessionApi;
  onClose: () => void;
}

type Pending = { kind: "kill" | "restart" } | null;

/** Kill 确认框文案是 docs/spec/ux.md 定的，改文案先改 spec。 */
export const KILL_BODY = "将结束正在运行的 agent 进程。输出会保留，直到你清理这个会话。";
export const RESTART_BODY =
  "将结束当前进程，在同一个会话中重新启动并尝试恢复对话。";

/** 节点对 Restart 的交代：resume 了哪个对话，或为什么退化为原命令（ADR-002 D7，绝不静默）。 */
export function restartNoteOf(v: unknown): string {
  const r = (v as RestartResult | null | undefined)?.restart;
  if (!r) return "已重新启动。";
  if (r.resumed) return `已重新启动，恢复对话 ${r.agent_session_id ?? ""}。`;
  return `已重新启动，但未恢复对话：${r.reason ?? "未知原因"}。`;
}

/**
 * Screen D：改名、Kill、Restart、Delete metadata（MISSION §4.6）。
 * 改名同名也发（§4.5）；Kill / Restart 先不带 confirmed 发，节点说会杀才弹框（§8）。
 */
export function SessionSettings({ row, api, onClose }: Props) {
  const [name, setName] = useState(String(row.display_name ?? rowName(row)));
  const [pending, setPending] = useState<Pending>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [restartNote, setRestartNote] = useState<string | null>(null);
  /** 确认之后到节点返回之前：Kill 走 TERM → 5 s → KILL 宽限（ADR-001 D2），shell 会吃满，
   * 只把按钮变灰会让人以为没点上（agora-9nv）。节点现在只同步等 1 s、没退就带着 killed_at
   * 立即返回（agora-284），所以 Kill 的"正在结束"还要看行本身：killed_at 已写而进程仍活着。
   * 读 `process` 不读旧的 `alive`（agora-5gg.18）：那个字段只保留一版，下一版删。 */
  const [ending, setEnding] = useState<"kill" | "restart" | null>(null);
  const killing = ending ?? (row.killed_at && rowProcess(row) === "alive" ? "kill" : null);
  // 只有可写的、由 agora 管的运行时才接受 Kill / Restart。采纳行（managed=false）与死 pane
  // （text_via=none）点下去只会 409 read_only；旧节点没有 text_via 时退回 runtime_ref（agora-q4kl）。
  const canOperate = textVia(row) === "runtime" && row.managed !== false && rowProcess(row) === "alive";
  const canRestart = canOperate && typeof row.command === "string" && row.command.trim() !== "";

  useEffect(() => {
    setName(String(row.display_name ?? rowName(row)));
  }, [row.id, row.display_name]);

  async function run(kind: "kill" | "restart", confirmed: boolean) {
    setBusy(true);
    setError(null);
    setRestartNote(null);
    if (confirmed) setEnding(kind);
    const r = await (kind === "kill" ? api.kill(row.id, confirmed) : api.restart(row.id, confirmed));
    setBusy(false);
    setEnding(null);
    if (r.ok) {
      setPending(null);
      if (kind === "restart") setRestartNote(restartNoteOf(r.value));
      return;
    }
    if (r.needsConfirmation) {
      setPending({ kind });
      return;
    }
    setPending(null);
    setError(`${r.error.error}: ${r.error.message}`);
  }

  async function rename() {
    setBusy(true);
    setError(null);
    const r = await api.rename(row.id, name.trim());
    setBusy(false);
    if (!r.ok && !r.needsConfirmation) setError(`${r.error.error}: ${r.error.message}`);
  }

  async function deleteMetadata() {
    setBusy(true);
    setError(null);
    const r = await api.deleteMetadata(row.id);
    setBusy(false);
    if (r.ok) onClose();
    else if (!r.needsConfirmation) setError(`${r.error.error}: ${r.error.message}`);
  }

  const label = `${rowName(row)} / ${String(row.agent_type ?? "")} @ ${row.node}`;
  const conversation = typeof row.agent_session_id === "string" ? row.agent_session_id : null;
  return (
    <section className="settings" aria-label="会话设置">
      <div className="settings-head">
        <h2>会话设置</h2>
        <button onClick={onClose} aria-label="关闭设置">
          ×
        </button>
      </div>
      <form
        className="settings-row"
        onSubmit={(e) => {
          e.preventDefault();
          void rename();
        }}
      >
        <label htmlFor="rename">名称</label>
        <input id="rename" value={name} onChange={(e) => setName(e.target.value)} disabled={busy} />
        <button type="submit" disabled={busy || name.trim() === ""}>
          改名
        </button>
      </form>
      <div className="settings-row actions">
        <button
          onClick={() => void run("restart", false)}
          disabled={busy || !canRestart}
          title={canRestart ? undefined : "采纳的会话没记下启动命令，无法重新启动；请在终端里自己重启"}
        >
          重新启动
        </button>
        {canOperate && (
          <button className="danger" onClick={() => void run("kill", false)} disabled={busy} data-testid="kill">
            结束会话
          </button>
        )}
        <button onClick={() => void deleteMetadata()} disabled={busy} title="只移除 agora 的记录，不杀运行时会话">
          删除记录
        </button>
      </div>
      <p className="muted">
        重新启动会在同一运行时会话内重建并尝试恢复原对话
        {conversation ? <>（当前对话 <code>{conversation}</code>）</> : "（agent 还没自报对话 id，重新启动会用原命令）"}
        ；删除记录不会结束进程。
      </p>
      {restartNote && (
        <p className="muted restart-note" data-testid="restart-note" title={restartNote}>
          {restartNote}
        </p>
      )}
      {killing && (
        <p className="muted" data-testid="ending-note">
          {killing === "kill" ? "正在结束…" : "正在重启…"}（先请进程退出，最多等 7 秒）
        </p>
      )}
      {error && <p className="error">{error}</p>}
      {pending && (
        <ConfirmDialog
          title={pending.kind === "kill" ? `结束会话 ${label}？` : `重新启动 ${label}？`}
          body={pending.kind === "kill" ? KILL_BODY : RESTART_BODY}
          confirmLabel={pending.kind === "kill" ? "结束会话" : "重新启动"}
          cancelLabel="取消"
          onCancel={() => setPending(null)}
          onConfirm={() => void run(pending.kind, true)}
        />
      )}
    </section>
  );
}
