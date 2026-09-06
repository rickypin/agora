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

/**
 * peers 段里 `last_error` 的五个值（docs/spec/api.md Health；模型 src/peer/state.rs）。按类型不按文本。
 * `misconfigured`（agora-41e；ADR-003 D3）：本机这一行 peers[] 字面上就用不了（token_file 权限 / 属主 /
 * 内容、url 不是 https、指纹不合法），节点不重试、按固定间隔重读配置——所以它是唯一 `retrying` 恒 false 的离线。
 */
export type PeerError = "incompatible_version" | "fingerprint_mismatch" | "unauthorized" | "unreachable" | "misconfigured";

const PEER_ERRORS: readonly PeerError[] = ["incompatible_version", "fingerprint_mismatch", "unauthorized", "unreachable", "misconfigured"];

export function isPeerError(v: unknown): v is PeerError {
  return typeof v === "string" && (PEER_ERRORS as readonly string[]).includes(v);
}

/** peers 段里一项：一个 peer 在本节点眼里的状态。 */
export interface PeerHealth {
  online: boolean;
  /** 上次成功交互，本节点时钟打的 UTC 文本；没连上过为 null。离线后保留（不变量 8）。 */
  last_seen: string | null;
  /** 下一次网络重试已排定；`misconfigured` 时恒 false（重试改不了文件，节点在定时重读配置）。 */
  retrying: boolean;
  last_error: PeerError | null;
}

/** 带 principal 才有的完整形态（MISSION §10.3）；未认证只拿到 PublicHealth。 */
export interface FullHealth extends PublicHealth {
  runtime?: RuntimeHealth;
  database?: boolean;
  peers?: Record<string, PeerHealth>;
}

/** 本机 + 每个 peer 的状态快照，Header 的数据源（agora-7ku.12）。 */
export interface NodesHealth {
  /** 上一次拉 /api/health 成功了吗（本机的"在线"）；还没拉过为 null。 */
  reachable: boolean | null;
  peers: Record<string, PeerHealth>;
}

/** 把 peers 段整理成形：缺字段补默认、不认识的 last_error 当 null，别让一条坏数据毁掉整个 Header。 */
export function peersOf(h: unknown): Record<string, PeerHealth> {
  if (typeof h !== "object" || h === null) return {};
  const peers = (h as FullHealth).peers;
  if (typeof peers !== "object" || peers === null) return {};
  const out: Record<string, PeerHealth> = {};
  for (const [name, p] of Object.entries(peers)) {
    if (typeof p !== "object" || p === null) continue;
    const v = p as Partial<PeerHealth>;
    out[name] = {
      online: v.online === true,
      last_seen: typeof v.last_seen === "string" ? v.last_seen : null,
      retrying: v.retrying === true,
      last_error: isPeerError(v.last_error) ? v.last_error : null,
    };
  }
  return out;
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
  /** 同一次拉取顺手带出的节点状态（不另起轮询，MISSION §10.3）。引用只在内容变了才换：useSyncExternalStore 靠它判等。 */
  private nodes: NodesHealth = { reachable: null, peers: {} };
  private listeners = new Set<() => void>();
  /** 节点状态单独一组订阅：peer 上上下下不该让横幅重渲染，反过来也一样。 */
  private nodeListeners = new Set<() => void>();
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

  /** 订阅节点状态（本机可达性 + 每个 peer）的变化；与 `subscribe`（degraded）各自独立。 */
  subscribeNodes = (l: () => void): (() => void) => {
    this.nodeListeners.add(l);
    return () => this.nodeListeners.delete(l);
  };

  /** 本机可达性 + 每个 peer 的状态；Header 的数据源。 */
  nodesSnapshot = (): NodesHealth => this.nodes;

  private async poll(): Promise<void> {
    this.polls += 1;
    let next = this.degraded;
    let nodes = this.nodes;
    try {
      const report = await (this.opts.fetchHealth ?? defaultFetchHealth)();
      next = runtimeDegraded(report);
      nodes = { reachable: true, peers: peersOf(report) };
    } catch {
      // 拉不到：degraded 沿用上一次的结论（不闪）；本机标不可达，peer 保留最后一眼（不变量 8 的前端半边）。
      nodes = { reachable: false, peers: this.nodes.peers };
    }
    if (next !== this.degraded) {
      this.degraded = next;
      for (const l of this.listeners) l();
    }
    if (JSON.stringify(nodes) !== JSON.stringify(this.nodes)) {
      this.nodes = nodes;
      for (const l of this.nodeListeners) l();
    }
    if (this.stopped) return;
    // refresh() 与在途的定时 poll 可能同时收尾：只留一个定时器。
    if (this.timer) clearTimeout(this.timer);
    const delay = this.degraded !== null ? (this.opts.degradedMs ?? 10_000) : (this.opts.okMs ?? 60_000);
    this.timer = setTimeout(() => void this.poll(), delay);
  }
}

// ---------- API 版本（docs/spec/api.md「api_version 兼容规则」；MISSION §7.3；agora-7ku.4） ----------

/** `GET /api/system` 里的 `api_version`：`{ major, minor }`。 */
export interface ApiVersion {
  major: number;
  minor: number;
}

/**
 * 页面构建时对着的 API 版本。与 src/api/version.rs 的 `API_VERSION` 同步改——
 * Rust 单测 `page_is_built_against_the_same_api_version` 读这一行钉住两边一致，
 * 所以这行的写法（`{ major: N, minor: M }` 字面量）别改成别的形态。
 */
export const API_VERSION: ApiVersion = { major: 1, minor: 4 };

/**
 * 版本比对的结论，按类型分类（MISSION §2.3 规则 10）：
 * - compatible：同 major，minor 谁大谁小都能对话；
 * - major_mismatch：形态可能变了，读了就是错读；
 * - unreadable：`api_version` 缺失或不是 `{ major, minor }`——旧二进制的裸整数也算，不猜成 1.0。
 */
export type VersionVerdict =
  | { kind: "compatible"; node: ApiVersion; page: ApiVersion }
  | { kind: "major_mismatch"; node: ApiVersion; page: ApiVersion }
  | { kind: "unreadable"; page: ApiVersion };

function isVersionNumber(n: unknown): n is number {
  return typeof n === "number" && Number.isInteger(n) && n >= 0;
}

/** 读 `api_version` 的值；不是两个非负整数的对象就是 null。 */
export function parseApiVersion(v: unknown): ApiVersion | null {
  if (typeof v !== "object" || v === null) return null;
  const { major, minor } = v as Partial<Record<"major" | "minor", unknown>>;
  if (!isVersionNumber(major) || !isVersionNumber(minor)) return null;
  return { major, minor };
}

/** 对 `GET /api/system` 的原始响应体下结论；`page` 缺省是本页面构建时的版本。 */
export function checkApiVersion(system: unknown, page: ApiVersion = API_VERSION): VersionVerdict {
  const raw = typeof system === "object" && system !== null ? (system as { api_version?: unknown }).api_version : undefined;
  const node = parseApiVersion(raw);
  if (node === null) return { kind: "unreadable", page };
  if (node.major !== page.major) return { kind: "major_mismatch", node, page };
  return { kind: "compatible", node, page };
}

export function formatApiVersion(v: ApiVersion): string {
  return `${v.major}.${v.minor}`;
}

/**
 * 不兼容时横幅的文案；兼容或还没有结论 → null（什么都不显示）。
 * 刷新还是升级按数字判：节点比页面新，页面是升级前留下的旧标签页，刷新就好；
 * 节点比页面旧（或读不出版本），刷新只会拿到同样旧的节点，得升级节点。
 */
export function versionBlocked(v: VersionVerdict | null): string | null {
  if (v === null || v.kind === "compatible") return null;
  const page = formatApiVersion(v.page);
  if (v.kind === "unreadable") {
    return `节点没有报告可识别的 API 版本，页面按 ${page} 构建，请升级节点后刷新页面`;
  }
  const hint = v.node.major > v.page.major ? "请刷新页面" : "请升级节点后刷新页面";
  return `节点 API 版本 ${formatApiVersion(v.node)}，页面按 ${page} 构建，${hint}`;
}

async function defaultFetchSystem(): Promise<unknown> {
  const resp = await apiFetch("/api/system");
  if (!resp.ok) throw new Error(`GET /api/system ${resp.status}`);
  try {
    return await resp.json();
  } catch {
    // 200 却不是 JSON：这是节点在说别的协议，不是网络抖动——交给 checkApiVersion 判成 unreadable。
    return undefined;
  }
}

export interface VersionWatcherOptions {
  /** 拉一次 `/api/system`；默认 `apiFetch("/api/system")`。 */
  fetchSystem?: () => Promise<unknown>;
  /** 页面自己的版本；默认 `API_VERSION`，测试用来造不兼容。 */
  page?: ApiVersion;
}

/**
 * 盯着节点的 `api_version`（agora-7ku.4）。
 *
 * 不轮询：`check()` 由 Workspace 在 `/api/events` 每次连上时调一次（启动的首连、断流后的重连）——
 * 升级节点必然重启 daemon、WS 必然断一次，换代总能在重连时被看见。拉不到（daemon 正在重启 /
 * 5xx / 401）沿用上一次结论，不在"不兼容"与"正常"之间闪；一次都没成功过就是 null（无结论，
 * 页面照常渲染——没有结论不等于不兼容，而且拉不到 /api/system 时会话也拉不到，无所谓错读）。
 */
export class VersionWatcher {
  private verdict: VersionVerdict | null = null;
  /** 节点自报的本机 `node`（agora-7ku.5）：Header 本机那一枚的名字、侧栏行"是不是本机"都看它；一次都没拉到为 null。 */
  private node: string | null = null;
  private listeners = new Set<() => void>();
  /** 查过几次（测试断言用）。 */
  checks = 0;

  constructor(private readonly opts: VersionWatcherOptions = {}) {}

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  snapshot = (): VersionVerdict | null => this.verdict;

  /** 本机 node.id（`/api/system` 的 `node`）；与 `snapshot` 共用 `subscribe`，同一次拉取带出，不另起轮询。 */
  nodeSnapshot = (): string | null => this.node;

  async check(): Promise<void> {
    this.checks += 1;
    let next = this.verdict;
    let node = this.node;
    try {
      const system = await (this.opts.fetchSystem ?? defaultFetchSystem)();
      next = checkApiVersion(system, this.opts.page ?? API_VERSION);
      node = localNodeOf(system) ?? node;
    } catch {
      /* 拉不到：沿用上一次的结论 */
    }
    if (JSON.stringify(next) === JSON.stringify(this.verdict) && node === this.node) return;
    this.verdict = next;
    this.node = node;
    for (const l of this.listeners) l();
  }
}

/** `/api/system` 里的 `node`（本机 node.id）；不是非空字符串就是 null。 */
export function localNodeOf(system: unknown): string | null {
  if (typeof system !== "object" || system === null) return null;
  const node = (system as { node?: unknown }).node;
  return typeof node === "string" && node !== "" ? node : null;
}
