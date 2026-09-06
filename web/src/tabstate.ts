/**
 * 浏览器 Tab 只表示"这个浏览器打开了哪些 agent"，不是 agent 生命周期（MISSION §4.6）。
 * 这是个纯 reducer：开 / 关 / 切换只改本地列表，不会也不能发任何请求——
 * 关 Tab 等价 Detach，绝不 restart / recreate / kill。守卫在 Workspace.test.tsx（文件名不叫 tabs.ts：macOS 上与 Tabs.tsx 只差大小写，tsc 报 TS1261）。
 */

export interface TabsState {
  open: string[];
  active: string | null;
}

export const emptyTabs: TabsState = { open: [], active: null };

export type TabsAction =
  | { type: "open"; id: string }
  | { type: "close"; id: string }
  | { type: "activate"; id: string }
  /** 会话从列表里消失（被删 metadata）：Tab 跟着关；内容相等就原样返回。 */
  | { type: "prune"; existing: Set<string> };

export function tabsReducer(state: TabsState, action: TabsAction): TabsState {
  switch (action.type) {
    case "open":
      if (state.open.includes(action.id)) {
        return state.active === action.id ? state : { ...state, active: action.id };
      }
      return { open: [...state.open, action.id], active: action.id };
    case "activate":
      return state.open.includes(action.id) && state.active !== action.id
        ? { ...state, active: action.id }
        : state;
    case "close": {
      const i = state.open.indexOf(action.id);
      if (i < 0) return state;
      const open = state.open.filter((x) => x !== action.id);
      let active = state.active;
      if (active === action.id) active = open[Math.min(i, open.length - 1)] ?? null;
      return { open, active };
    }
    case "prune": {
      const open = state.open.filter((id) => action.existing.has(id));
      if (open.length === state.open.length) return state;
      const active = state.active && open.includes(state.active) ? state.active : (open[0] ?? null);
      return { open, active };
    }
  }
}

/**
 * diff 标签页（MISSION §6.3 看结果；agora-h1k.5）的 id：`diff:<会话 id>`。它不是会话——只是一个跑 git diff 的
 * 只读终端，关掉即消失、侧栏不多一行；Workspace 在 prune 的 existing 集合里给每个现有会话也放一个 diff id，
 * 否则 diff 标签一开就被当"消失的会话"剪掉。
 */
export const DIFF_PREFIX = "diff:";

export function diffTabId(sessionId: string): string {
  return `${DIFF_PREFIX}${sessionId}`;
}

/** `diff:<id>` → `<id>`；不是 diff 标签页返回 null。 */
export function diffTarget(tabId: string): string | null {
  return tabId.startsWith(DIFF_PREFIX) ? tabId.slice(DIFF_PREFIX.length) : null;
}
