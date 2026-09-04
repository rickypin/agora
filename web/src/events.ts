/**
 * `/api/events` 的消费纪律（docs/spec/api.md）：
 * - 先拉 `GET /api/sessions` 全量，再按事件就地 patch；
 * - 合并突发：事件先进缓冲，~300 ms 后一次性应用；
 * - 断流重连 / 收到 `resync` → 重拉全量对齐；绝不回退为轮询。
 *
 * 与 DOM 无关（vitest 在 node 环境里跑）：WebSocket 与快照拉取通过参数注入。
 */

import { apiFetch } from "./net";

export interface SessionRow {
  id: string;
  node: string;
  status: string;
  alive: boolean;
  [key: string]: unknown;
}

/** 运行时里有、metadata 里没有的会话（Unknown Agent，可采纳；docs/spec/api.md）。 */
export interface UnregisteredRow {
  runtime_ref: string;
  name: string;
  title: string;
  alive: boolean;
  managed: boolean;
  working_directory: string;
  /** 进程树认出来的 adapter 名；只是默认值，用户填的优先（MISSION §5.4）。 */
  agent_hint: string | null;
  node: string;
}

export interface Snapshot {
  sessions: SessionRow[];
  unregistered: UnregisteredRow[];
}

export type AgoraEvent =
  | { type: "session_created"; id: string; session: SessionRow }
  | { type: "session_removed"; id: string }
  | { type: "session_updated"; id: string; session: SessionRow }
  | {
      type: "status_changed";
      id: string;
      status: string;
      source: string;
      reason: string | null;
      alive: boolean;
      detail?: string | null;
      prompt?: string | null;
      progress?: string | null;
      preview?: string | null;
      status_since?: number;
    }
  | { type: "decision_resolved"; id: string; tool_use_id: string; via: string }
  | { type: "notification"; id: string | null; title: string; body: string }
  | { type: "resync" };

/** 最小的 WebSocket 形态，便于测试用假对象。 */
export interface SocketLike {
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  close(): void;
}

export interface EventsClientOptions {
  /** 建 WS 连接；默认 `new WebSocket(<同源>/api/events)`。 */
  connect?: () => SocketLike;
  /** 拉全量；默认 `apiFetch("/api/sessions")`。 */
  fetchSnapshot?: () => Promise<Snapshot>;
  /** 合并窗口，默认 300 ms。 */
  coalesceMs?: number;
  /** 重连退避起点 / 上限，默认 1 s / 30 s。 */
  reconnectMinMs?: number;
  reconnectMaxMs?: number;
  /** 视图变了才回调；内容相等不重渲染由调用方比较。 */
  onChange: (sessions: Map<string, SessionRow>) => void;
  onNotification?: (n: { id: string | null; title: string; body: string }) => void;
  /** 挂起的决定被终端 / 超时 / 退出解除：就地回答的面板据此收起（ADR-002 D5）。 */
  onDecisionResolved?: (e: { id: string; tool_use_id: string; via: string }) => void;
  /** 未登记会话只随全量快照来（事件流不推它们）；每次 resync 后回调。 */
  onUnregistered?: (rows: UnregisteredRow[]) => void;
}

export function defaultSocket(): SocketLike {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return new WebSocket(`${proto}//${window.location.host}/api/events`) as unknown as SocketLike;
}

export async function defaultSnapshot(): Promise<Snapshot> {
  const resp = await apiFetch("/api/sessions");
  if (!resp.ok) throw new Error(`GET /api/sessions ${resp.status}`);
  return (await resp.json()) as Snapshot;
}

export class EventsClient {
  readonly sessions = new Map<string, SessionRow>();
  unregistered: UnregisteredRow[] = [];
  private socket: SocketLike | null = null;
  private pending: AgoraEvent[] = [];
  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoff: number;
  private stopped = false;
  /** 统计：重拉全量的次数（测试断言用）。 */
  snapshots = 0;

  constructor(private readonly opts: EventsClientOptions) {
    this.backoff = opts.reconnectMinMs ?? 1000;
  }

  start(): void {
    this.stopped = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    if (this.flushTimer) clearTimeout(this.flushTimer);
    this.socket?.close();
    this.socket = null;
  }

  private open(): void {
    const sock = (this.opts.connect ?? defaultSocket)();
    this.socket = sock;
    sock.onopen = () => {
      this.backoff = this.opts.reconnectMinMs ?? 1000;
      // 连上（含重连）先对齐全量：断流期间丢掉的事件不可能补回来。
      void this.resync();
    };
    sock.onmessage = (ev) => {
      let batch: AgoraEvent[];
      try {
        const parsed = JSON.parse(ev.data) as AgoraEvent[] | { type: string };
        batch = Array.isArray(parsed) ? parsed : [];
      } catch {
        return;
      }
      for (const e of batch) this.enqueue(e);
    };
    sock.onclose = () => this.scheduleReconnect();
    sock.onerror = () => {
      /* onclose 会跟着来 */
    };
  }

  private scheduleReconnect(): void {
    if (this.stopped) return;
    const delay = this.backoff;
    this.backoff = Math.min(this.backoff * 2, this.opts.reconnectMaxMs ?? 30_000);
    this.reconnectTimer = setTimeout(() => this.open(), delay);
  }

  private enqueue(e: AgoraEvent): void {
    if (e.type === "resync") {
      // 服务端丢过我们的事件：缓冲里的增量已不可信，直接重拉。
      this.pending = [];
      void this.resync();
      return;
    }
    if (e.type === "notification") {
      this.opts.onNotification?.(e);
      return;
    }
    if (e.type === "decision_resolved") {
      this.opts.onDecisionResolved?.(e);
      return;
    }
    this.pending.push(e);
    if (!this.flushTimer) {
      this.flushTimer = setTimeout(() => this.flush(), this.opts.coalesceMs ?? 300);
    }
  }

  private flush(): void {
    this.flushTimer = null;
    const batch = this.pending;
    this.pending = [];
    let changed = false;
    for (const e of batch) changed = this.apply(e) || changed;
    if (changed) this.opts.onChange(this.sessions);
  }

  /** 就地 patch；返回是否真的变了。 */
  private apply(e: AgoraEvent): boolean {
    switch (e.type) {
      case "session_created":
      case "session_updated": {
        const prev = this.sessions.get(e.id);
        if (prev && JSON.stringify(prev) === JSON.stringify(e.session)) return false;
        this.sessions.set(e.id, e.session);
        return true;
      }
      case "session_removed":
        // 只删 metadata 的会话会变成"未登记"，而未登记列表只在快照里：重拉一次。
        void this.resync();
        return this.sessions.delete(e.id);
      case "status_changed": {
        const row = this.sessions.get(e.id);
        if (!row) return false;
        const next: SessionRow = { ...row, status: e.status, source: e.source, reason: e.reason, alive: e.alive };
        // 预览与起点字段：事件没带（undefined）就沿用旧值，带了 null 就是清空。
        for (const k of ["detail", "prompt", "progress", "preview", "status_since"] as const) {
          if (e[k] !== undefined) next[k] = e[k];
        }
        if ((["status", "source", "reason", "alive", "detail", "prompt", "progress", "preview", "status_since"] as const).every((k) => row[k] === next[k]))
          return false;
        this.sessions.set(e.id, next);
        return true;
      }
      default:
        return false;
    }
  }

  /** 采纳 / 删除之后让未登记列表对齐（它不走事件流）。 */
  refresh(): Promise<void> {
    return this.resync();
  }

  private async resync(): Promise<void> {
    this.snapshots += 1;
    let snap: Snapshot;
    try {
      snap = await (this.opts.fetchSnapshot ?? defaultSnapshot)();
    } catch {
      return; // 下一次重连或 resync 再试；不轮询。
    }
    this.sessions.clear();
    for (const s of snap.sessions) this.sessions.set(s.id, s);
    this.unregistered = snap.unregistered ?? [];
    this.opts.onChange(this.sessions);
    this.opts.onUnregistered?.(this.unregistered);
  }
}
