import { useEffect, useState } from "react";
import type { RestartResult, SessionApi } from "./api";
import { ConfirmDialog } from "./ConfirmDialog";
import type { SessionRow } from "./events";
import { rowName } from "./Sidebar";

interface Props {
  row: SessionRow;
  api: SessionApi;
  onClose: () => void;
}

type Pending = { kind: "kill" | "restart" } | null;

/** Kill 确认框文案是 docs/spec/ux.md 定的，改文案先改 spec。 */
export const KILL_BODY = "The running agent process will be killed. Its output stays until you clean it up.";
export const RESTART_BODY =
  "The running agent process will be killed and started again in the same session, resuming its conversation.";

/** 节点对 Restart 的交代：resume 了哪个对话，或为什么退化为原命令（ADR-002 D7，绝不静默）。 */
export function restartNoteOf(v: unknown): string {
  const r = (v as RestartResult | null | undefined)?.restart;
  if (!r) return "已 Restart。";
  if (r.resumed) return `已 Restart，resume 对话 ${r.agent_session_id ?? ""}。`;
  return `已 Restart，但没有 resume：${r.reason ?? "未知原因"}。`;
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
   * 只把按钮变灰会让人以为没点上（agora-9nv）。 */
  const [ending, setEnding] = useState<"kill" | "restart" | null>(null);
  const canRestart = typeof row.command === "string" && row.command.trim() !== "";

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
    <section className="settings" aria-label="Session Settings">
      <div className="settings-head">
        <h2>Session Settings</h2>
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
        <label htmlFor="rename">Name</label>
        <input id="rename" value={name} onChange={(e) => setName(e.target.value)} disabled={busy} />
        <button type="submit" disabled={busy || name.trim() === ""}>
          Rename
        </button>
      </form>
      <div className="settings-row actions">
        <button
          onClick={() => void run("restart", false)}
          disabled={busy || !canRestart}
          title={canRestart ? undefined : "采纳的会话没记下启动命令，无法 Restart；请在终端里自己重启"}
        >
          Restart
        </button>
        <button className="danger" onClick={() => void run("kill", false)} disabled={busy} data-testid="kill">
          Kill
        </button>
        <button onClick={() => void deleteMetadata()} disabled={busy} title="只移除 agora 的记录，不杀运行时会话">
          Delete metadata
        </button>
      </div>
      <p className="muted">
        Restart 在同一运行时会话内重建并 resume 原对话
        {conversation ? <>（当前对话 <code>{conversation}</code>）</> : "（agent 还没自报对话 id，Restart 会用原命令）"}
        ；Delete metadata 不杀进程。
      </p>
      {restartNote && <p className="muted" data-testid="restart-note">{restartNote}</p>}
      {ending && (
        <p className="muted" data-testid="ending-note">
          {ending === "kill" ? "正在结束…" : "正在重启…"}（先请进程退出，最多等 7 秒）
        </p>
      )}
      {error && <p className="error">{error}</p>}
      {pending && (
        <ConfirmDialog
          title={pending.kind === "kill" ? `Kill ${label}?` : `Restart ${label}?`}
          body={pending.kind === "kill" ? KILL_BODY : RESTART_BODY}
          confirmLabel={pending.kind === "kill" ? "Kill" : "Restart"}
          onCancel={() => setPending(null)}
          onConfirm={() => void run(pending.kind, true)}
        />
      )}
    </section>
  );
}
