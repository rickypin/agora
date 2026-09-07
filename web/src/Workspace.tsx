import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { catalogApi, sessionApi, type CatalogApi, type SessionApi } from "./api";
import { partitionByAttention, sortByAttention } from "./attention";
import { ChangesApiContext } from "./Changes";
import { CommandPalette } from "./CommandPalette";
import { fuzzyFilter } from "./fuzzy";
import { nodeStatuses } from "./Header";
import { HealthWatcher, versionBlocked, VersionWatcher } from "./health";
import { isDesktop, matchShortcut } from "./keys";
import { NewAgentDialog } from "./NewAgentDialog";
import { browserDeps, Notifier, type NotifierDeps, type Permission } from "./notify";
import { Respond } from "./Respond";
import { SessionSettings } from "./SessionSettings";
import { rowHaystack, rowName, Sidebar } from "./Sidebar";
import { SessionStore, useSessions, useUnregistered } from "./store";
import { defaultDiffSocket, type TerminalClientOptions } from "./terminal";
import { TerminalView } from "./TerminalView";

/**
 * 主区显示什么：侧栏 active 行的终端，或它的只读 diff（agora-h1k.5）。页面只有一个选中集合——侧栏的
 * active 行；顶栏标签页曾是第二个选中集合（标签页 active 管主区、行 active 管展开区，二者可不一致），
 * 用户 2026-09-07 反馈"只有左侧栏时很容易上手，加上顶栏就不知道该如何操作"，去掉（agora-a46）。
 * 这是纯视图状态：改它只挂 / 卸载 TerminalView，绝不发请求——切行 / 关闭终端视图 = Detach，
 * 不 restart / recreate / kill（MISSION §4.6；不变量 4；守卫 Workspace.test.tsx 的 A20 一条）。
 */
interface View {
  id: string;
  kind: "terminal" | "diff";
}

interface Props {
  store?: SessionStore;
  api?: SessionApi;
  catalog?: CatalogApi;
  /** 测试注入：侧栏行渲染计数。 */
  onRowRender?: (id: string) => void;
  /** 测试注入：浏览器通知的构造器与权限。 */
  notifyDeps?: NotifierDeps;
  /** 测试注入：`/api/health` 的 runtime 段观察者。 */
  health?: HealthWatcher;
  /** 测试注入：`/api/system` 的 api_version 比对。 */
  version?: VersionWatcher;
  /** 测试注入：终端 WS 的建法（透传给 TerminalView）。 */
  terminalConnect?: TerminalClientOptions["connect"];
  /** 测试注入：只读 diff 终端 WS 的建法；缺省 `defaultDiffSocket`（agora-h1k.5）。 */
  diffConnect?: TerminalClientOptions["connect"];
  /** 事件流被服务端以 4401 关掉（本设备被吊销，agora-0jt）：App 换回配对门。 */
  onRevoked?: () => void;
}

/** Screen A 的侧栏（Attention Dashboard）+ Screen B：侧栏 active 行的终端 + Session Settings（无顶栏标签页，agora-a46）。 */
export function Workspace({ store: given, api: givenApi, catalog: givenCatalog, onRowRender, notifyDeps, health: givenHealth, version: givenVersion, terminalConnect, diffConnect, onRevoked }: Props) {
  const store = useMemo(() => given ?? new SessionStore(), [given]);
  const api = useMemo(() => givenApi ?? sessionApi(), [givenApi]);
  const catalog = useMemo(() => givenCatalog ?? catalogApi(), [givenCatalog]);
  const rows = useSessions(store);
  const unregistered = useUnregistered(store);
  // 运行时 degraded（ADR-001 D7）：没有它，用户看到的是一屋子 UNKNOWN 却不知道是运行时失明（agora-bgr）。
  // 只在配对之后（Workspace 才挂）带 cookie 拉完整报告；未认证的门页只用公开子集。
  const health = useMemo(() => givenHealth ?? new HealthWatcher(), [givenHealth]);
  const degraded = useSyncExternalStore(health.subscribe, health.snapshot, health.snapshot);
  // Header 的节点状态与 degraded 同一次拉取带出，不另起轮询（MISSION §10.3；agora-7ku.12）；peer 的翻转由下面的 onPeerChanged 秒级推进来（agora-c8h）。
  const nodesHealth = useSyncExternalStore(health.subscribeNodes, health.nodesSnapshot, health.nodesSnapshot);
  useEffect(() => {
    health.start();
    return () => health.stop();
  }, [health]);
  // API 版本（MISSION §7.3；agora-7ku.4）：节点各自原地升级，这个标签页可能是升级前留下的旧页面。
  // 不轮询——挂在 /api/events 每次连上的那一刻比一次（升级必然重启 daemon、WS 必然断一次）。
  // 这个 effect 要排在 store.start() 的那个之前：onOpen 得在首连之前装好。
  const version = useMemo(() => givenVersion ?? new VersionWatcher(), [givenVersion]);
  const verdict = useSyncExternalStore(version.subscribe, version.snapshot, version.snapshot);
  const blocked = versionBlocked(verdict);
  // 本机 node.id 随同一次 /api/system 来（agora-7ku.5）：Header 本机那一枚的名字、侧栏行标不标 `@ node` 都看它。
  const localNode = useSyncExternalStore(version.subscribe, version.nodeSnapshot, version.nodeSnapshot);
  const nodes = useMemo(() => nodeStatuses(nodesHealth, localNode), [nodesHealth, localNode]);
  // 断流期间的 peer 翻转补不回来（事件是增量）：重连时顺手重拉一次 health 对齐 Header；首连不拉——
  // 上面 health.start() 刚拉过（agora-c8h）。
  const opened = useRef(false);
  useEffect(() => {
    store.onOpen = () => {
      void version.check();
      if (opened.current) void health.refresh();
      opened.current = true;
    };
    return () => {
      store.onOpen = null;
    };
  }, [store, version, health]);
  // peer 上线 / 掉线走事件流，Header 的点与侧栏行的变灰同一眼看到（agora-c8h）。
  useEffect(() => {
    store.onPeerChanged = (name, peer) => health.applyPeer(name, peer);
    return () => {
      store.onPeerChanged = null;
    };
  }, [store, health]);
  useEffect(() => {
    store.onRevoked = onRevoked ?? null;
    return () => {
      store.onRevoked = null;
    };
  }, [store, onRevoked]);
  const [view, setView] = useState<View | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newAgentOpen, setNewAgentOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const filterInput = useRef<HTMLInputElement>(null);
  // 刚创建的会话：等它随事件流进列表再选中。POST 的响应先于 `session_created` 到达，
  // 这时就选中会被下面"行没了就清空"的 effect（列表里还没有这一行）立刻清掉。
  const [pendingOpen, setPendingOpen] = useState<string | null>(null);

  // 点已经 active 的那一行：view 不变、TerminalView 不重挂、不会再 focus，焦点就留在刚点的按钮上——
  // 敲键盘什么都不进 pane，按 Enter 还会再点一次（agora-vcc，2026-09-05 代检）。用户的直觉是
  // "点一下这个会话就能打字"，所以命中当前 active 时显式把焦点交回终端。不在按钮的 mousedown 上
  // preventDefault：那会让侧栏失去键盘可达性。external 会话没有终端，ref 为空、不动。
  const terminalFocus = useRef<(() => void) | null>(null);
  const viewRef = useRef<View | null>(null);
  viewRef.current = view;
  const focusTerminal = useCallback(() => terminalFocus.current?.(), []);
  // 回调必须稳定：侧栏行是 memo 的，每次渲染换一个闭包会让所有行跟着重渲染。
  // 点行 = 主区显示它的终端（在看它的 diff 时也回到终端）；点别的行 = 前一个终端卸载（detach）。
  const openTab = useCallback(
    (id: string) => {
      const cur = viewRef.current;
      if (cur?.id === id && cur.kind === "terminal") {
        focusTerminal();
        return;
      }
      setView({ id, kind: "terminal" });
    },
    [focusTerminal],
  );

  // 浏览器通知（MISSION §6.6；A18）：点击回到该行——openTab 让它成为侧栏 active 行，WAITING /
  // TURN_DONE 的就地回答区就随行展开（Sidebar 只给 active 行渲染 renderExpanded）。
  const notifier = useMemo(() => new Notifier(notifyDeps ?? browserDeps(), openTab), [notifyDeps, openTab]);
  const [notifyPerm, setNotifyPerm] = useState<Permission>(() => notifier.permission());
  useEffect(() => {
    store.onNotification = (n) => notifier.show(n);
    store.start();
    return () => {
      store.onNotification = null;
      store.stop();
    };
  }, [store, notifier]);

  const byId = useMemo(() => new Map(rows.map((r) => [r.id, r])), [rows]);
  useEffect(() => {
    // 选中的会话从列表消失（被删 metadata）：主区回到空，diff 视图随它一起没了（agora-h1k.5）。
    setView((v) => (v && !byId.has(v.id) ? null : v));
  }, [byId]);
  useEffect(() => {
    if (pendingOpen && byId.has(pendingOpen)) {
      setView({ id: pendingOpen, kind: "terminal" });
      setPendingOpen(null);
    }
  }, [byId, pendingOpen]);

  const active = view ? byId.get(view.id) : undefined;
  const showDiff = view?.kind === "diff";
  const openNewAgent = useCallback(() => setNewAgentOpen(true), []);
  // 「看 diff」（MISSION §6.3 看结果；agora-h1k.5）：把主区切成该行的只读 diff——不发请求、不进会话列表，
  // 里面的 TerminalView 以 diff socket 连 `WS /api/sessions/:id/diff`，关掉 diff / 点别的行即关 WS。
  const openDiff = useCallback((id: string) => setView({ id, kind: "diff" }), []);
  // crumb 的「关闭」= Detach：主区清空、终端卸载；「关闭 diff」回到同一行的终端。都不发请求。
  const closeView = useCallback(() => setView(null), []);
  const closeDiff = useCallback(() => setView((v) => (v ? { id: v.id, kind: "terminal" } : v)), []);
  // 采纳成功：会话随 `session_created` 进列表后再开 Tab；未登记列表不走事件流，主动重拉。
  const adopt = useCallback(
    (body: Parameters<SessionApi["adopt"]>[0]) => {
      void api.adopt(body).then((r) => {
        if (r.ok) setPendingOpen(r.value.id);
        void store.client.refresh();
      });
    },
    [api, store],
  );

  // 侧栏显示顺序：先按 attention 排（MISSION §6.3），过滤只删不换序（空 query 同分稳定），
  // 再把 NEEDS ATTENTION 提到 RUNNING 前面；Alt/Option+N 跳的就是这个顺序（agora-xqa.14 验收）。
  const visible = useMemo(
    () => partitionByAttention(fuzzyFilter(sortByAttention(rows), filter, rowHaystack)),
    [rows, filter],
  );

  useEffect(() => {
    // 手机端没有键盘：全局快捷键与命令面板只在桌面装（MISSION §6.5）。
    if (!isDesktop()) return;
    const onKey = (ev: KeyboardEvent) => {
      // 对话框开着的时候方向键、Enter 归它自己；全局层让位。
      if (paletteOpen || newAgentOpen) return;
      const hit = matchShortcut(ev);
      if (!hit) return; // 终端的 Ctrl+C/D/Z/R/A/E 走这条路原样落到 pane
      ev.preventDefault();
      switch (hit.action) {
        case "palette":
          setPaletteOpen(true);
          break;
        case "filter":
          filterInput.current?.focus();
          filterInput.current?.select();
          break;
        case "new":
          setNewAgentOpen(true);
          break;
        case "next":
        case "prev": {
          if (visible.length === 0) break;
          const at = visible.findIndex((r) => r.id === view?.id);
          const step = hit.action === "next" ? 1 : -1;
          const next = visible[(at + step + visible.length) % visible.length];
          if (next) openTab(next.id);
          break;
        }
        case "jump": {
          const target = visible[hit.index];
          if (target) openTab(target.id);
          break;
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteOpen, newAgentOpen, visible, view?.id, openTab]);

  if (blocked !== null) {
    // 节点与页面不是同一个 API major（或读不出版本）：只留横幅，侧栏 / 终端一概不挂——
    // 形态可能已经变了，渲染出来的每个字段都可能是错读，空白比错读诚实（MISSION §7.3）。
    // 横幅就是 agora-bgr 的 runtime-degraded 那一条的位置与样式，不做第二套。store 与 health
    // 照常跑着：下一次 WS 重连再比一次，版本对上了页面自己回来，不需要用户做什么。
    return (
      <div className="workspace">
        <section className="main">
          <div className="runtime-degraded" data-testid="api-version-mismatch" role="alert" title={blocked}>
            ⚠ {blocked}
          </div>
        </section>
      </div>
    );
  }

  return (
    <ChangesApiContext.Provider value={api}>
    <div className="workspace">
      <Sidebar
        rows={visible}
        nodes={nodes}
        localNode={localNode ?? undefined}
        all={rows}
        total={rows.length}
        // 看 diff 时它的会话行仍是选中行（展开区留着，验收标准 / 改动列表与 diff 并排对照，MISSION §6.3）；
        // 不然行一折叠，关掉 diff 又重挂载重拉一次 /changes（2026-09-06 代检时看到）。
        active={view?.id ?? null}
        onOpen={openTab}
        onNewAgent={openNewAgent}
        onRowRender={onRowRender}
        filter={filter}
        onFilter={setFilter}
        filterRef={filterInput}
        onFilterEnter={() => {
          const first = visible[0];
          if (first) openTab(first.id);
        }}
        renderExpanded={(r) => <Respond row={r} api={api} onOpenTerminal={openTab} />}
        unregistered={unregistered}
        onAdopt={adopt}
        onOpenDiff={openDiff}
      />
      <section className="main">
        {degraded !== null && (
          <div className="runtime-degraded" data-testid="runtime-degraded" role="status" title={degraded}>
            ⚠ 运行时 degraded：{degraded}。会话状态暂不可知，进程没有被杀。
          </div>
        )}
        {notifyPerm === "default" && (
          <div className="notify-ask" data-testid="notify-ask">
            <span>agent 需要你时弹浏览器通知？</span>
            <button onClick={() => void notifier.request().then(setNotifyPerm)}>允许通知</button>
            <button onClick={() => setNotifyPerm("denied")} title="本次不问；浏览器设置里随时可开">
              以后再说
            </button>
          </div>
        )}
        {active ? (
          <>
            <div className="crumb">
              {showDiff ? (
                // diff 视图：没有 Settings——它不是会话，Kill / Restart / Rename 都不是它的事；「关闭 diff」回到终端。
                <>
                  <span data-testid="crumb-diff">git diff / {rowName(active)}</span>
                  <button onClick={closeDiff} data-testid="close-diff" title="回到这个会话的终端">
                    关闭 diff
                  </button>
                </>
              ) : (
                <>
                  <span data-testid="crumb">
                    {rowName(active)} / {String(active.agent_type ?? "")} @ {active.node}
                  </span>
                  <span className="crumb-actions">
                    <button onClick={() => setSettingsOpen((v) => !v)} aria-pressed={settingsOpen}>
                      Settings
                    </button>
                    <button onClick={closeView} data-testid="close-view" title="只关闭这个终端视图（detach）；agent 继续运行">
                      关闭
                    </button>
                  </span>
                </>
              )}
            </div>
            <div className="pane">
              {/* key=会话 id：切行时旧终端卸载（detach）、新终端挂载，永不 restart。 */}
              {showDiff ? (
                // 只读 diff 终端（agora-h1k.5）：key 与同一会话的终端不同，切走即卸载 = WS 关、
                // git 进程被收走；sessionId 给基础会话 id，URL 的 /diff 由 defaultDiffSocket 拼。
                <TerminalView
                  key={`diff:${active.id}`}
                  sessionId={active.id}
                  connect={diffConnect ?? defaultDiffSocket}
                  readOnly
                  focusRef={terminalFocus}
                />
              ) : active.origin === "external" ? (
                // external 会话没有运行时句柄（MISSION §5.5）：只有状态与 hook 的 respond，没有终端可挂。
                <p className="muted empty" data-testid="no-terminal">
                  external 会话：agora 没有它的终端，只能看状态、经 hook 回答；要操作请去它自己的窗口。
                </p>
              ) : (
                <TerminalView key={active.id} sessionId={active.id} connect={terminalConnect} focusRef={terminalFocus} />
              )}
              {settingsOpen && !showDiff && <SessionSettings row={active} api={api} onClose={() => setSettingsOpen(false)} />}
            </div>
          </>
        ) : (
          <p className="muted empty">{rows.length ? "从左侧选一个 agent。" : "还没有会话。"}</p>
        )}
      </section>
      {newAgentOpen && (
        <NewAgentDialog
          api={api}
          catalog={catalog}
          nodes={nodes}
          onClose={() => setNewAgentOpen(false)}
          onCreated={(id) => setPendingOpen(id)}
        />
      )}
      {paletteOpen && (
        <CommandPalette
          rows={rows}
          api={api}
          catalog={catalog}
          nodes={nodes}
          onOpen={openTab}
          onNewAgent={openNewAgent}
          onCreated={(id) => setPendingOpen(id)}
          onClose={() => setPaletteOpen(false)}
        />
      )}
    </div>
    </ChangesApiContext.Provider>
  );
}
