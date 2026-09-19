/**
 * `/api/events` 的消费纪律（docs/spec/api.md）：
 * - 先拉 `GET /api/sessions` 全量，再按事件就地 patch；
 * - 合并突发：事件先进缓冲，~300 ms 后一次性应用；
 * - 断流重连 / 收到 `resync` → 重拉全量对齐；绝不回退为轮询。
 *
 * 与 DOM 无关（vitest 在 node 环境里跑）：WebSocket 与快照拉取通过参数注入。
 */

import type { PeerHealth } from "./health";
import { apiFetch } from "./net";

/**
 * 进程三态（Q4 裁决 agora-5gg.4；`docs/spec/api.md`「会话形态」）。
 * - `alive`：探到了，进程还在；
 * - `gone`：对话结束了（FINISHED / FAILED 一律如此，哪怕那个 pid 还在跑别的对话）或探到进程没了；
 * - `unknown`：agora 说不上（无可信进程号的 external 行，Codex Desktop 那一类）。
 *
 * 旧的 `alive` 布尔把后两者压成一个 `false`（盘点 B2：7 行 turn_done + alive:false 与真没了的行
 * 长得一样），所以只保留一版。
 */
export type ProcessState = "alive" | "gone" | "unknown";

const PROCESS_STATES: readonly string[] = ["alive", "gone", "unknown"];

/** 运行时守卫：行与事件都来自别的节点的文本，不在词表里的字面量一律当「没说」。 */
export function isProcessState(v: unknown): v is ProcessState {
  return typeof v === "string" && PROCESS_STATES.includes(v);
}

/**
 * 读一行当前的进程三态。没升级的 peer（api_version minor < 1.7）不发 `process`，
 * 从旧布尔退回两值：`false` 在旧形态里既可能是真没了也可能是「不知道」，这里读成 `gone`——
 * 猜错的方向是少说一次未知，不影响按钮与排序（新节点升级后走全量重拉，自然拿到三值）。
 */
export function rowProcess(row: SessionRow): ProcessState {
  return isProcessState(row.process) ? row.process : row.alive ? "alive" : "gone";
}

export interface SessionRow {
  id: string;
  node: string;
  status: string;
  /**
   * 旧字段（= `process === "alive"`），服务端只保留一版（agora-5gg.18）：新调用方读
   * [`SessionRow.process`]。用 [`rowProcess`] 读进程事实，它对没升级的 peer 行自动退回这里。
   */
  alive: boolean;
  /**
   * 进程三态（Q4 裁决 agora-5gg.4；`docs/spec/api.md`「会话形态」）：见 [`ProcessState`]。
   * peer 行可能是没升级的节点来的（同 major、minor 更旧），**这个键可以不存在** —— 读它走
   * [`rowProcess`]，不要直接 `row.process === "gone"`。
   */
  process?: ProcessState;
  /** 只有并入的 peer 行带它：该 peer 掉线后保留的最后一眼（MISSION §3.5；不变量 8）；本机行没有。 */
  stale?: boolean;
  /**
   * 只有 `stale: true` 的 peer 行带它：该 peer 的"上次见到"，本节点时钟打的 UTC 文本
   * （与 `/api/health` peers 段的 `last_seen` 同一个值，`docs/spec/api.md`「peer 视图」；agora-7ku.6）。
   * 非 stale 时这个键不存在，本机行永远没有。
   */
  last_seen?: string;
  pending_decision?: { request_id: string; summary: string; epoch: number; host?: string } | null;
  /**
   * 用户按过 Kill 的时刻（Restart 清空）。`killed_at && alive` = 进程还在吃 TERM、节点的宽限在后台
   * 走着：设置面板据此显示"正在结束"，直到行推成 FINISHED（agora-284；`docs/spec/ux.md` Kill）。
   */
  killed_at?: string | null;
  /**
   * 会话所在 git 仓库 / worktree / 分支（服务端按 working_directory 现算；agora-uvd.1，A49）。
   * 目录不存在 / 不是仓库 / git 不可用 / 超时为 null，前端据此归「其它目录」。
   * 本任务只加类型，不改渲染。
   */
  project?: {
    repo: string;
    name: string;
    worktree: string;
    branch: string | null;
    main: boolean;
  } | null;
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
      /** 进程三态（agora-5gg.18）：与 `alive` 同时带；没升级的节点不发。 */
      process?: ProcessState;
      detail?: string | null;
      prompt?: string | null;
      progress?: string | null;
      preview?: string | null;
      status_since?: number;
      hooks_unheard?: string | null;
    }
  | { type: "decision_resolved"; id: string; tool_use_id: string; via: string }
  /** 浏览器通知（MISSION §6.6）：服务端只在四种转换上发；`status` 是转换后的状态。 */
  | { type: "notification"; id: string | null; title: string; body: string; status?: string | null }
  /** 一个 peer 的面貌变了（agora-c8h）：`peer` 是 `/api/health` peers 段里该节点的那一项。 */
  | { type: "peer_changed"; name: string; peer: PeerHealth }
  | { type: "resync" };

/** 服务端因吊销 / 轮换主动关长连接时的关闭码（docs/spec/api.md「认证」；agora-0jt）。 */
export const REVOKED_CLOSE_CODE = 4401;

/** 最小的 WebSocket 形态，便于测试用假对象。 */
export interface SocketLike {
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  /** 真 WebSocket 给的是 CloseEvent（有 `code`）；假对象可以什么都不给。 */
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
  onNotification?: (n: { id: string | null; title: string; body: string; status?: string | null }) => void;
  /** 挂起的决定被终端 / 超时 / 退出解除：就地回答的面板据此收起（ADR-002 D5）。 */
  onDecisionResolved?: (e: { id: string; tool_use_id: string; via: string }) => void;
  /** 未登记会话只随全量快照来（事件流不推它们）；每次 resync 后回调。 */
  onUnregistered?: (rows: UnregisteredRow[]) => void;
  /**
   * 一个 peer 上线 / 掉线 / 换了错误类型（agora-c8h）：立刻回调，不进 300 ms 的合并批——Header 的点
   * 与侧栏行的变灰该是同一眼看到的。`peer` 原样给出，由 HealthWatcher 整形（web/src/health.ts）。
   */
  onPeerChanged?: (name: string, peer: unknown) => void;
  /**
   * WS 每次连上（启动的首连、断流后的重连）时回调，在重拉全量之前。节点的 api_version 比对
   * 挂这里（agora-7ku.4）：升级节点必然重启 daemon、WS 必然断一次，所以换代总能在这一刻被看见。
   */
  onOpen?: () => void;
  /**
   * 服务端以 4401 关掉了这条流：本设备被吊销（agora-0jt）。之后不再重连——重连只会吃 401，
   * 页面该回到配对门。
   */
  onRevoked?: () => void;
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
      this.opts.onOpen?.();
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
    sock.onclose = (ev) => {
      if ((ev as { code?: unknown } | null)?.code === REVOKED_CLOSE_CODE) {
        this.stop();
        this.opts.onRevoked?.();
        return;
      }
      this.scheduleReconnect();
    };
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
    if (e.type === "peer_changed") {
      this.opts.onPeerChanged?.(e.name, e.peer);
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
        // `process` 只认词表里的值（对端是别的节点的二进制，多一个不认识的字面量不算新闻）。
        // 事件没带 = 对端还没升级：行上也不留旧的三值，否则会出现 `alive: false` 而 `process:
        // "alive"` 这种自己打自己的行，而 rowProcess 会拿那个陈旧值说话。带了却不认识：保留上一眼。
        if (isProcessState(e.process)) next.process = e.process;
        else if (e.process === undefined) delete next.process;
        // 预览与起点字段：事件没带（undefined）就沿用旧值，带了 null 就是清空。
        for (const k of ["detail", "prompt", "progress", "preview", "status_since", "hooks_unheard"] as const) {
          if (e[k] !== undefined) next[k] = e[k];
        }
        if (
          (["status", "source", "reason", "alive", "process", "detail", "prompt", "progress", "preview", "status_since", "hooks_unheard"] as const).every(
            (k) => row[k] === next[k],
          )
        )
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
