import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";
import { registerPwa } from "./pwa";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

// PWA 壳（agora-thc.4）：只在安全上下文且非 loopback 时注册，见 web/src/pwa.ts。
registerPwa();
