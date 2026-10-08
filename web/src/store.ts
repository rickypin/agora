/**
 * 会话列表的 React 接线：EventsClient（就地 patch、300 ms 合并、resync 重拉）→
 * `useSyncExternalStore`。行对象只在自己变了时换引用（EventsClient.apply 只写被改的行），
 * 所以侧栏行用 `memo` 后，一条状态变化只重渲染那一行——渲染计数守卫在 Workspace.test.tsx。
 */
import { useSyncExternalStore } from "react";
import type { ServerClock } from "./attention";
import {
  defaultSnapshot,
  EventsClient,
  type EventsClientOptions,
  type SessionRow,
  type UnregisteredRow,
  type UnreadableSocket,
} from "./events";
import type { Incoming } from "./notify";

export class SessionStore {
  private rows: SessionRow[] = [];
  private unregistered: UnregisteredRow[] = [];
  /** 未登记列表这一次没扫成的 socket（agora-ebfa）；与 `unregistered` 同一次快照。 */
  private unreadableSockets: UnreadableSocket[] = [];
  /**
   * 最近一次快照的节点钟基准（agora-au5）：响应顶层 `now` + 收到它时的页面钟读数。
   * 老节点 / 测试桩不发 `now`，或还没拉到快照时是 null（调用方退回页面钟）。
   */
  private clock: ServerClock | null = null;
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
      // 每次快照都把响应顶层的 now（节点钟读数）与收到它的页面钟时刻配成一对（agora-au5）：行上的
      // `status_since` 是节点打的（peer 行也是本节点钟重写的），页面钟的偏差会整体平移时长。
      // 老节点不发 now（undefined）就当没有——`clockSnapshot` 回到 null，调用方退回页面钟。
      fetchSnapshot: async () => {
        const snap = await (opts.fetchSnapshot ?? defaultSnapshot)();
        const now = snap.now;
        this.clock = typeof now === "number" && Number.isFinite(now) ? { now, at: Math.floor(Date.now() / 1000) } : null;
        return snap;
      },
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
  /** 最近一次快照的节点钟基准（agora-au5）；老节点 / 还没拉到快照时 null。 */
  clockSnapshot = (): ServerClock | null => this.clock;
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

/**
 * 最近一次快照的节点钟基准（agora-au5）：页面算等待时长拿它当 `now`（`anchoredNow`），
 * 页面与节点的钟差不再平移行上的时长。老节点 / 还没拉到快照时是 null，调用方退回页面钟。
 */
export function useServerClock(store: SessionStore): ServerClock | null {
  return useSyncExternalStore(store.subscribe, store.clockSnapshot, store.clockSnapshot);
}
