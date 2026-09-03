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

export type FetchLike = (url: string, init: RequestInit) => Promise<Response>;

export function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  if (!path.startsWith(API_PREFIX)) {
    throw new Error(`前端只能请求 ${API_PREFIX} 下的地址，收到: ${path}`);
  }
  return fetch(path, init);
}
