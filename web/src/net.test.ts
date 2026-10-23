/**
 * 前端唯一网络出口的期限（agora-axz0）。
 *
 * 启动链路（health / 凭据 / 全量快照）必须是**有界**的：半开 TCP 上 `fetch` 可以一直不
 * settle，门页就永远停在「凭据：检查中…」、收件箱把「拿不到」画成「没有等你的事」。
 * 写请求的期限（30 s）在 `api.ts` 的 `sessionApi.call` 里，那一侧自己带 signal——这里
 * 不能再套一层，否则两层计时器会互相打架。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { apiFetch, REQUEST_TIMEOUT_MS } from "./net";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("apiFetch 的期限（agora-axz0）", () => {
  it("挂住的请求在默认期限后被 abort", async () => {
    vi.useFakeTimers();
    let signal: AbortSignal | undefined;
    const fetchMock = vi.fn((_url: string, init: RequestInit = {}) => {
      signal = init.signal ?? undefined;
      return new Promise<Response>((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    const pending = apiFetch("/api/health");
    const outcome = pending.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS - 1);
    expect(signal?.aborted).toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    expect((await outcome as Error).name).toBe("AbortError");
    expect(signal?.aborted).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("调用方自己带 signal 时原样交给 fetch（sessionApi 的 30 s 判定在 api.ts）", async () => {
    const controller = new AbortController();
    const fetchMock = vi.fn(async (_url: string, _init: RequestInit) => new Response("{}"));
    vi.stubGlobal("fetch", fetchMock);

    await apiFetch("/api/sessions", { signal: controller.signal });
    expect(fetchMock.mock.calls[0]?.[1]?.signal).toBe(controller.signal);
  });

  it("api/ 之外的地址在发请求之前就抛错", () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    expect(() => apiFetch("/assets/index-OLD.js")).toThrow(/\/api\//);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
