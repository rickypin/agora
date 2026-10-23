/**
 * 崩溃现场（agora-bkbg）。
 *
 * 手机上没有 devtools：一次 render / effect 异常会把整棵 React 树卸载，屏幕上什么都不剩，
 * 也没有任何东西能粘给维护者。这里做两件事：
 *  1. `installErrorCapture` 把 `window` 的 `error` / `unhandledrejection` 记到一个**有界**的
 *     环形缓冲（5 条），随布局报告（`mobileDebug`）一起复制出去；
 *  2. `ErrorBoundary` 捕获 render / 生命周期异常，画一屏可复制的错误页（而不是空白）。
 *
 * 只留在内存：崩溃现场不值得为它开一个上报端点（与 `mobileDebug` 同一条纪律），也不含凭据。
 */

export interface CapturedError {
  /** 记录时刻（ISO，手机与节点可能没校时，只用于排序）。 */
  at: string;
  kind: "error" | "rejection";
  message: string;
  stack?: string;
}

/** 最多留几条：够看出"同一个错误连发"，又不至于把报告撑爆。 */
const MAX_CAPTURED = 5;

const captured: CapturedError[] = [];

/** 任意抛出物的可读文本（不吃掉字符串 / 对象；循环引用也不抛）。 */
export function errorText(value: unknown): string {
  if (value instanceof Error) return value.message || value.name;
  if (typeof value === "string") return value;
  if (value === null || value === undefined) return String(value);
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function errorStack(value: unknown): string | undefined {
  return value instanceof Error && typeof value.stack === "string" ? value.stack : undefined;
}

/** 记一条；超出上限时丢最旧的。返回记下的那一条（测试断言用）。 */
export function recordError(kind: CapturedError["kind"], value: unknown): CapturedError {
  const entry: CapturedError = {
    at: new Date().toISOString(),
    kind,
    message: errorText(value),
    stack: errorStack(value),
  };
  captured.push(entry);
  if (captured.length > MAX_CAPTURED) captured.splice(0, captured.length - MAX_CAPTURED);
  return entry;
}

/** 当前缓冲的副本（诊断报告 / 错误页用；调用方改不到内部数组）。 */
export function capturedErrors(): CapturedError[] {
  return captured.map((entry) => ({ ...entry }));
}

/** 测试用：清空缓冲。 */
export function clearCapturedErrors(): void {
  captured.length = 0;
}

/** `window` 的那一小半；测试可以注入假目标（node 环境没有 DOM）。 */
export interface ErrorTarget {
  addEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
  addEventListener(type: "unhandledrejection", listener: (event: PromiseRejectionEvent) => void): void;
  removeEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
  removeEventListener(type: "unhandledrejection", listener: (event: PromiseRejectionEvent) => void): void;
}

/**
 * 装全局采集；返回拆除函数。**在 createRoot 之前**装（main.tsx），这样首帧就受保护——
 * 装在 effect 里会漏掉 render 阶段的错误。
 */
export function installErrorCapture(target: ErrorTarget): () => void {
  const onError = (event: ErrorEvent) => {
    recordError("error", event.error ?? event.message);
  };
  const onRejection = (event: PromiseRejectionEvent) => {
    recordError("rejection", event.reason);
  };
  target.addEventListener("error", onError);
  target.addEventListener("unhandledrejection", onRejection);
  return () => {
    target.removeEventListener("error", onError);
    target.removeEventListener("unhandledrejection", onRejection);
  };
}

/** 错误页正文：构建号 + UA + 错误栈 + 最近捕获（一行一条，便于复制）。 */
export function crashReport(error: unknown, build: string, userAgent: string): string {
  const stack = errorStack(error) ?? errorText(error);
  const recent = capturedErrors()
    .map((entry) => `- ${entry.at} [${entry.kind}] ${entry.message}`)
    .join("\n");
  return [
    `前端 ${build}`,
    userAgent,
    "",
    stack,
    recent === "" ? "" : `\n最近捕获的错误：\n${recent}`,
  ].join("\n");
}
