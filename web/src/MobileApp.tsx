/**
 * 手机交互收件箱 /m（MISSION §6.9；A37 / A52；agora-thc.5；会话卡 agora-thc.10）。
 *
 * 这一屏只回答一件事：**现在谁在等我**。四段与排序复用 attention.ts（与桌面同一条规则、同一个
 * 「看过」集合）。移动工作台提供概览、关注筛选、搜索与任务卡片，底部导航进入预设和设置。
 * 没有终端、自由创建表单、diff / 验收 / 改动列表，也没有消息流与历史翻页（A52；DOM 守卫在 MobileApp.test.tsx）。
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
import { anchoredNow, isHandleless, loadSeen, sectionOf, seenKey, sortByAttention, storeSeen, taskLabel, type Section, type SeenSet } from "./attention";
import { ConfirmDialog } from "./ConfirmDialog";
import type { SessionRow } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { activityPhraseForProgress } from "./mobileActivity";
import { MobileIcon } from "./MobileIcon";
import { MobileCard } from "./MobileCard";
import { MobileSettings } from "./MobileSettings";
import { applyOutline, rememberLayout } from "./mobileDebug";
import { installSafeInsets } from "./mobileInsets";
import { parseSessionTarget } from "./mobileRoute";
import { mobileStatusLine } from "./mobileStatus";
import { loadMobileTextSize, storeMobileTextSize, type MobileTextSize } from "./mobileText";
import { browserPushEnv, selfCheckPush, type PushEnv } from "./push";
import { nodeHue } from "./nodeColor";
import { identityName, rowName, statusSymbol, str } from "./SessionRow";
import { SessionStore, useServerClock, useSessions } from "./store";

/** 前三段固定顺序（与 attention.ts 的 partitionByAttention 同一顺序）；已完成是折叠的一段。
 *  段名回答的是「要不要我管」，不是「在不在跑」：working 段装的是**一切不需要你的行**——running /
 *  starting / idle，以及看过一次的 turn_done（agora-5gg.21 的降段）。用户点开一行只是记了「看过」，
 *  行没在跑却显示成「在跑」，正是 2026-10-08 真机反馈的第四条（agora-o975.4）。 */
const OPEN_SECTIONS: { key: Section; label: string }[] = [
  { key: "attention", label: "需要我" },
  { key: "unclear", label: "状态待确认" },
  { key: "working", label: "暂无需处理" },
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

/** 一行摘要：pending_decision.summary > progress > detail > 人可读 reason 的第一行，80 字封顶（docs/spec/ux.md 的信息预算）。 */
export function mobileSummary(row: SessionRow): string {
  // Structured reasons are state codes, not message previews. Keep human-readable legacy reasons.
  const reason = str(row.reason);
  const pending = str(row.pending_decision?.summary);
  // 在跑的行：活动层给的 `progress` 是工具名（bash/read），先说成人话再进摘要——列表上「bash」
  // 不是一个能读的进展（2026-10-10 真机反馈；与卡片的活动行共用 `mobileActivity` 那份文案）。
  // 还没活动的在跑行 progress 为空，落到下面用 detail / prompt 继续说明它在做的那件事。
  if (pending === "" && (row.status === "running" || row.status === "starting")) {
    const phrase = activityPhraseForProgress(str(row.progress));
    if (phrase !== null) return phrase;
  }
  const raw = pending || str(row.progress) || str(row.detail) || (/^[a-z_]+$/.test(reason) ? "" : reason);
  const line = raw.split("\n").find((l) => l.trim() !== "")?.trim().replace(/^#{1,6}\s+/, "") ?? "";
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
  // 行上时长以节点钟为基准（agora-au5）：手机与节点没校时是最常见的现场，用页面钟减 status_since
  // 会把钟差整体加在时长上；serverClock 缺省（老节点 / 还没拉到快照）时退回页面钟。
  const serverClock = useServerClock(store);
  const health = useMemo(() => givenHealth ?? new HealthWatcher(), [givenHealth]);
  const version = useMemo(() => givenVersion ?? new VersionWatcher(), [givenVersion]);
  const localNode = useSyncExternalStore(version.subscribe, version.nodeSnapshot, version.nodeSnapshot);
  const nodes = useSyncExternalStore(health.subscribeNodes, health.nodesSnapshot, health.nodesSnapshot);
  const nodeWarning = useSyncExternalStore(health.subscribe, health.snapshot, health.snapshot);
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
  // Drafts stay in this page's memory only, so switching sessions never loses an unfinished reply.
  const drafts = useRef<Record<string, string>>({});
  const inboxScroll = useRef(0);
  const inboxRef = useRef<HTMLDivElement>(null);
  const openRow = (id: string) => {
    inboxScroll.current = inboxRef.current?.scrollTop ?? 0;
    const url = new URL(window.location.href);
    url.searchParams.set("session", id);
    window.history.pushState({ agoraMobileCard: true }, "", url);
    setSelected(id);
  };
  const closeCard = () => {
    setSelected(null);
    if (window.history.state?.agoraMobileCard) window.history.back();
    else {
      const url = new URL(window.location.href);
      url.searchParams.delete("session");
      window.history.replaceState(null, "", url);
    }
  };
  useEffect(() => {
    const restore = () => {
      const value = new URL(window.location.href).searchParams.get("session");
      setSelected(value && rows.some((r) => r.id === value) ? value : null);
    };
    window.addEventListener("popstate", restore);
    return () => window.removeEventListener("popstate", restore);
  }, [rows]);
  useEffect(() => {
    if (selected === null && inboxRef.current) inboxRef.current.scrollTop = inboxScroll.current;
  }, [selected]);
  const [finishedOpen, setFinishedOpen] = useState(false);
  const [filter, setFilter] = useState<"all" | "attention" | "working">("all");
  const [query, setQuery] = useState("");
  const matches = (row: SessionRow) => `${rowName(row)} ${taskLabel(row)} ${row.node} ${row.agent_type} ${row.project?.name ?? ""} ${row.project?.branch ?? ""} ${mobileSummary(row)}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase());
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
  // 新建一屏（agora-prdg.4；epic agora-hxva）：收件箱底部导航 →「新建」→ 预设按钮 → 点一下直接起。
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
  // 锚定要拿**当下的墙钟**，不能拿 `clock`（它是每 30 s 走一格的渲染信号）——否则锚定值最多落后
  // 30 s，卡片心跳从这个偏低的起点往上加，刚进在跑的行头几秒被夹成 `0s`（agora-o975.5，2026-10-08
  // 真机实测：节点侧已过 5 s、页面还写 0s）。`clock` 仍留着当重渲染的信号。
  const nowSeconds = now ?? (serverClock ? anchoredNow(serverClock, Math.floor(Date.now() / 1000)) : clock);
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
          key={selectedRow.id}
          row={selectedRow}
          api={api}
          initialDraft={drafts.current[selectedRow.id] ?? ""}
          onDraftChange={(text) => { drafts.current[selectedRow.id] = text; }}
          onNext={sections.attention.some((r) => r.id !== selectedRow.id) ? () => {
            const id = sections.attention.find((r) => r.id !== selectedRow.id)!.id;
            const url = new URL(window.location.href);
            url.searchParams.set("session", id);
            window.history.replaceState(window.history.state, "", url);
            setSelected(id);
          } : undefined}
          now={nowSeconds}
          onBack={closeCard}
          onSeen={markSeen}
          seen={seen.has(seenKey(selectedRow))}
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
          <h1>开始一件事</h1>
        </header>
        <div className="mobile-inbox" data-testid="mobile-preset-screen">
          <div className="mobile-page-intro"><span className="mobile-eyebrow">准备就绪，轻点开始</span><h2>选择一个预设</h2><p>在节点上启动，随时回来查看进展。</p></div>
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
    // 「已读」是本设备的 seen 集合算出来的（MISSION §4.6 证据 ①）：记号跟着「这一次完成」走。
    <MobileRow key={row.id} row={row} now={nowSeconds} localNode={localNode} seen={seen.has(seenKey(row))} onOpen={openRow} />
  );

  return (
    <main className="mobile" data-testid="mobile-inbox" data-text={textSize}>
      <header className="mobile-top mobile-home-head">
        <div className="mobile-brand"><span className="mobile-brand-mark" aria-hidden="true">a</span><h1>agora</h1></div>
        <span className="mobile-carrier" data-testid="mobile-carrier" data-reachable={String(nodes.reachable === true)}>
          <span aria-hidden="true">●</span> {localNode ?? "连接中"} · {nodes.reachable === false ? "离线" : nodes.reachable === true ? "已连接" : "连接中"}
        </span>
      </header>
      <div className="mobile-inbox mobile-home-inbox" ref={inboxRef}>
        {nodeWarning && <p className="mobile-note warning" role="status" data-testid="mobile-node-warning">{nodeWarning}</p>}
        <section className="mobile-overview" aria-label="会话概览">
          <span className="mobile-eyebrow">你的移动工作台</span>
          <h2>{sections.attention.length > 0 ? <>有 <strong>{sections.attention.length}</strong> 件事需要你</> : "暂时没有待办"}</h2>
          <p>{sections.attention.length > 0 ? "看看结果，给个决定，让工作继续。" : "工作留在节点上，进展随时在这里。"}</p>
          <div className="mobile-overview-meta"><span>{rows.length} 个会话</span><span>{sections.working.length} 个暂无需处理</span></div>
        </section>
        {nodes.reachable === false && <p className="mobile-note warning" role="status">暂时连不上节点，当前显示上次收到的状态。</p>}
        <div className="mobile-filter" role="group" aria-label="筛选会话">
          <button type="button" aria-pressed={filter === "all"} onClick={() => setFilter("all")}>全部</button>
          <button type="button" aria-pressed={filter === "attention"} onClick={() => setFilter("attention")}>需要我 <span>{sections.attention.length}</span></button>
          <button type="button" aria-pressed={filter === "working"} onClick={() => setFilter("working")}>无需处理</button>
        </div>
        <label className="mobile-search"><MobileIcon name="search"/><input aria-label="搜索会话" placeholder="搜索任务、项目或节点" value={query} onChange={(e) => setQuery(e.target.value)}/>{query && <button type="button" aria-label="清除搜索" onClick={() => setQuery("")}>×</button>}</label>
        {query.trim() && !Object.entries(sections).some(([key, items]) => (filter === "all" || key === filter) && items.some(matches)) && <p className="mobile-empty" role="status">没有匹配的会话，试试其他关键词。</p>}
        {/* 清理结果（含跳过 / 失败）画在收件箱顶上：行全删完时「已完成」段整个消失，画在段里就跟着没了。 */}
        {clearNote !== null && (
          <p className="mobile-note" data-testid="mobile-clear-note" role="status">
            {clearNote}
          </p>
        )}
        {OPEN_SECTIONS.filter(({ key }) => filter === "all" || key === filter).map(({ key, label }) => (
          <section key={key} className="mobile-section" data-testid={`mobile-section-${key}`} aria-label={label}>
            <h2>
              <span>{label}</span>
              <span>{sections[key].filter(matches).length}</span>
            </h2>
            {sections[key].filter(matches).length === 0
              ? key === "attention" && !query && <div className="mobile-empty"><MobileIcon name="check"/><p>没有等你的事</p><span>可以安心离开，有新进展再回来。</span></div>
              : <ul>{sections[key].filter(matches).map(renderRow)}</ul>}
          </section>
        ))}
        {filter === "all" && sections.finished.length > 0 && (
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
            {(finishedOpen || query.trim() !== "") && <ul>{sections.finished.filter(matches).map(renderRow)}</ul>}
          </section>
        )}
      </div>
      <nav className="mobile-nav" aria-label="主导航">
        <button type="button" aria-current="page" onClick={() => { setFilter("all"); setQuery(""); }}><MobileIcon name="inbox"/><span>会话</span></button>
        <button type="button" data-testid="mobile-new-open" onClick={() => void openNew()}><MobileIcon name="plus"/><span>新建</span></button>
        <button type="button" data-testid="mobile-settings-open" onClick={() => setSettingsOpen(true)}><MobileIcon name="settings"/><span>设置</span></button>
      </nav>
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
  /** 本设备看过这一行了吗（`seen.has(seenKey(row))`）：turn_done 的「待查看 / 已读」靠它。 */
  seen: boolean;
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
            <span className="mobile-agent-glyph" aria-hidden="true">{badge.label.slice(0, 1).toUpperCase()}</span> {badge.label}
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

function MobileRow({ row, now, localNode, seen, onOpen }: RowProps) {
  const badge = agentBadge(String(row.agent_type ?? ""));
  const local = localNode !== null && row.node === localNode;
  const summary = mobileSummary(row);
  // 手机端的词（等你 / 待查看 / 已读 / 在跑…），不是桌面那份英文（agora-o975.4）。
  // 时长用分钟粒度（agora-o975.3 审查修订）：列表 30 s 才走一格，显示秒会“冻住”；秒级只留给
  // 有 1 s 心跳的卡片。
  const status = mobileStatusLine(row, seen, now, "minutes");
  // 在跑 / 启动中的符号脉冲（agora-o975.3）：与卡片同一个 .mobile-symbol.live、同一条 CSS。
  const live = row.status === "running" || row.status === "starting";
  return (
    <li>
      <button
        type="button"
        className={`mobile-row st-${row.status}${row.stale ? " mobile-row-stale" : ""}`}
        data-testid={`mobile-row-${row.id}`}
        onClick={() => onOpen(row.id)}
      >
        {/* Identity/status stay separate from the task title; agent and node follow the preview. */}
        <span className="mobile-row-head">
          <span className={`mobile-symbol${live ? " live" : ""}`} aria-hidden="true">
            {statusSymbol(row.status)}
          </span>
          <span className="mobile-row-name" data-testid={`mobile-identity-${row.id}`}>{identityName(row)}</span>
          <span className="mobile-status">{status}</span>
        </span>
        <span className="mobile-row-sub">
          <span className="mobile-agent" style={{ "--hue": badge.hue } as CSSProperties}>
            <span className="mobile-agent-glyph" aria-hidden="true">{badge.label.slice(0, 1).toUpperCase()}</span> {badge.label}
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
        </span>
        <span className="mobile-row-task">{taskLabel(row) || rowName(row)}</span>
        {summary !== "" && <span className="mobile-row-summary">{summary}</span>}
        <span className="mobile-row-footer"><span>{row.stale ? "节点离线 · 保留上次状态" : row.status === "waiting" ? "查看请求" : row.status === "turn_done" ? "阅读回复" : "查看会话"}</span><MobileIcon name="arrow"/></span>
      </button>
    </li>
  );
}
