import { expect, it } from "vitest";
import { nodeHue } from "./nodeColor";

it("hue is stable for a name and differs for mac vs zuan (A49)", () => {
  expect(nodeHue("mac")).toBe(nodeHue("mac"));
  expect(nodeHue("zuan")).toBe(nodeHue("zuan"));
  expect(nodeHue("mac")).not.toBe(nodeHue("zuan"));
  // 8 档、相邻 45°：落在色板上，而不是任意 0–359。
  expect(nodeHue("mac") % 45).toBe(0);
  expect(nodeHue("zuan") % 45).toBe(0);
  expect(nodeHue("mac")).toBeGreaterThanOrEqual(0);
  expect(nodeHue("mac")).toBeLessThan(360);
});
