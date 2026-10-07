/**
 * PWA service worker 的注册判据（agora-thc.4；MISSION §6.9 手机入口）。
 *
 * 只在**安全上下文且非 loopback** 时注册：`http://127.0.0.1` 在浏览器里也是 secure context，
 * 不排除它的话开发机一开页面就装上 SW、把旧 bundle 钉在浏览器里（A39 的另一半）。生产上
 * 承载节点走 HTTPS（thc.3），iPhone 主屏 PWA 才会注册。
 */
export interface PwaLocation {
  secure: boolean;
  hostname: string;
}

const LOOPBACK = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

export function shouldRegisterPwa(loc: PwaLocation): boolean {
  if (!loc.secure) return false;
  return !LOOPBACK.has(loc.hostname.toLowerCase());
}

export interface PwaEnv extends PwaLocation {
  /** 浏览器支持 serviceWorker 且脚本已注册（`register` 的出口）。 */
  supported: boolean;
  register: (url: string) => Promise<unknown>;
}

function browserEnv(): PwaEnv {
  const supported = typeof navigator !== "undefined" && "serviceWorker" in navigator;
  return {
    supported,
    secure: typeof window !== "undefined" && window.isSecureContext === true,
    hostname: typeof window !== "undefined" ? window.location.hostname : "",
    register: (url) => navigator.serviceWorker.register(url),
  };
}

/** 该注册就注册；返回是否真的发起了注册（测试断言用）。失败静默：壳的功能不依赖 SW。 */
export function registerPwa(env: PwaEnv = browserEnv()): boolean {
  if (!env.supported || !shouldRegisterPwa(env)) return false;
  void env.register("/sw.js").catch(() => {});
  return true;
}
