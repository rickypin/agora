import { describe, expect, it, vi } from "vitest";
import { registerPwa, shouldRegisterPwa, type PwaEnv } from "./pwa";

describe("shouldRegisterPwa", () => {
  it("only registers in a secure context on a non-loopback host", () => {
    expect(shouldRegisterPwa({ secure: true, hostname: "zuan.tail5fb9b.ts.net" })).toBe(true);
    expect(shouldRegisterPwa({ secure: false, hostname: "zuan.tail5fb9b.ts.net" })).toBe(false);
    // 127.0.0.1 在浏览器里也是 secure context：不排除它，开发机一开页面就装上 SW，
    // 升级后的旧 bundle 会钉在浏览器里（A39；agora-thc.4）。
    expect(shouldRegisterPwa({ secure: true, hostname: "127.0.0.1" })).toBe(false);
    expect(shouldRegisterPwa({ secure: true, hostname: "localhost" })).toBe(false);
    expect(shouldRegisterPwa({ secure: true, hostname: "::1" })).toBe(false);
  });
});

describe("registerPwa", () => {
  function env(patch: Partial<PwaEnv> = {}): PwaEnv {
    return {
      supported: true,
      secure: true,
      hostname: "zuan.tail5fb9b.ts.net",
      register: vi.fn(async () => ({})),
      ...patch,
    };
  }

  it("registers /sw.js from the https node", () => {
    const register = vi.fn(async () => ({}));
    expect(registerPwa(env({ register }))).toBe(true);
    expect(register).toHaveBeenCalledWith("/sw.js");
  });

  it("does nothing without serviceWorker support, on http, or on loopback", () => {
    for (const patch of [{ supported: false }, { secure: false }, { hostname: "127.0.0.1" }]) {
      const register = vi.fn(async () => ({}));
      expect(registerPwa(env({ ...patch, register }))).toBe(false);
      expect(register).not.toHaveBeenCalled();
    }
  });
});
