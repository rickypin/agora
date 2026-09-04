import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { catalogApi, sessionApi, type CatalogApi, type SessionApi } from "./api";
import { CommandPalette } from "./CommandPalette";
import { fuzzyFilter } from "./fuzzy";
import { isDesktop, matchShortcut } from "./keys";
import { NewAgentDialog } from "./NewAgentDialog";
import { Respond } from "./Respond";
import { SessionSettings } from "./SessionSettings";
import { rowHaystack, rowName, Sidebar } from "./Sidebar";
import { SessionStore, useSessions, useUnregistered } from "./store";
import { Tabs } from "./Tabs";
import { TerminalView } from "./TerminalView";
import { emptyTabs, tabsReducer } from "./tabstate";

interface Props {
  store?: SessionStore;
  api?: SessionApi;
  catalog?: CatalogApi;
  /** 测试注入：侧栏行渲染计数。 */
  onRowRender?: (id: string) => void;
}

/** Screen B：侧栏 + Tabs + 终端 + Session Settings。 */
export function Workspace({ store: given, api: givenApi, catalog: givenCatalog, onRowRender }: Props) {
  const store = useMemo(() => given ?? new SessionStore(), [given]);
  const api = useMemo(() => givenApi ?? sessionApi(), [givenApi]);
  const catalog = useMemo(() => givenCatalog ?? catalogApi(), [givenCatalog]);
  const rows = useSessions(store);
  const unregistered = useUnregistered(store);
  const [tabs, dispatch] = useReducer(tabsReducer, emptyTabs);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newAgentOpen, setNewAgentOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const filterInput = useRef<HTMLInputElement>(null);
  // 刚创建的会话：等它随事件流进列表再开 Tab。POST 的响应先于 `session_created` 到达，
  // 这时开 Tab 会被 prune（列表里还没有这一行）立刻关掉。
  const [pendingOpen, setPendingOpen] = useState<string | null>(null);

  useEffect(() => {
    store.start();
    return () => store.stop();
  }, [store]);

  const byId = useMemo(() => new Map(rows.map((r) => [r.id, r])), [rows]);
  useEffect(() => {
    dispatch({ type: "prune", existing: new Set(byId.keys()) });
  }, [byId]);
  useEffect(() => {
    if (pendingOpen && byId.has(pendingOpen)) {
      dispatch({ type: "open", id: pendingOpen });
      setPendingOpen(null);
    }
  }, [byId, pendingOpen]);

  const active = tabs.active ? byId.get(tabs.active) : undefined;
  // 回调必须稳定：侧栏行是 memo 的，每次渲染换一个闭包会让所有行跟着重渲染。
  const openTab = useCallback((id: string) => dispatch({ type: "open", id }), []);
  const openNewAgent = useCallback(() => setNewAgentOpen(true), []);
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

  // 侧栏显示顺序 = 过滤后的顺序，Alt/Option+N 跳的就是它（agora-xqa.14 验收）。
  const visible = useMemo(() => fuzzyFilter(rows, filter, rowHaystack), [rows, filter]);

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
          const at = visible.findIndex((r) => r.id === tabs.active);
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
  }, [paletteOpen, newAgentOpen, visible, tabs.active, openTab]);

  return (
    <div className="workspace">
      <Sidebar
        rows={visible}
        total={rows.length}
        active={tabs.active}
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
      />
      <section className="main">
        <Tabs
          open={tabs.open}
          active={tabs.active}
          rows={byId}
          onActivate={(id) => dispatch({ type: "activate", id })}
          onClose={(id) => dispatch({ type: "close", id })}
        />
        {active ? (
          <>
            <div className="crumb">
              <span>
                {rowName(active)} / {String(active.agent_type ?? "")} @ {active.node}
              </span>
              <button onClick={() => setSettingsOpen((v) => !v)} aria-pressed={settingsOpen}>
                Settings
              </button>
            </div>
            <div className="pane">
              {/* key=会话 id：切 Tab 时旧终端卸载（detach）、新终端挂载，永不 restart。 */}
              <TerminalView key={active.id} sessionId={active.id} />
              {settingsOpen && <SessionSettings row={active} api={api} onClose={() => setSettingsOpen(false)} />}
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
          onClose={() => setNewAgentOpen(false)}
          onCreated={(id) => setPendingOpen(id)}
        />
      )}
      {paletteOpen && (
        <CommandPalette
          rows={rows}
          api={api}
          catalog={catalog}
          onOpen={openTab}
          onNewAgent={openNewAgent}
          onCreated={(id) => setPendingOpen(id)}
          onClose={() => setPaletteOpen(false)}
        />
      )}
    </div>
  );
}
