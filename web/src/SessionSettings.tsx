import { useEffect, useState } from "react";
import type { SessionApi } from "./api";
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

/**
 * Screen D：改名、Kill、Restart、Delete metadata（MISSION §4.6）。
 * 改名同名也发（§4.5）；Kill / Restart 先不带 confirmed 发，节点说会杀才弹框（§8）。
 */
export function SessionSettings({ row, api, onClose }: Props) {
  const [name, setName] = useState(String(row.display_name ?? rowName(row)));
  const [pending, setPending] = useState<Pending>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setName(String(row.display_name ?? rowName(row)));
  }, [row.id, row.display_name]);

  async function run(kind: "kill" | "restart", confirmed: boolean) {
    setBusy(true);
    setError(null);
    const r = await (kind === "kill" ? api.kill(row.id, confirmed) : api.restart(row.id, confirmed));
    setBusy(false);
    if (r.ok) {
      setPending(null);
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
        <button onClick={() => void run("restart", false)} disabled={busy}>
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
        Restart 在同一运行时会话内重建并 resume 原对话；Delete metadata 不杀进程。
      </p>
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
