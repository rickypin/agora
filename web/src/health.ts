import { apiFetch } from "./net";

/** `GET /api/health` 的公开子集（docs/spec/api.md）。 */
export interface PublicHealth {
  status: string;
}

/** 完整报告里的 runtime 段（ADR-001 D7）：status / reason 每次请求现算。 */
export interface RuntimeHealth {
  status: string;
  reason: string | null;
  path_source?: string;
}

/** 带 principal 才有的完整形态（MISSION §10.3）；未认证只拿到 PublicHealth。 */
export interface FullHealth extends PublicHealth {
  runtime?: RuntimeHealth;
  database?: boolean;
}

export function isHealthy(h: unknown): h is PublicHealth {
  return typeof h === "object" && h !== null && (h as PublicHealth).status === "ok";
}

export async function fetchHealth(): Promise<boolean> {
  const resp = await apiFetch("/api/health");
  if (!resp.ok) return false;
  return isHealthy(await resp.json());
}

/**
 * runtime 报 degraded → 原因原文；ok、或者根本没有 runtime 段（未认证的公开子集）→ null。
 * 公开子集不算 degraded：横幅只根据带 cookie 拿到的完整报告出现，未认证路径什么都不显示
 * （ADR-003 D1；tests/health.rs 守卫公开子集只有 status 一个键）。
 */
export function runtimeDegraded(h: unknown): string | null {
  if (typeof h !== "object" || h === null) return null;
  const rt = (h as FullHealth).runtime;
  if (typeof rt !== "object" || rt === null || rt.status !== "degraded") return null;
  const reason = typeof rt.reason === "string" ? rt.reason.trim() : "";
  return reason || "原因未知";
}

async function defaultFetchHealth(): Promise<unknown> {
  const resp = await apiFetch("/api/health");
  if (!resp.ok) throw new Error(`GET /api/health ${resp.status}`);
  return resp.json();
}

export interface HealthWatcherOptions {
  /** 拉一次完整报告（同源请求自带 cookie）；默认 `apiFetch("/api/health")`。 */
  fetchHealth?: () => Promise<unknown>;
  /** 健康时的重拉间隔，默认 60 s。 */
  okMs?: number;
  /** degraded 期间的重拉间隔，默认 10 s：运行时恢复后横幅要能很快自己消失。 */
  degradedMs?: number;
}

/**
 * 盯着 `/api/health` 的 runtime 段（agora-bgr）。
 *
 * 这是前端唯一的一处轮询，与 `/api/events` 的"不得回退为轮询"（docs/spec/api.md）不冲突：
 * health 的 degraded 是服务端每次请求现算的结论，没有事件流推它；健康时一分钟一次几乎没有
 * 流量，degraded 时缩到 10 s 让恢复能被看见。拉不到（daemon 不在 / 401）就保持上一次的结论，
 * 不在"运行时异常"与"没有异常"之间来回闪。
 */
export class HealthWatcher {
  private degraded: string | null = null;
  private listeners = new Set<() => void>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  /** 拉过几次（测试断言用）。 */
  polls = 0;

  constructor(private readonly opts: HealthWatcherOptions = {}) {}

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    void this.poll();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  /** 立刻拉一次并重排下一次；给"刚才可能变了"的时刻用。 */
  refresh(): Promise<void> {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    return this.poll();
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  /** degraded 的原因原文；健康为 null。 */
  snapshot = (): string | null => this.degraded;

  private async poll(): Promise<void> {
    this.polls += 1;
    let next = this.degraded;
    try {
      next = runtimeDegraded(await (this.opts.fetchHealth ?? defaultFetchHealth)());
    } catch {
      /* 拉不到：沿用上一次的结论 */
    }
    if (next !== this.degraded) {
      this.degraded = next;
      for (const l of this.listeners) l();
    }
    if (this.stopped) return;
    // refresh() 与在途的定时 poll 可能同时收尾：只留一个定时器。
    if (this.timer) clearTimeout(this.timer);
    const delay = this.degraded !== null ? (this.opts.degradedMs ?? 10_000) : (this.opts.okMs ?? 60_000);
    this.timer = setTimeout(() => void this.poll(), delay);
  }
}
