import { taskLabel } from "./attention";
import type { SessionRow } from "./events";
import { mobileStatusText } from "./mobileStatus";
import { statusClass } from "./RowIdentity";
import { rowName, statusSymbol } from "./SessionRow";

export function SessionHeading({ row, seen, settingsOpen, onSettings, onClose }: {
  row: SessionRow; seen: boolean; settingsOpen: boolean; onSettings: () => void; onClose: () => void;
}) {
  return <div className="session-heading">
    <div className="session-heading-main"><p className="eyebrow">{row.project?.name ?? "会话"}{row.project?.branch ? ` / ${row.project.branch}` : ""}</p>
      <h1 title={taskLabel(row)}>{taskLabel(row)}</h1>
      <div className="session-context"><span className={statusClass(row.status)}>{statusSymbol(row.status)} {mobileStatusText(row, seen)}</span><span className="muted" data-testid="crumb">{rowName(row)} / {String(row.agent_type ?? "")} @ {row.node}</span></div>
    </div>
    <div className="crumb-actions"><button onClick={onSettings} aria-pressed={settingsOpen}>会话设置</button><button onClick={onClose} data-testid="close-view" title="返回工作台；只关闭视图，agent 继续运行">返回工作台</button></div>
  </div>;
}
