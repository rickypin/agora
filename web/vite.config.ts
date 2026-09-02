/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 开发时 /api 代理到 daemon；产物由 rust-embed 内嵌（src/api/spa.rs）。
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:7680", ws: true },
    },
  },
  build: { outDir: "dist", emptyOutDir: true },
  test: { environment: "node" },
});
