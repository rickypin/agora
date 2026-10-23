// @vitest-environment jsdom
/**
 * 崩溃边界（agora-bkbg）：render 阶段的异常会把根清空；这里钉住"换成可复制的错误页"，
 * 而不是一片空白 + 手机上看不到的 console。
 */
import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";
import { capturedErrors, clearCapturedErrors } from "./errors";

function Boom(): ReactNode {
  throw new Error("render-boom");
}

beforeEach(() => {
  clearCapturedErrors();
  // React 自己会把错误打到 console.error（dev 还有组件栈），测试里不需要这份噪音。
  vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ErrorBoundary (agora-bkbg)", () => {
  it("把卸载后的空白换成带构建号与错误栈的可复制错误页", () => {
    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );

    expect(screen.getByTestId("app-crash")).toBeTruthy();
    const detail = (screen.getByTestId("app-crash-detail") as HTMLTextAreaElement).value;
    expect(detail).toContain("render-boom");
    expect(detail).toContain("前端 dev");
    expect(capturedErrors().some((entry) => entry.message === "render-boom")).toBe(true);
  });

  it("没有错误时原样子树", () => {
    render(
      <ErrorBoundary>
        <p>ok</p>
      </ErrorBoundary>,
    );
    expect(screen.getByText("ok")).toBeTruthy();
    expect(screen.queryByTestId("app-crash")).toBeNull();
  });
});
