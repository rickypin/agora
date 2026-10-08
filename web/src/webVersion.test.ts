/**
 * "手机上的前端是不是最新"（agora-xu12）。
 *
 * 判据只能来自服务端：iOS 主屏 PWA 会把 start_url 钉在缓存里，行为上分辨不出新旧（2026-10-08
 * 用户两次报同一个问题、我这边的 WebKit 全是新的）。保守规则：服务端没报或读到空值一律 unknown，
 * 不提示——不能因为一个拿不准的比较让用户天天点"重新加载"。
 */
import { describe, expect, it } from "vitest";
import { checkWebBuild } from "./webVersion";

describe("前端构建号比对（agora-xu12）", () => {
  it("两边一致 = current", () => {
    expect(checkWebBuild({ web_build: "2026-10-08T03:16Z" }, "2026-10-08T03:16Z")).toEqual({
      kind: "current",
      build: "2026-10-08T03:16Z",
    });
  });

  it("不一致 = stale，两个值都带上（提示里要写服务端那份）", () => {
    expect(checkWebBuild({ web_build: "2026-10-08T04:00Z" }, "2026-10-08T03:16Z")).toEqual({
      kind: "stale",
      build: "2026-10-08T03:16Z",
      served: "2026-10-08T04:00Z",
    });
  });

  it("服务端没报（旧节点）/ 报空 / 响应不是对象 = unknown，不提示", () => {
    for (const body of [{}, { web_build: null }, { web_build: "  " }, null, "nope", 42]) {
      expect(checkWebBuild(body, "2026-10-08T03:16Z"), JSON.stringify(body)).toEqual({
        kind: "unknown",
        build: "2026-10-08T03:16Z",
      });
    }
  });

  it("本页构建号是 dev（直接 vite build）时不比，免得永远提示", () => {
    expect(checkWebBuild({ web_build: "2026-10-08T03:16Z" }, "dev").kind).toBe("unknown");
  });
});
