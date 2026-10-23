import { describe, expect, it } from "vitest";
import sw from "../public/sw.js?raw";

// sw.js 是 public/ 里的纯脚本，不经过 Vite 打包；用 ?raw 把它当数据钉关键路径（不引 @types/node）。
// 推送处理另有 sw.test.ts（动态 import 同一份字节）；这里只钉离线壳（agora-vmt6）。
// 真正的离线行为由 HTTPS 代理 + 真 Chromium 的代检验证（见 agora-vmt6 的 notes）。
describe("service worker 的离线壳（agora-vmt6）", () => {
  it("拦截导航与 assets，但 /api 直连、不缓存", () => {
    expect(sw).toContain('addEventListener("fetch"');
    expect(sw).toContain('request.mode === "navigate"');
    const guard = sw.indexOf('if (url.pathname.startsWith("/api/")) return;');
    expect(guard).toBeGreaterThan(-1);
    // 这条 return 必须在导航 / assets 分支之前，否则 /api 会被壳缓存吞掉。
    expect(sw.slice(guard)).toContain('if (request.mode === "navigate")');
  });

  it("导航网络优先 + 超时回缓存，安装时预缓存 /m 与 /", () => {
    expect(sw).toContain('const SHELL_CACHE = "agora-shell-');
    expect(sw).toContain('const SHELL_PATHS = ["/m", "/"]');
    expect(sw).toContain("NAV_TIMEOUT_MS");
    expect(sw).toContain("Promise.race");
    expect(sw).toContain("skipWaiting");
    expect(sw).toContain("clients.claim");
  });

  it("缓存前去掉 Content-Encoding，离线回放不会被二次解码", () => {
    expect(sw).toContain('headers.delete("content-encoding")');
    expect(sw).toContain('headers.delete("content-length")');
  });
});
