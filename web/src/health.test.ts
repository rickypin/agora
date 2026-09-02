import { describe, expect, it } from "vitest";
import { isHealthy } from "./health";

describe("isHealthy", () => {
  it("accepts the public subset", () => {
    expect(isHealthy({ status: "ok" })).toBe(true);
  });
  it("rejects anything else", () => {
    expect(isHealthy({ status: "degraded" })).toBe(false);
    expect(isHealthy(null)).toBe(false);
    expect(isHealthy("ok")).toBe(false);
  });
});
