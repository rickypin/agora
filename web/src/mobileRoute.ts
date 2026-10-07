/**
 * /m 的路由判据、深链解析与窄屏判据（agora-thc.5；MISSION §6.9）。纯逻辑与一个 matchMedia 钩子，
 * App 与 MobileApp 共用——手机入口的判据只有这一处，免得路由与壳各写一份、改一处漏一处。
 */
import { useSyncExternalStore } from "react";

/** 地址栏是不是手机收件箱：`/m` 与它下面的路径（PWA 的 `start_url`，docs/spec/ux.md）。 */
export function isMobilePath(pathname: string): boolean {
  return pathname === "/m" || pathname.startsWith("/m/");
}

export interface SessionTarget {
  node: string;
  id: string;
}

/**
 * `/m?session=<node>:<id>`（推送深链；推送载荷只带标题与会话身份，见 docs/spec/api.md）。
 * 缺 node / id、或没有 session 参数 → null：不猜、不跳错行。
 */
export function parseSessionTarget(search: string): SessionTarget | null {
  const raw = new URLSearchParams(search).get("session");
  if (!raw) return null;
  const at = raw.indexOf(":");
  if (at <= 0 || at === raw.length - 1) return null;
  return { node: raw.slice(0, at), id: raw.slice(at + 1) };
}

const NARROW_QUERY = "(max-width: 699px)";

/**
 * 桌面窗口窄于 700 px（MISSION §6.6 的 `/m` 替代句）：不再做抽屉式全功能布局，直接引导到 /m。
 * jsdom 没有 matchMedia（测试环境）时恒 false，桌面路径照常渲染。
 */
export function useNarrow(): boolean {
  return useSyncExternalStore(
    (notify) => {
      const mql = typeof window.matchMedia === "function" ? window.matchMedia(NARROW_QUERY) : null;
      if (!mql) return () => {};
      mql.addEventListener("change", notify);
      return () => mql.removeEventListener("change", notify);
    },
    () => (typeof window.matchMedia === "function" ? window.matchMedia(NARROW_QUERY).matches : false),
    () => false,
  );
}
