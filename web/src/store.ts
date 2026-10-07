/**
 * 会话列表的 React 接线：EventsClient（就地 patch、300 ms 合并、resync 重拉）→
 * `useSyncExternalStore`。行对象只在自己变了时换引用（EventsClient.apply 只写被改的行），
 * 所以侧栏行用 `memo` 后，一条状态变化只重渲染那一行——渲染计数守卫在 Workspace.test.tsx。
 */
import { useSyncExternalStore } from "react";
import { EventsClient, type EventsClientOptions, type SessionRow, type UnregisteredRow, type UnreadableSocket } from "./events";
import type { Incoming } from "./notify";

export class SessionStore {
  private rows: SessionRow[] = [];
  private unregistered: UnregisteredRow[] = [];
  /** 未登记列表这一次没扫成的 socket（agora-ebfa）；与 `unregistered` 同一次快照。 */
  private unreadableSockets: UnreadableSocket[] = [];
  private listeners = new Set<() => void>();
  readonly client: EventsClient;
  /** onChange 次数（测试断言用）。 */
  changes = 0;
  /** `notification` 事件的出口；Workspace 装上 Notifier（web/src/notify.ts）。 */
  onNotification: ((n: Incoming) => void) | null = null;
  /** WS 每次连上的出口；Workspace 挂 api_version 比对（web/src/health.ts 的 VersionWatcher）。 */
  onOpen: (() => void) | null = null;
  /** 服务端以 4401 关流（本设备被吊销，agora-0jt）的出口；App 据此回到配对门。 */
  onRevoked: (() => void) | null = null;
  /** `peer_changed` 事件的出口（agora-c8h）；Workspace 接到 HealthWatcher，Header 的点随之改。 */
  onPeerChanged: ((name: string, peer: unknown) => void) | null = null;

  constructor(
    opts: Omit<EventsClientOptions, "onChange" | "onUnregistered" | "onNotification" | "onOpen" | "onRevoked" | "onPeerChanged"> = {},
  ) {
    this.client = new EventsClient({
      ...opts,
      onNotification: (n) => this.onNotification?.(n),
      onOpen: () => this.onOpen?.(),
      onRevoked: () => this.onRevoked?.(),
      onPeerChanged: (name, peer) => this.onPeerChanged?.(name, peer),
      onChange: (map) => {
        this.changes += 1;
        this.rows = [...map.values()];
        for (const l of this.listeners) l();
      },
      onUnregistered: (rows, unreadable) => {
        this.unregistered = rows;
        this.unreadableSockets = unreadable;
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
  unreadableSocketsSnapshot = (): UnreadableSocket[] => this.unreadableSockets;
}

export function useUnregistered(store: SessionStore): UnregisteredRow[] {
  return useSyncExternalStore(store.subscribe, store.unregisteredSnapshot, store.unregisteredSnapshot);
}

/** 这一次没扫成的 socket（agora-ebfa）：侧栏用它说明 UNREGISTERED 为什么可能不完整。 */
export function useUnreadableSockets(store: SessionStore): UnreadableSocket[] {
  return useSyncExternalStore(store.subscribe, store.unreadableSocketsSnapshot, store.unreadableSocketsSnapshot);
}

export function useSessions(store: SessionStore): SessionRow[] {
  return useSyncExternalStore(store.subscribe, store.snapshot, store.snapshot);
}
