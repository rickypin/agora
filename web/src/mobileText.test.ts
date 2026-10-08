/**
 * 手机端字号档位的读写（agora-x70t.xsgz）。与 attention.ts 的"看过"集合同一条纪律：存储可能
 * 不可用（隐私窗口 / 被禁），读不出或值不认识就退回默认档——丢了的代价只是回到标准字号。
 */
import { describe, expect, it } from "vitest";
import {
  DEFAULT_MOBILE_TEXT_SIZE,
  MOBILE_TEXT_SIZES,
  MOBILE_TEXT_STORAGE_KEY,
  loadMobileTextSize,
  storeMobileTextSize,
} from "./mobileText";

function fake(initial: Record<string, string> = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    raw: (k: string) => map.get(k),
  };
}

describe("字号档位", () => {
  it("默认是标准档，且每一档都不低于 iOS 的 16px 输入门槛或另有下限（见 mobileCss.test.ts）", () => {
    expect(DEFAULT_MOBILE_TEXT_SIZE).toBe("m");
    expect(MOBILE_TEXT_SIZES.find((s) => s.key === "m")!.px).toBe(17);
    for (const s of MOBILE_TEXT_SIZES) expect(s.px).toBeGreaterThanOrEqual(15);
    // 档位从小到大，且没有重复——设置页的四个按钮就是这个顺序。
    const pxs = MOBILE_TEXT_SIZES.map((s) => s.px);
    expect([...pxs].sort((a, b) => a - b)).toEqual(pxs);
    expect(new Set(pxs).size).toBe(pxs.length);
  });

  it("读：空存储 / 不认识的值 / 存储抛异常都退回默认档", () => {
    expect(loadMobileTextSize(fake())).toBe("m");
    expect(loadMobileTextSize(fake({ [MOBILE_TEXT_STORAGE_KEY]: "xxl" }))).toBe("m");
    expect(loadMobileTextSize(fake({ [MOBILE_TEXT_STORAGE_KEY]: "" }))).toBe("m");
    const hostile = {
      getItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadMobileTextSize(hostile)).toBe("m");
    expect(loadMobileTextSize(null)).toBe("m");
  });

  it("写读往返；存储拒绝写入时不抛", () => {
    const s = fake();
    for (const size of MOBILE_TEXT_SIZES.map((x) => x.key)) {
      storeMobileTextSize(size, s);
      expect(s.raw(MOBILE_TEXT_STORAGE_KEY)).toBe(size);
      expect(loadMobileTextSize(s)).toBe(size);
    }
    const hostile = {
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(() => storeMobileTextSize("l", hostile)).not.toThrow();
  });
});
