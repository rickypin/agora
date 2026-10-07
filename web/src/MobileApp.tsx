/**
 * 手机交互收件箱 /m（MISSION §6.9；A37 / A52；agora-thc.5；会话卡 agora-thc.10）。
 *
 * 这一屏只回答一件事：**现在谁在等我**。四段与排序复用 attention.ts（与桌面同一条规则、同一个
 * 「看过」集合），但信息预算只有一行——状态 + 时长、agent 徽标、任务标签、节点、一行摘要。没有
 * 终端、创建、diff / 验收 / 改动列表，也没有消息流与历史翻页（A52；DOM 守卫在 MobileApp.test.tsx）。
 *
 * 点一行进会话卡（MobileCard）：即时消息语法，动作走与桌面相同的节点 API。本文件只管收件箱、
 * 深链与会话卡的进出；卡片内部的决策 / composer / Kill / Restart 在 MobileCard 里。
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
} from "react";
import { agentBadge } from "./agentBadge";
import { sessionApi, type SessionApi } from "./api";
import { loadSeen, sectionOf, sortByAttention, statusLine, storeSeen, taskLabel, type Section, type SeenSet } from "./attention";
import type { SessionRow } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { MobileCard } from "./MobileCard";
import { MobileSettings } from "./MobileSettings";
import { parseSessionTarget } from "./mobileRoute";
import { browserPushEnv, selfCheckPush, type PushEnv } from "./push";
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
  api?: SessionApi;
  health?: HealthWatcher;
  version?: VersionWatcher;
  /** 事件流被服务端以 4401 关掉（本设备被吊销）：App 换回配对门。 */
  onRevoked?: () => void;
  /** 测试注入：现在（unix 秒）；不给就每 30 s 自己走一格。 */
  now?: number;
  /** 测试注入：推送客户端的浏览器环境（agora-thc.7）。 */
  pushEnv?: PushEnv;
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

export function MobileApp({ store: given, api: givenApi, health: givenHealth, version: givenVersion, onRevoked, now, pushEnv: givenPushEnv }: Props) {
  const store = useMemo(() => given ?? new SessionStore(), [given]);
  const api = useMemo(() => givenApi ?? sessionApi(), [givenApi]);
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
  // 打开 /m 自查一次订阅（agora-thc.7）：iOS 可能静默回收订阅，本地看着还在、其实已经没人发；
  // 有就重新登记、没有但权限还在就重订。失败不打扰：设置页会显示状态。
  const pushEnv = useMemo(() => givenPushEnv ?? browserPushEnv(), [givenPushEnv]);
  useEffect(() => {
    if (!pushEnv.supported || !pushEnv.secure) return;
    void selfCheckPush(pushEnv);
  }, [pushEnv]);
  // SW 的 notificationclick 在页面已开着时发来消息（不重新加载）：定位到那一行，可发送的把焦点
  // 放进 composer（docs/spec/ux.md「行为」；agora-thc.7）。
  useEffect(() => {
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return;
    const onMessage = (ev: MessageEvent) => {
      const data = ev.data as { type?: string; session?: string | null } | null;
      if (!data || data.type !== "agora-open-session") return;
      const session = typeof data.session === "string" ? data.session : null;
      if (!session) return;
      const at = session.indexOf(":");
      if (at <= 0) return;
      setSelected(session);
      setFocusComposerFor(session);
    };
    navigator.serviceWorker.addEventListener("message", onMessage);
    return () => navigator.serviceWorker.removeEventListener("message", onMessage);
  }, []);

  // 「看过」集合（每设备 localStorage，与桌面同键；写发生在会话卡打开那一刻）。
  const [seen, setSeen] = useState<Set<string>>(() => loadSeen());
  const markSeen = useCallback((key: string) => {
    setSeen((prev) => {
      if (prev.has(key)) return prev;
      const next = new Set(prev);
      next.add(key);
      storeSeen(next);
      return next;
    });
  }, []);
  const sections = useMemo(() => groupSections(rows, seen), [rows, seen]);

  // 深链（推送点击进来）：`/m?session=<node>:<id>`。行的全局 id 就是 `<node>:<id>`
  // （api/sessions.rs 的 export），所以先拼回全名精确匹配、再退回幂等写法（手输 / 旧载荷）。
  const target = useMemo(() => parseSessionTarget(window.location.search), []);
  const targetRow = useMemo(() => {
    if (!target) return undefined;
    const full = `${target.node}:${target.id}`;
    return (
      rows.find((r) => r.id === full && r.node === target.node) ??
      rows.find((r) => r.id === full) ??
      rows.find((r) => r.id === target.id)
    );
  }, [rows, target]);
  const [selected, setSelected] = useState<string | null>(null);
  const [finishedOpen, setFinishedOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 推送点击进来的那一行要不要把焦点送进 composer（只有可发送的行要；打开一次即消费）。
  const [focusComposerFor, setFocusComposerFor] = useState<string | null>(null);
  // 深链只自动打开一次：只用 `selected === null` 判，返回收件箱后会把同一行又弹出来。
  const deepLinked = useRef(false);
  useEffect(() => {
    if (deepLinked.current || !targetRow) return;
    deepLinked.current = true;
    setSelected(targetRow.id);
    // 可发送的行（turn_done / idle）把焦点放进 composer——推送点开就是要你回话（ux.md）。
    if (targetRow.status === "turn_done" || targetRow.status === "idle") setFocusComposerFor(targetRow.id);
  }, [targetRow]);
  useEffect(() => {
    // 选中的行没了（被删 metadata）：回到收件箱。
    if (selected !== null && !rows.some((r) => r.id === selected)) setSelected(null);
  }, [rows, selected]);

  const nowSeconds = now ?? clock;
  const selectedRow = selected === null ? undefined : rows.find((r) => r.id === selected);

  if (selectedRow) {
    return (
      <main className="mobile" data-testid="mobile-inbox">
        <MobileCard
          row={selectedRow}
          api={api}
          now={nowSeconds}
          onBack={() => setSelected(null)}
          onSeen={markSeen}
          focusComposer={focusComposerFor === selectedRow.id}
        />
      </main>
    );
  }
  if (settingsOpen) {
    return (
      <MobileSettings
        env={givenPushEnv}
        onClose={() => setSettingsOpen(false)}
        onRevoked={onRevoked ?? (() => {})}
      />
    );
  }

  const renderRow = (row: SessionRow) => (
    <MobileRow key={row.id} row={row} now={nowSeconds} localNode={localNode} onOpen={setSelected} />
  );

  return (
    <main className="mobile" data-testid="mobile-inbox">
      <header className="mobile-top">
        <h1>agora</h1>
        <span className="mobile-carrier" data-testid="mobile-carrier" data-reachable={String(nodes.reachable === true)}>
          承载节点 {localNode ?? "…"} {nodes.reachable === false ? "○" : "●"}
        </span>
        <button type="button" className="mobile-gear" data-testid="mobile-settings-open" onClick={() => setSettingsOpen(true)}>
          设置
        </button>
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
  /** 本机 node.id（`/api/system`）：已知后每一行标节点 chip，本机不着色（与桌面 RowIdentity 同规矩）。 */
  localNode: string | null;
  onOpen: (id: string) => void;
}

function MobileRow({ row, now, localNode, onOpen }: RowProps) {
  const badge = agentBadge(String(row.agent_type ?? ""));
  const local = localNode !== null && row.node === localNode;
  const summary = mobileSummary(row);
  return (
    <li>
      <button
        type="button"
        className="mobile-row"
        data-testid={`mobile-row-${row.id}`}
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
