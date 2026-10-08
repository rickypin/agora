/**
 * 手机交互收件箱 /m（MISSION §6.9；A37 / A52；agora-thc.5；会话卡 agora-thc.10）。
 *
 * 这一屏只回答一件事：**现在谁在等我**。四段与排序复用 attention.ts（与桌面同一条规则、同一个
 * 「看过」集合），但信息预算只有一行——状态 + 时长、agent 徽标、任务标签、节点、一行摘要。没有
 * 终端、创建、diff / 验收 / 改动列表，也没有消息流与历史翻页（A52；DOM 守卫在 MobileApp.test.tsx）。
 *
 * 点一行进会话卡（MobileCard）：即时消息语法，动作走与桌面相同的节点 API。本文件管收件箱、
 * 深链、会话卡的进出，以及「已完成」段的清理入口（与桌面同一删除名单、逐行 DELETE、跳过 stale）；
 * 卡片内部的决策 / composer / Kill / Restart 在 MobileCard 里。
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
import { sessionApi, type PresetInfo, type SessionApi } from "./api";
import { isHandleless, loadSeen, sectionOf, sortByAttention, statusLine, storeSeen, taskLabel, type Section, type SeenSet } from "./attention";
import { ConfirmDialog } from "./ConfirmDialog";
import type { SessionRow } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { MobileCard } from "./MobileCard";
import { MobileSettings } from "./MobileSettings";
import { applyOutline, rememberLayout } from "./mobileDebug";
import { installSafeInsets } from "./mobileInsets";
import { parseSessionTarget } from "./mobileRoute";
import { loadMobileTextSize, storeMobileTextSize, type MobileTextSize } from "./mobileText";
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

/** 清理跑完的一句话（与桌面 `Sidebar.clearSummary` 同一句文案；手机面板不依赖桌面组件，所以就地一份）。 */
function clearSummary(r: { removed: number; skipped: number; failed: number }): string {
  const parts = [`已清理 ${r.removed} 行`];
  if (r.skipped > 0) parts.push(`跳过 ${r.skipped} 行（节点离线）`);
  if (r.failed > 0) parts.push(`失败 ${r.failed} 行`);
  return parts.join("，");
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
  // 手机版的一键清理（agora-off0）：与桌面同一条链——对象是「已完成」段里的全部行（`sectionOf === "finished"`，
  // 与 Sidebar.tsx 的 clearable 同一判据）、逐行 DELETE /api/sessions/:id（MISSION §11 不引入批量端点）、
  // 跳过 peer stale 的行（一跳转发到不了）。文案与桌面有意差一处（2026-10-08 口径修正时定）：手机报
  // **实际会删的行数**（去掉 stale），桌面确认框报的是 clearable.length（含 stale，属已知小瑕）；
  // 「其中 N 行是 agora 起的会话」也只从实际会删的行里数——stale 的行根本不会被删，不该被算进去。
  const clearable = sections.finished;
  const deletable = clearable.filter((r) => !r.stale);
  const ownClearable = deletable.filter((r) => !isHandleless(r)).length;
  const [clearAsk, setClearAsk] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [clearNote, setClearNote] = useState<string | null>(null);
  useEffect(() => {
    if (clearNote === null) return;
    const t = setTimeout(() => setClearNote(null), 8000);
    return () => clearTimeout(t);
  }, [clearNote]);
  async function clearFinished() {
    setClearAsk(false);
    setClearing(true);
    const result = { removed: 0, skipped: 0, failed: 0 };
    for (const r of clearable) {
      if (r.stale) {
        result.skipped += 1;
        continue;
      }
      const w = await api.deleteMetadata(r.id);
      if (w.ok) result.removed += 1;
      else result.failed += 1;
    }
    setClearing(false);
    setClearNote(clearSummary(result));
  }

  // 「新建」：每次打开重新拉一遍列表（终端里刚加 / 删的预设下次打开就是新的），拉失败把节点那句话
  // 留在屏上（"正在读…"同时收掉，不留一个停不下来的转圈）。
  async function openNew() {
    setNewOpen(true);
    setPresets(null);
    setPresetError(null);
    const r = await api.presets();
    if (r.ok) setPresets(r.value.presets);
    else if (!r.needsConfirmation) setPresetError(r.error.message);
  }

  // 点一条即起：**不要第二段确认**——预设本身就是"预先批准"，确认反倒与"少点几下"的初衷相悖
  // （epic agora-hxva 的设计要点，2026-10-08）。失败留在原屏 + 节点给的中文错误（未知预设 /
  // 目录不存在 / 起不来），不落卡片、不猜。
  async function startPreset(preset: PresetInfo) {
    if (starting !== null) return;
    setStarting(preset.name);
    setPresetError(null);
    const r = await api.create({ preset: preset.name });
    if (!r.ok) {
      setStarting(null);
      setPresetError(r.needsConfirmation ? "节点要求确认，但起会话没有确认语义；重试一次" : r.error.message);
      return;
    }
    // 成功：**留在这一屏等行到**（201 先于 `session_created`，立刻关屏会先闪一下收件箱，那不像
    // "跳到卡片"）。按钮上保持"正在起…"，pendingNew 的 effect 一行到就关屏落卡片。
    setPendingNew(r.value.id);
  }
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 新建一屏（agora-prdg.4；epic agora-hxva）：收件箱 Header →「新建」→ 预设按钮 → 点一下直接起。
  // **零打字**：这一屏只有按钮（名称 / agent 徽标 / 目录 / 参数摘要 / 首句），没有任何 input /
  // textarea——DOM 守卫在 MobileApp.test.tsx。这是 agora-uqpi 的拍板：手机要能"开始一件事"，
  // 但"能起什么"冻结在桌面侧的 CLI 预设里，被临时拿到的手机只能选已经批准过的那几条。
  const [newOpen, setNewOpen] = useState(false);
  // null = 还没拉到（打开时才拉，终端里改了预设下次打开就是新的）。
  const [presets, setPresets] = useState<PresetInfo[] | null>(null);
  const [presetError, setPresetError] = useState<string | null>(null);
  // 正在起的那条预设名：在途期间禁用整列，别让连点起出两条（与桌面 shell 按钮的 in-flight 同一条规矩）。
  const [starting, setStarting] = useState<string | null>(null);
  // 起成功但新行还没进列表（201 先于 `session_created` 到达，与 Workspace 的 pendingOpen 同一条 race）：
  // 行一到就选中它。不在这里乐观插行——列表的真相在事件流。
  const [pendingNew, setPendingNew] = useState<string | null>(null);
  // 字号档位（agora-x70t.xsgz）：存 localStorage，`data-text` 是它与 CSS 的接口（--m-fs 一族）。
  const [textSize, setTextSize] = useState<MobileTextSize>(() => loadMobileTextSize());
  const changeTextSize = useCallback((size: MobileTextSize) => {
    setTextSize(size);
    storeMobileTextSize(size);
  }, []);
  // 推送点击进来的那一行要不要把焦点送进 composer（只有可发送的行要；打开一次即消费）。
  const [focusComposerFor, setFocusComposerFor] = useState<string | null>(null);
  // 深链只自动打开一次：找到那一行才置位，打开过之后返回收件箱不再把同一行弹出来（agora-thc.10）。
  const deepLinked = useRef(false);
  useEffect(() => {
    if (deepLinked.current) return;
    // 冷启动时 URL 先到、列表后到（推送打开的就是刚起的会话，第一份快照可能还没有它）：
    // 找不到**不置位也不静默**，等 rows 随快照 / 事件变化后这个 effect 再跑一遍，找到才开
    // （agora-f068；5c745f9 起就是“找到才置位”，这条守卫钉住别退成一次性尝试）。
    if (!targetRow) return;
    deepLinked.current = true;
    setSelected(targetRow.id);
    // 可发送的行（turn_done / idle）把焦点放进 composer——推送点开就是要你回话（ux.md）。
    if (targetRow.status === "turn_done" || targetRow.status === "idle") setFocusComposerFor(targetRow.id);
  }, [targetRow]);
  useEffect(() => {
    // 选中的行没了（被删 metadata）：回到收件箱。
    if (selected !== null && !rows.some((r) => r.id === selected)) setSelected(null);
  }, [rows, selected]);
  useEffect(() => {
    // 预设起的会话到了：关掉新建屏、落在它的卡片；可发送（turn_done / idle）时把焦点放进卡片现成的
    // composer，第一句在那儿说（预设带首句时已经在启动命令行上发过了，这里只管没带首句的那些）。
    if (pendingNew === null || !rows.some((r) => r.id === pendingNew)) return;
    setNewOpen(false);
    setStarting(null);
    setSelected(pendingNew);
    setFocusComposerFor(pendingNew);
    setPendingNew(null);
  }, [rows, pendingNew]);
  const nowSeconds = now ?? clock;
  const selectedRow = selected === null ? undefined : rows.find((r) => r.id === selected);

  // 安全区自适应（agora-xu12）：主屏 PWA 全屏时 env(safe-area-inset-*) 要照加；但浏览器里
  // 或状态栏样式被改成非 translucent 时，系统已经把 chrome 让出去了，再加一遍就是凭空多出 62px。
  useEffect(() => {
    installSafeInsets();
  }, []);
  // 收件箱是"问题现场"，而设置屏会把它卸载掉：挂载 / 行变化时抓一份布局快照，设置页里复制的
  // 报告因此带着收件箱的盒子数字（agora-xu12 第二轮）。描边状态也在这里按存储值应用。
  useEffect(() => {
    if (selectedRow || settingsOpen) return;
    rememberLayout();
    applyOutline();
  }, [rows, selectedRow, settingsOpen]);

  if (selectedRow) {
    return (
      <main className="mobile" data-testid="mobile-inbox" data-text={textSize}>
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
        textSize={textSize}
        onTextSize={changeTextSize}
        onClose={() => setSettingsOpen(false)}
        onRevoked={onRevoked ?? (() => {})}
      />
    );
  }
  if (newOpen) {
    return (
      <main className="mobile" data-testid="mobile-inbox" data-text={textSize}>
        <header className="mobile-top">
          <button
            type="button"
            className="mobile-back"
            data-testid="mobile-new-back"
            aria-label="返回收件箱"
            onClick={() => {
              // 不等了：会话已经在那台机器上起了（不管发没发出去，节点都会把它放进列表），只是
              // 不再自动弹卡片；回收件箱点那一行就是。
              setNewOpen(false);
              setStarting(null);
              setPendingNew(null);
            }}
          >
            ←
          </button>
          <h1>新建</h1>
        </header>
        <div className="mobile-inbox" data-testid="mobile-preset-screen">
          {presetError !== null && (
            <p className="mobile-error" data-testid="mobile-preset-error" role="alert">
              {presetError}
            </p>
          )}
          {presets === null ? (
            presetError === null && (
              <p className="mobile-note" data-testid="mobile-preset-loading">
                正在读预设…
              </p>
            )
          ) : presets.length === 0 ? (
            /* 空态也是零打字：给一句照抄 CLI 用法的指引，不在手机上开表单（agora-prdg.4）。 */
            <p className="mobile-empty" data-testid="mobile-preset-empty">
              还没有预设：在终端跑 <code>agora preset add &lt;名字&gt; --agent &lt;agent&gt; --dir &lt;目录&gt;</code>
              加一条
            </p>
          ) : (
            <ul className="mobile-presets" data-testid="mobile-preset-list">
              {presets.map((preset) => (
                <PresetButton
                  key={preset.name}
                  preset={preset}
                  starting={starting === preset.name}
                  disabled={starting !== null}
                  onStart={startPreset}
                />
              ))}
            </ul>
          )}
        </div>
      </main>
    );
  }

  const renderRow = (row: SessionRow) => (
    <MobileRow key={row.id} row={row} now={nowSeconds} localNode={localNode} onOpen={setSelected} />
  );

  return (
    <main className="mobile" data-testid="mobile-inbox" data-text={textSize}>
      <header className="mobile-top">
        <h1>agora</h1>
        <span className="mobile-carrier" data-testid="mobile-carrier" data-reachable={String(nodes.reachable === true)}>
          承载节点 {localNode ?? "…"} {nodes.reachable === false ? "○" : "●"}
        </span>
        <button type="button" className="mobile-new" data-testid="mobile-new-open" onClick={() => void openNew()}>
          新建
        </button>
        <button type="button" className="mobile-gear" data-testid="mobile-settings-open" onClick={() => setSettingsOpen(true)}>
          设置
        </button>
      </header>
      <div className="mobile-inbox">
        {/* 清理结果（含跳过 / 失败）画在收件箱顶上：行全删完时「已完成」段整个消失，画在段里就跟着没了。 */}
        {clearNote !== null && (
          <p className="mobile-note" data-testid="mobile-clear-note" role="status">
            {clearNote}
          </p>
        )}
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
            <div className="mobile-finished-head">
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
              {/* 入口只在有「已完成」行时出现，与段一起；报的数 = 实际会删的行数（去掉 stale）。 */}
              <button
                type="button"
                className="mobile-finished-clear"
                data-testid="mobile-clear-finished"
                disabled={deletable.length === 0 || clearing}
                title={
                  deletable.length === 0
                    ? "这些行都在离线的节点上，现在删不了（等节点上线再来）"
                    : "删掉「已完成」里的记录，不 kill 进程"
                }
                onClick={() => setClearAsk(true)}
              >
                {clearing ? "清理中…" : `清理 ${deletable.length} 行`}
              </button>
            </div>
            {finishedOpen && <ul>{sections.finished.map(renderRow)}</ul>}
          </section>
        )}
      </div>
      {clearAsk && (
        <ConfirmDialog
          title="清理「已完成」里的行？"
          body={`将删除「已完成」区里 ${deletable.length} 行的记录${ownClearable > 0 ? `（其中 ${ownClearable} 行是 agora 起的会话，它们已退出的运行时会话与输出会一并清掉）` : ""}。只删记录、不 kill；「需要我」里的行不动。不可撤销。`}
          confirmLabel={`删除 ${deletable.length} 行`}
          onConfirm={() => void clearFinished()}
          onCancel={() => setClearAsk(false)}
        />
      )}
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

interface PresetProps {
  preset: PresetInfo;
  starting: boolean;
  disabled: boolean;
  onStart: (preset: PresetInfo) => void;
}

/**
 * 预设按钮（agora-prdg.4）：名称 + agent 徽标 + 目录名 + 参数摘要（如 `--model opus`）+ 有固定
 * 首句时显示一句。整屏没有一个输入控件——"零打字"是这一屏的验收（DOM 守卫在 MobileApp.test.tsx）。
 */
function PresetButton({ preset, starting, disabled, onStart }: PresetProps) {
  const badge = agentBadge(preset.agent_type);
  // 目录只显示末段（一行放得下）；完整路径在 title 里，悬停 / 长按能看到。
  const dir = preset.working_directory.split("/").filter(Boolean).pop() ?? preset.working_directory;
  const args = (preset.args ?? "").trim();
  const prompt = (preset.prompt ?? "").split("\n").find((l) => l.trim() !== "")?.trim() ?? "";
  return (
    <li>
      <button
        type="button"
        className="mobile-preset"
        data-testid={`mobile-preset-${preset.name}`}
        disabled={disabled}
        onClick={() => onStart(preset)}
      >
        <span className="mobile-preset-head">
          <span className="mobile-preset-name">{preset.name}</span>
          <span className="mobile-agent" style={{ "--hue": badge.hue } as CSSProperties}>
            {badge.glyph} {badge.label}
          </span>
        </span>
        <span className="mobile-preset-dir" title={preset.working_directory}>
          {dir}
        </span>
        {args !== "" && <span className="mobile-preset-args">{args}</span>}
        {prompt !== "" && <span className="mobile-preset-prompt">{prompt}</span>}
        {starting && (
          <span className="mobile-preset-starting" data-testid={`mobile-preset-starting-${preset.name}`}>
            正在起…
          </span>
        )}
      </button>
    </li>
  );
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
        {/* 第一行只有两个锚点：名字（flex:0 1 auto，装不下才省略）与状态（flex:none，永不让位——
            "waiting 3m" 是这一行存在的理由）。徽标与节点在下一行的开头，跟任务文本一起排：
            发信人 + 内容，也是 IM 里最熟的那种一行（agora-nzbu）。 */}
        <span className="mobile-row-head">
          <span className="mobile-symbol" aria-hidden="true">
            {statusSymbol(row.status)}
          </span>
          <span className="mobile-row-name">{rowName(row)}</span>
          <span className="mobile-status">{statusLine(row, now)}</span>
        </span>
        <span className="mobile-row-sub">
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
          <span className="mobile-row-task">{taskLabel(row)}</span>
        </span>
        {summary !== "" && <span className="mobile-row-summary">{summary}</span>}
      </button>
    </li>
  );
}
