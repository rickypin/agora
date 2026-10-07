import { describe, expect, it } from "vitest";
import { isMobilePath, parseSessionTarget } from "./mobileRoute";

describe("isMobilePath", () => {
  it("matches /m and its subpaths, not lookalikes", () => {
    expect(isMobilePath("/m")).toBe(true);
    expect(isMobilePath("/m/")).toBe(true);
    expect(isMobilePath("/m/anything")).toBe(true);
    expect(isMobilePath("/")).toBe(false);
    expect(isMobilePath("/mail")).toBe(false);
    expect(isMobilePath("/workspace/m")).toBe(false);
  });
});

describe("parseSessionTarget", () => {
  it("splits node and id out of ?session=<node>:<id>", () => {
    expect(parseSessionTarget("?session=zuan:abc-123")).toEqual({ node: "zuan", id: "abc-123" });
    expect(parseSessionTarget("?keep=1&session=mac:d4na")).toEqual({ node: "mac", id: "d4na" });
  });

  it("returns null for missing or malformed values instead of guessing a row", () => {
    expect(parseSessionTarget("")).toBeNull();
    expect(parseSessionTarget("?session=")).toBeNull();
    expect(parseSessionTarget("?session=abc")).toBeNull();
    expect(parseSessionTarget("?session=:abc")).toBeNull();
    expect(parseSessionTarget("?session=zuan:")).toBeNull();
  });
});
