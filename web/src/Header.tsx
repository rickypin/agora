import type { NodesHealth, PeerError, PeerHealth } from "./health";

/**
 * 主界面 header：docs/spec/ux.md 主界面线框的首行（`agora   mac ●  zuan ●`），MISSION §6.1 / §10.3
 * 说它显示本机与每个 peer 的状态（在线 / 异常原因 / 上次见到）。侧栏就是 Dashboard（ux.md Attention
 * Dashboard 线框首行 `mac ● zuan ●   Agents: 12`），所以 header 也就是侧栏的首行。
 *
 * 数据源是 HealthWatcher 同一次拉取带出的 peers 段（agora-7ku.12），不另起轮询；peer 上线 / 掉线由
 * `/api/events` 的 `peer_changed` 就地更新同一份快照（agora-c8h），与侧栏行同一眼变。异常原因只按
 * `last_error` 的五个类型选文案，从不解析任何消息文本（MISSION §2.3 规则 10）。本机那一枚的名字是
 * `/api/system` 报的 node.id（VersionWatcher 同一次拉取带出，agora-7ku.5）：`mac ● zuan ●` 里两枚都是
 * 节点名，与侧栏行上的 `@ zuan` 对得上；还没拉到之前先叫"本机"。
 */

/** Header 上一枚节点状态：本机一枚（`local`），每个 peer 一枚。 */
export interface NodeStatus extends PeerHealth {
  name: string;
  local?: boolean;
}

/** 本机排第一，名字是 `localName`（`/api/system` 的 node；还没拉到时"本机"），peer 按 health 的键序。 */
export function nodeStatuses(h: NodesHealth, localName?: string | null): NodeStatus[] {
  const local: NodeStatus = {
    name: localName ?? "本机",
    local: true,
    // reachable 还是 null（没拉过）当"不在线但也没错"，渲染成探测中。
    online: h.reachable === true,
    last_seen: null,
    retrying: false,
    last_error: h.reachable === false ? "unreachable" : null,
  };
  return [local, ...Object.entries(h.peers).map(([name, p]) => ({ name, ...p }))];
}

/** 五个错误类型的文案（唯一出处）。 */
export function peerErrorLabel(e: PeerError): string {
  switch (e) {
    case "incompatible_version":
      return "版本不兼容";
    case "fingerprint_mismatch":
      return "指纹不匹配";
    case "unauthorized":
      return "未授权";
    case "unreachable":
      return "不可达";
    case "misconfigured":
      // ADR-003 D3 / agora-41e：本机这一行 peers[] 配错了（token_file 权限、url、指纹），要改文件，
      // 不是等网络。retrying 恒 false，所以 title 不会带"重试中"——现有逻辑已是条件的。
      return "配置错误";
  }
}

/** `HH:MM`，本地时区（时间是本节点打的，浏览器多半就在这台机器旁边）；解析不了就原样给。 */
export function clockText(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 一枚节点的点：符号、`.node-status` 的类、离线原因。树视图的节点组头复用（agora-uvd.3），与 Header 同源。 */
export function describeNode(n: NodeStatus): { symbol: string; cls: string; reason: string | null } {
  if (n.online) return { symbol: "●", cls: "ok", reason: null };
  if (n.last_error) return { symbol: "✗", cls: "bad", reason: peerErrorLabel(n.last_error) };
  if (n.last_seen) return { symbol: "○", cls: "stale", reason: null };
  return { symbol: n.local ? "…" : "○", cls: "unknown", reason: n.local ? "探测中" : "未连接" };
}

function NodeChip({ n }: { n: NodeStatus }) {
  const { symbol, cls, reason } = describeNode(n);
  // 在线只有名字和点；离线才带原因与"上次见到"（MISSION §3.5：保留最后一眼，绝不静默消失）。
  const seen = !n.online && n.last_seen ? `上次见到 ${clockText(n.last_seen)}` : null;
  const note = [reason, seen].filter((s): s is string => s !== null).join(" · ");
  const title = [
    n.online ? "在线" : (reason ?? "离线"),
    n.last_seen ? `上次见到 ${n.last_seen}` : null,
    n.retrying ? "重试中" : null,
  ]
    .filter((s): s is string => s !== null)
    .join("，");
  return (
    <span className={`node-status ${cls}`} data-testid={`node-${n.name}`} title={title}>
      <span className="node-name">{n.name}</span>
      <span className="dot">{symbol}</span>
      {note && <span className="node-note">{note}</span>}
    </span>
  );
}

interface Props {
  /** 侧栏计数文案：过滤时 `shown/total`，否则 total。 */
  agents: string | number;
  /** 本机与每个 peer；不给就不渲染节点行（只有标题的老形态）。 */
  nodes?: NodeStatus[];
}

export function Header({ agents, nodes }: Props) {
  return (
    <>
      <div className="sidebar-head">
        <h1>agora</h1>
        <span className="muted">AGENTS {agents}</span>
      </div>
      {nodes && (
        <div className="nodes" data-testid="nodes">
          {nodes.map((n) => (
            <NodeChip key={n.name} n={n} />
          ))}
        </div>
      )}
    </>
  );
}
