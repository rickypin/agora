import { describe, expect, it } from "vitest";
import { extractPairToken } from "./pair";

describe("extractPairToken", () => {
  it("reads the base64url token from the fragment", () => {
    expect(extractPairToken("#pair=abc-DEF_123")).toBe("abc-DEF_123");
  });
  it("ignores anything else", () => {
    expect(extractPairToken("")).toBeNull();
    expect(extractPairToken("#pair=")).toBeNull();
    expect(extractPairToken("#pair=a b")).toBeNull();
    expect(extractPairToken("#other=1")).toBeNull();
    expect(extractPairToken("?pair=abc")).toBeNull();
  });
});
