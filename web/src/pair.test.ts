import { describe, expect, it } from "vitest";
import { extractPairToken, extractPairTokenFromText } from "./pair";

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

// ---------- 门页粘贴（agora-thc.1） ----------

describe("extractPairTokenFromText", () => {
  it("accepts a full link, a fragment, and a bare token", () => {
    const token = "abc-DEF_123".padEnd(43, "x");
    expect(extractPairTokenFromText(`https://zuan.example:7681/#pair=${token}`)).toBe(token);
    expect(extractPairTokenFromText(`  #pair=${token}  `)).toBe(token);
    expect(extractPairTokenFromText(token)).toBe(token);
  });

  it("rejects junk instead of guessing", () => {
    expect(extractPairTokenFromText("")).toBeNull();
    expect(extractPairTokenFromText("https://zuan.example:7681/")).toBeNull();
    expect(extractPairTokenFromText("short")).toBeNull();
    expect(extractPairTokenFromText("#pair=has space")).toBeNull();
  });
});
