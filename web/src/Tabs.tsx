import type { SessionRow } from "./events";
import { rowName, statusSymbol } from "./Sidebar";
import { diffTarget } from "./tabstate";

interface Props {
  open: string[];
  active: string | null;
  rows: Map<string, SessionRow>;
  onActivate: (id: string) => void;
  /** 关 Tab = Detach（MISSION §4.6）：只从列表移除。 */
  onClose: (id: string) => void;
}

export function Tabs({ open, active, rows, onActivate, onClose }: Props) {
  if (open.length === 0) return null;
  return (
    <div className="tabs" role="tablist">
      {open.map((id) => {
        // diff 标签页（agora-h1k.5）：`diff:<会话 id>`，标签叫 `diff · <会话名>`，点是 ± 而不是会话状态。
        const target = diffTarget(id);
        const r = rows.get(target ?? id);
        const name = r ? rowName(r) : (target ?? id);
        const label = target !== null ? `diff · ${name}` : name;
        return (
          <div key={id} role="tab" aria-selected={id === active} className={id === active ? "tab active" : "tab"}>
            <button className="tab-label" onClick={() => onActivate(id)} data-testid={`tab-${id}`}>
              {target !== null ? (
                <span className="dot st-diff">±</span>
              ) : (
                <span className={`dot st-${r?.status ?? ""}`}>{statusSymbol(r?.status ?? "")}</span>
              )}
              {label}
            </button>
            <button className="tab-close" aria-label={`关闭 ${label}`} onClick={() => onClose(id)} data-testid={`close-${id}`}>
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}
