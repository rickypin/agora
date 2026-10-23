/**
 * 崩溃现场（agora-bkbg）：有界缓冲、任意抛出物的可读文本、错误页正文。
 * 纯逻辑 + 可注入的事件目标，node 环境直接测。
 */
import { beforeEach, describe, expect, it } from "vitest";
import {
  capturedErrors,
  clearCapturedErrors,
  crashReport,
  errorText,
  installErrorCapture,
  recordError,
  type ErrorTarget,
} from "./errors";

interface FakeTarget extends ErrorTarget {
  fire(type: "error" | "unhandledrejection", event: unknown): void;
  count(): number;
}

function fakeTarget(): FakeTarget {
  const listeners = new Map<string, (event: never) => void>();
  const target = {
    addEventListener(type: string, listener: (event: never) => void) {
      listeners.set(type, listener);
    },
    removeEventListener(type: string) {
      listeners.delete(type);
    },
    fire(type: "error" | "unhandledrejection", event: unknown) {
      listeners.get(type)?.(event as never);
    },
    count() {
      return listeners.size;
    },
  };
  return target as unknown as FakeTarget;
}

beforeEach(() => {
  clearCapturedErrors();
});

describe("错误采集（agora-bkbg）", () => {
  it("捕获 window 的 error 与 unhandledrejection，拆除后不再收", () => {
    const target = fakeTarget();
    const off = installErrorCapture(target);
    target.fire("error", { error: new Error("boom") });
    target.fire("unhandledrejection", { reason: "rejected-string" });
    target.fire("error", { message: "no-error-object" });

    expect(capturedErrors().map((entry) => entry.message)).toEqual(["boom", "rejected-string", "no-error-object"]);
    expect(capturedErrors()[0]?.stack).toContain("boom");

    off();
    expect(target.count()).toBe(0);
    target.fire("error", { error: new Error("after-off") });
    expect(capturedErrors()).toHaveLength(3);
  });

  it("只留最近 5 条，最旧的丢掉", () => {
    for (let i = 0; i < 7; i += 1) recordError("error", new Error(`e${i}`));
    expect(capturedErrors().map((entry) => entry.message)).toEqual(["e2", "e3", "e4", "e5", "e6"]);
  });

  it("任意抛出物都能说人话（字符串 / 对象 / null / 循环引用）", () => {
    expect(errorText(new Error("e"))).toBe("e");
    expect(errorText("plain")).toBe("plain");
    expect(errorText({ a: 1 })).toBe('{"a":1}');
    expect(errorText(null)).toBe("null");
    expect(errorText(undefined)).toBe("undefined");
    const loop: Record<string, unknown> = {};
    loop.self = loop;
    expect(errorText(loop)).toBe("[object Object]");
  });

  it("错误页正文带构建号、UA、错误栈与最近捕获", () => {
    recordError("rejection", "late-failure");
    const text = crashReport(new Error("render-boom"), "2026-10-24T00:00Z", "UA/1.0");
    expect(text).toContain("前端 2026-10-24T00:00Z");
    expect(text).toContain("UA/1.0");
    expect(text).toContain("render-boom");
    expect(text).toContain("late-failure");
  });
});
