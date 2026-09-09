// @vitest-environment jsdom
import { afterEach, expect, it } from "vitest";
import { clamp, DEFAULT, loadWidth, SIDEBAR_WIDTH_KEY, storeWidth } from "./sidebarWidth";

afterEach(() => {
  localStorage.removeItem(SIDEBAR_WIDTH_KEY);
});

it("clamp keeps width within [220, 50% viewport] (agora-uvd.5)", () => {
  const vw = 1000;
  expect(clamp(100, vw)).toBe(220);
  expect(clamp(220, vw)).toBe(220);
  expect(clamp(260, vw)).toBe(260);
  expect(clamp(500, vw)).toBe(500);
  expect(clamp(501, vw)).toBe(500);
  expect(clamp(9999, vw)).toBe(500);
});

it("loadWidth falls back on garbage / throwing storage (agora-uvd.5)", () => {
  expect(loadWidth({ getItem: () => "not-a-number" })).toBe(DEFAULT);
  expect(loadWidth({ getItem: () => "" })).toBe(DEFAULT);
  expect(loadWidth({ getItem: () => "   " })).toBe(DEFAULT);
  expect(loadWidth({ getItem: () => null })).toBe(DEFAULT);
  expect(loadWidth(null)).toBe(DEFAULT);
  const throwing = {
    getItem: (): string | null => {
      throw new Error("denied");
    },
  };
  expect(loadWidth(throwing)).toBe(DEFAULT);
  const mem = new Map<string, string>();
  const storage = {
    getItem: (k: string) => mem.get(k) ?? null,
    setItem: (k: string, v: string) => void mem.set(k, v),
  };
  storeWidth(360, storage);
  expect(mem.get(SIDEBAR_WIDTH_KEY)).toBe("360");
  expect(loadWidth(storage)).toBe(360);
});
