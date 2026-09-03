import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { catalogApi, sessionApi, type CatalogApi, type SessionApi } from "./api";
import { NewAgentDialog } from "./NewAgentDialog";
import { SessionSettings } from "./SessionSettings";
import { Sidebar, rowName } from "./Sidebar";
import { SessionStore, useSessions } from "./store";
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
  const [tabs, dispatch] = useReducer(tabsReducer, emptyTabs);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newAgentOpen, setNewAgentOpen] = useState(false);
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

  return (
    <div className="workspace">
      <Sidebar
        rows={rows}
        active={tabs.active}
        onOpen={openTab}
        onNewAgent={() => setNewAgentOpen(true)}
        onRowRender={onRowRender}
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
    </div>
  );
}
