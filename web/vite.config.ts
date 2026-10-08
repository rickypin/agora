/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

// 开发时 /api 代理到 daemon；产物由 rust-embed 内嵌（src/api/spa.rs）。
// 默认指向开发机上真的 daemon（127.0.0.1:7680）。并行施工时每个 agent 起自己的 daemon（独立 AGORA_HOME
// 与端口），dev server 必须指到它而不是真的那个：`env AGORA_DEV_API=http://127.0.0.1:7743 npm --prefix web run dev`
// （2026-09-06，agora-oir）。走 loadEnv 而不直接读 process.env：tsconfig 没带 @types/node，`process` 在
// typecheck 里是未声明的全局；loadEnv 会把带前缀的 process.env 一并收进来，且 .env 文件里写 AGORA_DEV_API 也认。
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "AGORA_");
  return {
    server: {
      proxy: {
        "/api": { target: env.AGORA_DEV_API ?? "http://127.0.0.1:7680", ws: true },
      },
    },
    // 前端构建号（agora-xu12）：iOS 主屏 PWA 会把 start_url 钉在缓存里，升级后手机上跑的到底是哪一版
    // 光看行为猜不出来（2026-10-08 用户两次报同一个问题、报告格式却旧了两版）。`npm run build` 用
    // UTC 时刻生成 AGORA_WEB_BUILD，loadEnv（上面的 "AGORA_" 前缀）把它带进来，烤成字面量显示在设置页
    // 底部、随诊断报告复制出去，并写成 `build.txt` 给服务端读——两边一比就知道手机是不是最新版。
    // **刻意不用 git sha**：web bundle 是在 commit 之前构建的，`git rev-parse HEAD` 拿到的是**父提交**
    // 的 sha，显示出来会让人以为手机上跑的是上一版（2026-10-08 实测，白白排查了一轮）。时间戳不会撒谎。
    // 直接 `vite build`（不带 npm script）时退成 dev，不因此让构建失败。
    define: { __WEB_BUILD__: JSON.stringify(env.AGORA_WEB_BUILD ?? "dev") },
    plugins: [
      react(),
      {
        // `build.txt`：非 hash 名字的构建号文件，服务端 `/api/system` 的 `web_build` 读它；
        // 手机端拿自己的 __WEB_BUILD__ 与它比，不同就是"有新版、该硬刷新"。
        name: "agora-web-build",
        generateBundle() {
          this.emitFile({ type: "asset", fileName: "build.txt", source: env.AGORA_WEB_BUILD ?? "dev" });
        },
      },
    ],
    build: { outDir: "dist", emptyOutDir: true },
    // css.include 是给 SessionRow.test.tsx 那条 CSS 守卫开的（agora-8lb）：vitest 默认 css: false 会把
    // CSS 模块 stub 成空，连 `import css from "./index.css?raw"` 也一起吞掉——返回空字符串，于是
    // `expect(css).not.toMatch(根因那条规则)` 永远绿，守卫等于没写（2026-09-09 实测 rawCss.length === 0）。
    // 只放行 index.css，别的 CSS 仍走默认 stub。
    test: { environment: "node", css: { include: [/index\.css/] } },
  };
});
