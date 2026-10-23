import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import { installErrorCapture } from "./errors";
import "./index.css";
import { registerPwa } from "./pwa";

// 先装采集再挂载（agora-bkbg）：装在 effect 里会漏掉首帧 render 阶段的异常。
installErrorCapture(window);

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </StrictMode>,
  );
}

// PWA 壳（agora-thc.4）：只在安全上下文且非 loopback 时注册，见 web/src/pwa.ts。
registerPwa();
