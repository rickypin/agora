/**
 * 崩溃边界（agora-bkbg）：render / 生命周期里的异常会把整棵树卸载，屏幕剩下一片空白；
 * 手机上既没有控制台也没有"刷新"按钮。这里把它换成可复制的错误页：构建号、UA、错误栈、
 * 最近捕获的错误，外加一个重新加载。事件处理器里的异常不会到这里（React 不因此卸载），
 * 那一路由 `errors.ts` 的 window 采集兜住。
 */
import { Component, type ErrorInfo, type ReactNode } from "react";
import { webBuild } from "./build";
import { crashReport, errorText, recordError } from "./errors";
import { isMobilePath } from "./mobileRoute";
import { loadMobileTextSize } from "./mobileText";

interface Props {
  children: ReactNode;
}

interface State {
  error: unknown;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: unknown): State {
    return { error };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    // 组件栈只进内存（window 采集那份没有它）；控制台在桌面上仍然有 React 自己那条。
    recordError("error", error);
    if (info.componentStack) {
      recordError("error", new Error(`组件栈：${info.componentStack.trim()}`));
    }
  }

  render(): ReactNode {
    if (this.state.error === null) return this.props.children;
    const mobile = typeof window !== "undefined" && isMobilePath(window.location.pathname);
    const report = crashReport(
      this.state.error,
      webBuild(),
      typeof navigator === "undefined" ? "" : navigator.userAgent,
    );
    return (
      <main className={`gate${mobile ? " gate-mobile" : ""}`} data-text={mobile ? loadMobileTextSize() : undefined} data-testid="app-crash" role="alert">
        <h1>agora 出了点问题</h1>
        <p>这一屏没法继续画了。把下面的内容复制给维护者就能定位；也可以先重新加载。</p>
        <p>
          <button type="button" data-testid="app-crash-reload" onClick={() => window.location.reload()}>
            重新加载
          </button>
        </p>
        <textarea
          className="crash-detail"
          data-testid="app-crash-detail"
          readOnly
          rows={12}
          value={report}
          aria-label={`错误信息（${errorText(this.state.error)}）`}
        />
      </main>
    );
  }
}
