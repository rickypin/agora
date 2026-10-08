/**
 * "手机上的前端是不是最新"（agora-xu12）。
 *
 * 起因：iOS 主屏 PWA 会把 start_url 钉在缓存里、从任务切换器回来常常只是"恢复旧页面"，于是修完的
 * 界面在手机上一直不出现，而我这边的 WebKit/模拟器全是新的——两边各说各话，每轮都在猜。判据只能来自
 * 服务端：页面构建时烤进 `build`（见 build.ts），服务端把它内嵌的那份经 `/api/system` 的
 * `web_build` 报出来；两个值不同就是"该硬刷新了"。
 *
 * 保守规则：服务端没报（旧节点 / 前端没构建）或读到空值 → `unknown`，**不提示**。宁可少提示，也不能
 * 因为一个拿不准的比较让用户天天去点"重新加载"。
 */
import { webBuild } from "./build";

export type WebVerdict =
  | { kind: "current"; build: string }
  | { kind: "stale"; build: string; served: string }
  | { kind: "unknown"; build: string };

/** 对 `/api/system` 的原始响应体 + 本页构建号下结论（纯函数）。 */
export function checkWebBuild(system: unknown, page: string = webBuild()): WebVerdict {
  const served =
    typeof system === "object" && system !== null ? (system as { web_build?: unknown }).web_build : undefined;
  // 本页构建号是 dev（直接 `vite build` / 单测）时没有可比性；服务端没报（旧节点 / 前端没构建）也一样。
  if (typeof served !== "string" || served.trim() === "" || page.trim() === "" || page.trim() === "dev") {
    return { kind: "unknown", build: page };
  }
  if (served.trim() === page.trim()) return { kind: "current", build: page };
  return { kind: "stale", build: page, served: served.trim() };
}
