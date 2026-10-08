/**
 * 安全区自适应（agora-xu12）：判定"这 62px 系统是不是已经让出去了"。
 *
 * 实测依据（2026-10-08，iPhone 18.5，同一台机器两分钟内的两份诊断报告）：
 *   02:44 inner=[402,874] screen=[402,874] → 主屏 PWA 全屏，env 的 62/34 必须照加；
 *   03:03 inner=[402,812] screen=[402,874] → 视口矮了正好一个状态栏（62），此时再加就是白白空一条。
 */
import { describe, expect, it } from "vitest";
import { resolveInsets } from "./mobileInsets";

describe("安全区自适应（agora-xu12）", () => {
  it("视口覆盖整屏（主屏 PWA 全屏）时照加 env 的四条", () => {
    const it0 = resolveInsets(874, 874, { top: 62, bottom: 34 });
    expect(it0.fullscreen).toBe(true);
    expect([it0.top, it0.bottom]).toEqual([62, 34]);
  });

  it("视口比屏幕矮（系统已让出 chrome）时上下都不再加", () => {
    const it0 = resolveInsets(812, 874, { top: 62, bottom: 34 });
    expect(it0.fullscreen).toBe(false);
    expect([it0.top, it0.bottom]).toEqual([0, 0]);
    // 原始 env 仍留在报告里：判断"为什么这台上不加上"要能看出来。
    expect([it0.envTop, it0.envBottom]).toEqual([62, 34]);
  });

  it("容差 4px 以内仍算全屏（iOS 的地址栏收起动画有亚像素差）", () => {
    expect(resolveInsets(871, 874, { top: 62, bottom: 34 }).top).toBe(62);
    expect(resolveInsets(869, 874, { top: 62, bottom: 34 }).top).toBe(0);
  });

  it("拿不到 screen（桌面浏览器 / 单测环境）时不猜：按全屏算，env 本来就是 0", () => {
    expect(resolveInsets(800, 0, { top: 0, bottom: 0 }).top).toBe(0);
  });
});
