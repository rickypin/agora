/**
 * 会话列表的 React 接线：EventsClient（就地 patch、300 ms 合并、resync 重拉）→
 * `useSyncExternalStore`。行对象只在自己变了时换引用（EventsClient.apply 只写被改的行），
 * 所以侧栏行用 `memo` 后，一条状态变化只重渲染那一行——渲染计数守卫在 Workspace.test.tsx。
 */
import { useSyncExternalStore } from "react";
import { EventsClient, type EventsClientOptions, type SessionRow, type UnregisteredRow } from "./events";

export class SessionStore {
  private rows: SessionRow[] = [];
  private unregistered: UnregisteredRow[] = [];
  private listeners = new Set<() => void>();
  readonly client: EventsClient;
  /** onChange 次数（测试断言用）。 */
  changes = 0;

  constructor(opts: Omit<EventsClientOptions, "onChange" | "onUnregistered"> = {}) {
    this.client = new EventsClient({
      ...opts,
      onChange: (map) => {
        this.changes += 1;
        this.rows = [...map.values()];
        for (const l of this.listeners) l();
      },
      onUnregistered: (rows) => {
        this.unregistered = rows;
        for (const l of this.listeners) l();
      },
    });
  }

  start(): void {
    this.client.start();
  }
  stop(): void {
    this.client.stop();
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = (): SessionRow[] => this.rows;
  unregisteredSnapshot = (): UnregisteredRow[] => this.unregistered;
}

export function useUnregistered(store: SessionStore): UnregisteredRow[] {
  return useSyncExternalStore(store.subscribe, store.unregisteredSnapshot, store.unregisteredSnapshot);
}

export function useSessions(store: SessionStore): SessionRow[] {
  return useSyncExternalStore(store.subscribe, store.snapshot, store.snapshot);
}
