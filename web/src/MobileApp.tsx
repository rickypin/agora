/**
 * 手机交互收件箱 /m（MISSION §6.9；A37 / A52；agora-thc.5）。
 *
 * 这一屏只回答一件事：**现在谁在等我**。四段与排序复用 attention.ts（与桌面同一条规则、同一个
 * 「看过」集合），但信息预算只有一行——状态 + 时长、agent 徽标、任务标签、节点、一行摘要。没有
 * 终端、创建、diff / 验收 / 改动列表，也没有消息流与历史翻页（A52；DOM 守卫在 MobileApp.test.tsx）。
 *
 * 会话卡（决策原文、回复气泡、composer、Kill / Restart）不在这个文件里：那是 agora-thc.10，接在
 * 行点击上。本任务里点行只做选中高亮——深链 `?session=<node>:<id>` 用同一格把行找出来并滚到视野内。
 */
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type Ref,
} from "react";
import { agentBadge } from "./agentBadge";
import { loadSeen, sectionOf, sortByAttention, statusLine, taskLabel, type Section, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { parseSessionTarget } from "./mobileRoute";
import { nodeHue } from "./nodeColor";
import { rowName, statusSymbol, str } from "./SessionRow";
import { SessionStore, useSessions } from "./store";

/** 前三段固定顺序（与 attention.ts 的 partitionByAttention 同一顺序）；已完成是折叠的一段。 */
const OPEN_SECTIONS: { key: Section; label: string }[] = [
  { key: "attention", label: "需要我" },
  { key: "unclear", label: "说不清" },
  { key: "working", label: "在跑" },
];

interface Props {
  store?: SessionStore;
  health?: HealthWatcher;
  version?: VersionWatcher;
  /** 事件流被服务端以 4401 关掉（本设备被吊销）：App 换回配对门。 */
  onRevoked?: () => void;
  /** 测试注入：现在（unix 秒）；不给就每 30 s 自己走一格。 */
  now?: number;
}

/** 四段分组：先按 attention 的排序排好，再按 sectionOf 落段（段内顺序 = 排序顺序）。 */
export function groupSections(rows: SessionRow[], seen: SeenSet): Record<Section, SessionRow[]> {
  const out: Record<Section, SessionRow[]> = { attention: [], unclear: [], working: [], finished: [] };
  for (const row of sortByAttention(rows)) out[sectionOf(row, seen)].push(row);
  return out;
}

/** 一行摘要：progress > detail > reason 的第一行，80 字封顶（docs/spec/ux.md 的信息预算）。 */
export function mobileSummary(row: SessionRow): string {
  const raw = str(row.progress) || str(row.detail) || str(row.reason);
  const line = raw.split("\n").find((l) => l.trim() !== "")?.trim() ?? "";
  return line.length > 80 ? `${line.slice(0, 79)}…` : line;
}

export function MobileApp({ store: given, health: givenHealth, version: givenVersion, onRevoked, now }: Props) {
  const store = useMemo(() => given ?? new SessionStore(), [given]);
  const rows = useSessions(store);
  const health = useMemo(() => givenHealth ?? new HealthWatcher(), [givenHealth]);
  const version = useMemo(() => givenVersion ?? new VersionWatcher(), [givenVersion]);
  const localNode = useSyncExternalStore(version.subscribe, version.nodeSnapshot, version.nodeSnapshot);
  const nodes = useSyncExternalStore(health.subscribeNodes, health.nodesSnapshot, health.nodesSnapshot);
  const [clock, setClock] = useState(() => Math.floor(Date.now() / 1000));

  useEffect(() => {
    health.start();
    return () => health.stop();
  }, [health]);
  useEffect(() => {
    // onOpen 在连上时比一次 api_version（与桌面同一条纪律：升级必然断流一次）。
    store.onRevoked = onRevoked ?? null;
    store.onOpen = () => void version.check();
    store.start();
    return () => {
      store.onRevoked = null;
      store.onOpen = null;
      store.stop();
    };
  }, [store, version, onRevoked]);
  useEffect(() => {
    const timer = setInterval(() => setClock(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(timer);
  }, []);

  // 「看过」只读：写它的时机是"看到结果"，那发生在会话卡里（agora-thc.10），不在这里。
  const seen: SeenSet = useMemo(() => loadSeen(), []);
  const sections = useMemo(() => groupSections(rows, seen), [rows, seen]);

  // 深链（推送点击进来）：`/m?session=<node>:<id>`。node 是行的完整身份的一部分，先精确匹配、
  // 再退回 id（旧载荷 / 手输）。
  const target = useMemo(() => parseSessionTarget(window.location.search), []);
  const targetRow = useMemo(() => {
    if (!target) return undefined;
    return rows.find((r) => r.id === target.id && r.node === target.node) ?? rows.find((r) => r.id === target.id);
  }, [rows, target]);
  const [selected, setSelected] = useState<string | null>(null);
  const [finishedOpen, setFinishedOpen] = useState(false);
  useEffect(() => {
    if (targetRow && selected === null) setSelected(targetRow.id);
  }, [targetRow, selected]);
  useEffect(() => {
    // 深链落在收起的已完成区：先把它打开，再滚过去——否则"定位到了"却看不见。
    if (targetRow && sectionOf(targetRow, seen) === "finished") setFinishedOpen(true);
  }, [targetRow, seen]);
  const selectedRef = useRef<HTMLLIElement | null>(null);
  useEffect(() => {
    if (selected === null) return;
    selectedRef.current?.scrollIntoView?.({ block: "center" });
  }, [selected, finishedOpen]);

  const nowSeconds = now ?? clock;
  const renderRow = (row: SessionRow) => (
    <MobileRow
      key={row.id}
      row={row}
      now={nowSeconds}
      selected={row.id === selected}
      localNode={localNode}
      onOpen={setSelected}
      liRef={row.id === selected ? selectedRef : undefined}
    />
  );

  return (
    <main className="mobile" data-testid="mobile-inbox">
      <header className="mobile-top">
        <h1>agora</h1>
        <span className="mobile-carrier" data-testid="mobile-carrier" data-reachable={String(nodes.reachable === true)}>
          承载节点 {localNode ?? "…"} {nodes.reachable === false ? "○" : "●"}
        </span>
      </header>
      <div className="mobile-inbox">
        {OPEN_SECTIONS.map(({ key, label }) => (
          <section key={key} className="mobile-section" data-testid={`mobile-section-${key}`} aria-label={label}>
            <h2>
              <span>{label}</span>
              <span>{sections[key].length}</span>
            </h2>
            {sections[key].length === 0
              ? key === "attention" && <p className="mobile-empty">没有等你的事</p>
              : <ul>{sections[key].map(renderRow)}</ul>}
          </section>
        ))}
        {sections.finished.length > 0 && (
          <section className="mobile-section" data-testid="mobile-section-finished" aria-label="已完成">
            <button
              type="button"
              className="mobile-finished-toggle"
              data-testid="mobile-finished-toggle"
              aria-expanded={finishedOpen}
              onClick={() => setFinishedOpen((v) => !v)}
            >
              <span>已完成</span>
              <span>
                {sections.finished.length} {finishedOpen ? "▾" : "▸"}
              </span>
            </button>
            {finishedOpen && <ul>{sections.finished.map(renderRow)}</ul>}
          </section>
        )}
      </div>
    </main>
  );
}

interface RowProps {
  row: SessionRow;
  now: number;
  selected: boolean;
  /** 本机 node.id（`/api/system`）：已知后每一行标节点 chip，本机不着色（与桌面 RowIdentity 同规矩）。 */
  localNode: string | null;
  onOpen: (id: string) => void;
  liRef?: Ref<HTMLLIElement>;
}

function MobileRow({ row, now, selected, localNode, onOpen, liRef }: RowProps) {
  const badge = agentBadge(String(row.agent_type ?? ""));
  const local = localNode !== null && row.node === localNode;
  const summary = mobileSummary(row);
  return (
    <li ref={liRef}>
      <button
        type="button"
        className={`mobile-row${selected ? " selected" : ""}`}
        data-testid={`mobile-row-${row.id}`}
        aria-current={selected ? "true" : undefined}
        onClick={() => onOpen(row.id)}
      >
        <span className="mobile-row-head">
          <span className="mobile-symbol" aria-hidden="true">
            {statusSymbol(row.status)}
          </span>
          <span className="mobile-row-name">{rowName(row)}</span>
          <span className="mobile-agent" style={{ "--hue": badge.hue } as CSSProperties}>
            {badge.glyph} {badge.label}
          </span>
          {localNode !== null && (
            <span
              className={`mobile-node-chip${local ? " local" : " peer"}`}
              data-testid={`mobile-node-${row.id}`}
              data-node={row.node}
              style={local ? undefined : ({ "--hue": nodeHue(row.node) } as CSSProperties)}
            >
              @{row.node}
            </span>
          )}
          <span className="mobile-status">{statusLine(row, now)}</span>
        </span>
        <span className="mobile-row-task">{taskLabel(row)}</span>
        {summary !== "" && <span className="mobile-row-summary">{summary}</span>}
      </button>
    </li>
  );
}
