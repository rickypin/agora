import { beforeEach, describe, expect, it, vi } from "vitest";
import { extractPairToken, extractPairTokenFromText, redeemPairOnce } from "./pair";
import { apiFetch } from "./net";

vi.mock("./net", () => ({ API_PREFIX: "/api/", apiFetch: vi.fn() }));

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

// ---------- 兑换缓存（agora-3w8 / agora-axz0） ----------

describe("redeemPairOnce", () => {
  beforeEach(() => {
    // 同一个模块级 mock 跨测试复用：只清调用记录，不清 once 队列。
    vi.mocked(apiFetch).mockClear();
  });

  it("网络失败不进缓存：重试会再发一次（token 没有被服务端用掉）", async () => {
    const mocked = vi.mocked(apiFetch);
    mocked.mockRejectedValueOnce(new Error("network down"));
    mocked.mockResolvedValueOnce({
      ok: true,
      json: async () => ({ device: { id: "d1", name: "test" } }),
    } as unknown as Response);

    await expect(redeemPairOnce("tok-retry")).rejects.toThrow("network down");
    await expect(redeemPairOnce("tok-retry")).resolves.toEqual({ id: "d1", name: "test" });
    expect(mocked).toHaveBeenCalledTimes(2);
  });

  it("服务端说链接无效（401 → null）是确定答案：同一 token 不再发第二次", async () => {
    const mocked = vi.mocked(apiFetch);
    mocked.mockResolvedValueOnce({ ok: false, status: 401, json: async () => ({}) } as unknown as Response);

    await expect(redeemPairOnce("tok-invalid")).resolves.toBeNull();
    await expect(redeemPairOnce("tok-invalid")).resolves.toBeNull();
    expect(mocked).toHaveBeenCalledTimes(1);
  });
});
