import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { catalogApi, sessionApi, type CatalogApi, type SessionApi } from "./api";
import { loadSeen, sectionOf, seenKey, storeSeen, type Section } from "./attention";
import { ChangesApiContext } from "./Changes";
import { CommandPalette } from "./CommandPalette";
import { nodeStatuses } from "./Header";
import { HealthWatcher, versionBlocked, VersionWatcher } from "./health";
import { isDesktop, matchShortcut } from "./keys";
import { NewAgentDialog, type NewAgentInitial } from "./NewAgentDialog";
import { browserDeps, Notifier, type NotifierDeps, type Permission } from "./notify";
import { hasRespondPanel, RespondPanel } from "./RespondPanel";
import { RowResult } from "./RowResult";
import { SessionSettings } from "./SessionSettings";
import { loadMode, storeMode, visibleOrder, type SidebarMode } from "./sidebarMode";
import { rowName, Sidebar } from "./Sidebar";
import { FREEZE_MS, stableOrder, stableSections } from "./stableOrder";
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

/** 没有一行动过：常量，省得每次渲染新造一个空集合把 memo 的下游叫醒。 */
const NO_MOVED: ReadonlySet<string> = new Set();

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
  // 把焦点放进回答面板的请求（A50，agora-4yr.1）：值是"给哪一行的面板"，不是一个裸计数——
  // 通知点击是「选中这一行」+「聚焦它的面板」同一批 state 更新，面板在那一帧才第一次挂载，
  // 裸计数在挂载那一刻分不出"刚被请求"与"上一次请求留下的旧值"。面板聚焦完回调清空，只用一次。
  const [respondFocus, setRespondFocus] = useState<string | null>(null);
  const clearRespondFocus = useCallback(() => setRespondFocus(null), []);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newAgentOpen, setNewAgentOpen] = useState(false);
  // 打开对话框时的预填（A48，agora-uvd.4）：树视图组头「+」带来的 Node / Project / Worktree；
  // 别的入口（按钮、Alt/Option+N、命令面板）不传，关掉就清空——下次打开不该还记着上次站在哪。
  const [newAgentInitial, setNewAgentInitial] = useState<NewAgentInitial | undefined>(undefined);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const filterInput = useRef<HTMLInputElement>(null);
  // 侧栏视图（A47，agora-uvd.2）：浏览器视图状态，默认 attention（MISSION §6.3 首页原则）。
  const [mode, setMode] = useState<SidebarMode>(() => loadMode());
  const applyMode = useCallback((next: SidebarMode) => {
    setMode(next);
    storeMode(next);
  }, []);
  const toggleMode = useCallback(() => {
    setMode((m) => {
      const next = m === "attention" ? "tree" : "attention";
      storeMode(next);
      return next;
    });
  }, []);
  // 「需要我」视图的重排冻结（A51，agora-4yr.4）：指针在侧栏里、或最近 3 s 内动过侧栏（点行、敲过滤框、
  // Alt/Option+N / ]/[）时顺序冻住——我在看 / 在操作的时候别动；我走开了再落位。只看指针与键盘，不看
  // 焦点：焦点常年在终端里，按焦点判会几乎永远冻着。3 s 写死，不做配置。
  const [frozen, setFrozen] = useState(false);
  const pointerInside = useRef(false);
  const thawTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clearThaw = useCallback(() => {
    if (thawTimer.current !== null) {
      clearTimeout(thawTimer.current);
      thawTimer.current = null;
    }
  }, []);
  const armThaw = useCallback(() => {
    clearThaw();
    // 定时器到点还要再看一眼指针：人可能一直悬在侧栏里只是没再按键，那时不该落位。
    thawTimer.current = setTimeout(() => {
      thawTimer.current = null;
      if (!pointerInside.current) setFrozen(false);
    }, FREEZE_MS);
  }, [clearThaw]);
  const touchSidebar = useCallback(() => {
    setFrozen(true);
    armThaw();
  }, [armThaw]);
  const enterSidebar = useCallback(() => {
    pointerInside.current = true;
    clearThaw();
    setFrozen(true);
  }, [clearThaw]);
  const leaveSidebar = useCallback(() => {
    pointerInside.current = false;
    armThaw();
  }, [armThaw]);
  useEffect(() => clearThaw, [clearThaw]);
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

  // 从侧栏点开一行：先冻住顺序再开。点行本身就是"我正在这里操作"，而它常常顺手改变排序——把上一行
  // 写进 seen（A46）就够让它掉进折叠区了。通知点击与命令面板走的是裸 openTab，不冻。
  const openFromSidebar = useCallback(
    (id: string) => {
      touchSidebar();
      openTab(id);
    },
    [openTab, touchSidebar],
  );
  const changeFilter = useCallback(
    (v: string) => {
      touchSidebar();
      setFilter(v);
    },
    [touchSidebar],
  );

  // 浏览器通知（MISSION §6.6；A18）：点击回到该行——openTab 让它成为侧栏 active 行，主区 crumb 之下
  // 就画出这一行的回答面板，焦点再落进面板里（MISSION §6.6「点击落到就地回答区」；A50，agora-4yr.1。
  // 面板不在这一行时下面那个 effect 会把请求丢掉，焦点不动）。
  const openFromNotification = useCallback(
    (id: string) => {
      openTab(id);
      setRespondFocus(id);
    },
    [openTab],
  );
  const notifier = useMemo(() => new Notifier(notifyDeps ?? browserDeps(), openFromNotification), [notifyDeps, openFromNotification]);
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
  // 聚焦请求只对当前这一帧有效：面板不在（这一行没有可回答的东西，或正在看 diff）就丢掉——
  // 留着它会在这一行下一次变成 TURN_DONE 的那一刻把焦点从终端抢走。
  const panelRow = active && !showDiff && hasRespondPanel(active) ? active : null;
  useEffect(() => {
    if (respondFocus !== null && respondFocus !== panelRow?.id) setRespondFocus(null);
  }, [respondFocus, panelRow?.id]);
  const openNewAgent = useCallback((initial?: NewAgentInitial) => {
    setNewAgentInitial(initial);
    setNewAgentOpen(true);
  }, []);
  const closeNewAgent = useCallback(() => {
    setNewAgentOpen(false);
    setNewAgentInitial(undefined);
  }, []);
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

  // 「看过」的 FINISHED 行（MISSION §4.6 证据 ①；A46，agora-j4w.1）：浏览器视图状态，localStorage 记着、
  // 不进服务端。记的时机是**离开**那一行（切到别的行 / 关闭视图）而不是选中的那一刻：选中期间它得留在
  // NEEDS ATTENTION 原位——记在选中那一刻它会立刻掉进收起的 Finished 区，主区还开着它的终端、侧栏却找不到
  // 这一行（2026-09-08 实现时先这么写过）。离开时看它**当时**的状态：选中时还在跑、离开后才 FINISHED 的
  // 不算看过（结果是离开之后才出现的）。
  const [seen, setSeen] = useState<Set<string>>(() => loadSeen());
  const byIdRef = useRef(byId);
  byIdRef.current = byId;
  const prevViewId = useRef<string | null>(null);
  useEffect(() => {
    const prev = prevViewId.current;
    const cur = view?.id ?? null;
    prevViewId.current = cur;
    if (prev === null || prev === cur) return;
    const left = byIdRef.current.get(prev);
    // external 的 FINISHED 不看 seen 就已经收起（finishedCollapsed），记它只是往 localStorage 里攒垃圾。
    if (!left || left.status !== "finished" || left.origin === "external") return;
    const key = seenKey(left);
    setSeen((s) => {
      if (s.has(key)) return s;
      const next = new Set(s).add(key);
      storeSeen(next);
      return next;
    });
  }, [view?.id]);
  // 看过的记号跟着"这一次完成"走：行被删了（Delete metadata）就忘掉，行又跑起来了（Restart）也忘掉——
  // 下一次 FINISHED 是新结果，得再进一次 NEEDS ATTENTION。记号是 `<id>@<status_since>`（`seenKey`）：
  // 中间的 running 被同一批事件或 resync 跳过时 byId 里从没出现过它，只看"当前是不是 finished"清不掉；
  // 新一次 FINISHED 的 status_since 不同，键对不上就是旧记号（agora-23h）。首个快照到达之前列表是空的，
  // 别把整个集合清掉。
  useEffect(() => {
    if (byId.size === 0) return;
    setSeen((s) => {
      const current = new Set<string>();
      for (const r of byId.values()) if (r.status === "finished") current.add(seenKey(r));
      const kept = [...s].filter((key) => current.has(key));
      if (kept.length === s.size) return s;
      const next = new Set(kept);
      storeSeen(next);
      return next;
    });
  }, [byId]);

  // 侧栏显示顺序：visibleOrder 一条（A47，agora-uvd.2）。attention 分支原样 =
  // partitionByAttention(fuzzyFilter(sortByAttention))；tree 分支是树的 DFS 顺序（agora-uvd.3，
  // 节点序取自 Header 那一排 nodes、本机第一）。Alt/Option+N / ]/[ / 过滤框 Enter 都走这条 visible。
  //
  // 冻结（A51，agora-4yr.4）只包在 attention 分支外面：树视图本来就不按状态重排，冻它没有意义，
  // 而且 prev 在树模式下一律清空——不然从树切回「需要我」时，冻着的是树的顺序。
  const prevOrder = useRef<{ ids: string[]; sections: Map<string, Section> } | null>(null);
  const { rows: visible, sections: visibleSections, moved } = useMemo(() => {
    const next = visibleOrder(mode, rows, filter, seen, nodes, localNode ?? undefined);
    if (mode !== "attention") return { rows: next, sections: undefined, moved: NO_MOVED };
    const prev = prevOrder.current;
    const stable = stableOrder(prev?.ids ?? null, next, frozen);
    const placed = stableSections(prev?.sections ?? null, stable.order, (r) => sectionOf(r, seen), frozen);
    return { rows: placed.order, sections: placed.sections, moved: stable.moved };
  }, [mode, rows, filter, seen, nodes, localNode, frozen]);
  // 上一次的显示顺序**只在 commit 阶段写**：render 阶段写 ref 是 React 明令禁止的（渲染被打断 / 丢弃时
  // ref 已经被改脏，而那一帧根本没提交），本仓库也已被 StrictMode 咬过两次（agora-3w8 的配对兑换、
  // TerminalView 的双挂载）。
  //
  // 但要说清楚这条守卫钉不住什么，免得后来者据此以为「写进 useMemo 也没事」：2026-09-10 实测 React
  // 19.2 + StrictMode，useMemo 的工厂确实跑两遍，**提交的却是第一遍的结果**（探针：mount 得到
  // call#1/#2、DOM 是 call#1；update 得到 call#3/#4、DOM 是 call#3）。所以把这一行挪回 useMemo 里，
  // 第二遍读到的毒值会被丢掉，下面那条 <StrictMode> 用例并不会变红——它钉的是"整条冻结 / 落位链路在
  // StrictMode 下照样出 moved"，不是"ref 写在哪一阶段"。真正拦住 render 阶段写 ref 的是这段注释和
  // React 自己的规则。守卫 Workspace.test.tsx「<StrictMode> still reports the rows that moved」。
  useEffect(() => {
    prevOrder.current = visibleSections
      ? { ids: visible.map((r) => r.id), sections: new Map(visible.map((r, i) => [r.id, visibleSections[i]])) }
      : null;
  }, [visible, visibleSections]);

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
          openNewAgent();
          break;
        case "next":
        case "prev": {
          if (visible.length === 0) break;
          touchSidebar();
          const at = visible.findIndex((r) => r.id === view?.id);
          const step = hit.action === "next" ? 1 : -1;
          const next = visible[(at + step + visible.length) % visible.length];
          if (next) openTab(next.id);
          break;
        }
        case "jump": {
          // 先冻再跳：跳过去这一下就常常改变排序（选中会把上一行写进 seen），第 N 条的位置得稳住，
          // 不然连按两下 Alt+2 会落到两行不同的会话上。用的是**冻结期间的显示顺序**，与眼睛看到的一致。
          const target = visible[hit.index];
          touchSidebar();
          if (target) openTab(target.id);
          break;
        }
        case "mode":
          toggleMode();
          break;
        case "respond":
          // 面板存在时才做事：Alt/Option+R 是把焦点挪进已经画出来的面板，不是"打开"什么。
          if (panelRow) setRespondFocus(panelRow.id);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteOpen, newAgentOpen, visible, view?.id, openTab, toggleMode, openNewAgent, panelRow, touchSidebar]);

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
        mode={mode}
        onMode={applyMode}
        seen={seen}
        nodes={nodes}
        localNode={localNode ?? undefined}
        all={rows}
        total={rows.length}
        // 看 diff 时它的会话行仍是选中行（展开区留着，验收标准 / 改动列表与 diff 并排对照，MISSION §6.3）；
        // 不然行一折叠，关掉 diff 又重挂载重拉一次 /changes（2026-09-06 代检时看到）。
        // 2026-09-10 复核（agora-4yr.3）：那两段已经从行里搬进主区的 result-panel，上面这条结论
        // 一个字不改——只是"折叠"的形态从"行收起来"变成"面板不画"，所以 diff 视图下只藏
        // respond-panel、result-panel 照画（下面那处），重拉那一幕仍然不会发生。
        active={view?.id ?? null}
        onOpen={openFromSidebar}
        onNewAgent={openNewAgent}
        onRowRender={onRowRender}
        filter={filter}
        onFilter={changeFilter}
        filterRef={filterInput}
        onFilterEnter={() => {
          const first = visible[0];
          if (first) openFromSidebar(first.id);
        }}
        sections={visibleSections}
        moved={moved}
        onPointerEnter={enterSidebar}
        onPointerLeave={leaveSidebar}
        unregistered={unregistered}
        onAdopt={adopt}
        onDeleteMetadata={api.deleteMetadata}
        // 树视图组头「shell」直接起会话（agora-uvd.4）：成功走 pendingOpen，行进列表后自动选中。
        api={api}
        onCreated={setPendingOpen}
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
            {/* crumb 之下、终端之上的两个面板（A50）：回答面板（agora-4yr.1）+ 看结果面板
                （agora-4yr.3）。包一层 .panels 是为了给**两者合计**一个 50vh 的上界、超了整块
                自己滚——分别限高只能保证各自不失控，一段长验收标准加一段长回复照样能把终端挤没。
                回答面板在上：回答问题 / 给下一条指令是先做的事，对照验收看结果是后做的事
                （agora-h1k.3 定的顺序）。
                diff 视图只藏回答面板（那一格在看结果，不在回答），RowResult 照画——验收标准 /
                改动列表与 diff 并排对照是 MISSION §6.3 要的，而且藏掉它「关闭 diff」会重拉一次
                GET /changes（见上面 Sidebar active 那条注释与 RowResult.tsx 文件头）。
                key 带会话 id：切行时面板重挂载，草稿 / busy / 错误 / 折叠状态不跨行残留（它们长在
                侧栏行的 <li> 里时是随行天然重挂的，搬进主区后位置固定，不加 key 就会串行）。两个 key
                各带前缀而不是都用裸 id：同一父节点下的两个兄弟用同一个 key，React 只把其中一个放进
                重排用的 key map，切 diff 时（回答面板那一格变成 false）另一个就可能被判成新节点重挂——
                Changes 一重挂就是一次多余的 GET /changes，正是本任务要避免的那一幕。 */}
            <div className="panels">
              {!showDiff && (
                <RespondPanel
                  key={`respond:${active.id}`}
                  row={active}
                  api={api}
                  onOpenTerminal={focusTerminal}
                  focusRequest={respondFocus === active.id}
                  onFocusHandled={clearRespondFocus}
                />
              )}
              <RowResult key={`result:${active.id}`} row={active} onOpenDiff={openDiff} />
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
          initial={newAgentInitial}
          nodes={nodes}
          onClose={closeNewAgent}
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
          onToggleMode={toggleMode}
          onCreated={(id) => setPendingOpen(id)}
          onClose={() => setPaletteOpen(false)}
        />
      )}
    </div>
    </ChangesApiContext.Provider>
  );
}
