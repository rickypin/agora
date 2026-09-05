/**
 * 主界面 header：docs/spec/ux.md 主界面线框的首行（`agora   mac ●  zuan ●`），MISSION §6.1 / §10.3
 * 说它显示本机与每个 peer 的状态。侧栏就是 Dashboard（ux.md Attention Dashboard 线框首行
 * `mac ● zuan ●   Agents: 12`），所以 header 也就是侧栏的首行。
 *
 * 先按 AGENTS.md 热点组件的规矩从 Sidebar.tsx 原样抽出来（这一步不改任何行为），peer 状态
 * 随 agora-7ku.12 加进来；后来者只改这个文件，不再碰 Sidebar / Workspace。
 */
interface Props {
  /** 侧栏计数文案：过滤时 `shown/total`，否则 total。 */
  agents: string | number;
}

export function Header({ agents }: Props) {
  return (
    <div className="sidebar-head">
      <h1>agora</h1>
      <span className="muted">AGENTS {agents}</span>
    </div>
  );
}
