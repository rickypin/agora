/**
 * 前端唯一的网络出口（A36 不变量 9：客户端零特权，一切功能经节点 API 完成）。
 *
 * 全仓库只有这个文件允许出现 `fetch(`，由 `tests/arch_boundary.rs` 守卫；WebSocket 的两个
 * 连接点（events / terminal）在各自文件里，守卫要求 URL 字面量同行以 `/api/` 开头。
 *
 * 这里除了收口还多一道**运行时**校验：`assert_only_under` 那种文本守卫拦得住"新写一个
 * fetch"，拦不住"经 net.ts 请求一个 /api/ 之外的地址"。前缀检查在真正发请求前抛错，
 * 静态与动态各守一半（agora-gwm）。
 */

export const API_PREFIX = "/api/";

/**
 * 默认请求期限（agora-axz0）：启动链路（`/api/health`、`/api/auth/devices`、`/api/sessions`）
 * 以前是裸 `fetch`，半开 TCP（移动网换网 / tailnet 断线 / NAT 重绑定）上可以一直不 settle，
 * 门页就永远停在「凭据：检查中…」，收件箱把「拿不到」画成「没有等你的事」。
 *
 * 20 s 与 `api.ts` 的写请求期限（30 s）分开：读探针宁可早失败、让 UI 说人话；写请求的
 * 语义不同（可能已经落库），30 s 的判定与文案在 `api.ts` 里，调用方自己带 signal 时不在这里
 * 再套一层（`init.signal` 存在 → 原样交给 fetch）。
 */
export const REQUEST_TIMEOUT_MS = 20_000;

export type FetchLike = (url: string, init: RequestInit) => Promise<Response>;

export function apiFetch(
  path: string,
  init: RequestInit = {},
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): Promise<Response> {
  if (!path.startsWith(API_PREFIX)) {
    throw new Error(`前端只能请求 ${API_PREFIX} 下的地址，收到: ${path}`);
  }
  if (init.signal || timeoutMs <= 0) return fetch(path, init);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  return fetch(path, { ...init, signal: controller.signal }).finally(() => clearTimeout(timer));
}
