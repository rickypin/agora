import type { SessionRow } from "./events";
import { rowName, statusSymbol } from "./Sidebar";

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
        const r = rows.get(id);
        return (
          <div key={id} role="tab" aria-selected={id === active} className={id === active ? "tab active" : "tab"}>
            <button className="tab-label" onClick={() => onActivate(id)} data-testid={`tab-${id}`}>
              <span className={`dot st-${r?.status ?? ""}`}>{statusSymbol(r?.status ?? "")}</span>
              {r ? rowName(r) : id}
            </button>
            <button className="tab-close" aria-label={`关闭 ${r ? rowName(r) : id}`} onClick={() => onClose(id)} data-testid={`close-${id}`}>
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}
