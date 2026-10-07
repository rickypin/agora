/**
 * agora 的 pi 扩展（agora-c3i）：把 pi 的生命周期事件投给 `agora hook --host pi`。
 *
 * 由 `agora hooks install pi` 写到 `~/.pi/agent/extensions/agora.ts`；文件里的两条路径是安装
 * 那一刻写死的（与 Claude / Codex 的 hook 命令同一个理由：agent 环境里没有 AGORA_HOME），
 * 换过 AGORA_HOME 就重装一次。
 *
 * 原则与其它宿主的 hook 一样：**永远不拖累 pi**。投递是旁路——spawn 失败、agora 不在、daemon
 * 不在，全部静默；事件落了投递箱就有，daemon 下次扫描照样能收（agora hook 自己负责落盘）。
 * 投递**串行**（等上一件 `agora hook` 退出再发下一件）：投递件按落盘顺序应用，顺序就是语义，
 * 各件独立 spawn 会乱序（2026-10-07 实测：`pi -p` 的 before_agent_start 抢在 session_start 前
 * 落盘，行按 prompt 先应用）；一件最多等 5 s，超时杀掉继续。唯一例外是 `session_shutdown`：
 * pi 退出是同步的 `process.exit()`，排进 promise 队列的微任务跑不到（2026-10-07 实测：quit 那条
 * 永远不落盘），所以它同步 spawn——它前面的事件早发完了，不靠这条序。
 *
 * 只发本文件里列的这几个事件与字段，不把 pi 的原始事件整包转出去（`agent_end.messages` 里带着
 * 整段系统提示与转录）。字段名与 `src/adapter/pi.rs` 的 `parse` 一一对应。
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { spawn } from "node:child_process";

// 安装时替换成 JS 字符串字面量（含引号）：agora 二进制与 AGORA_HOME。
const AGORA = {{AGORA}};
const AGORA_HOME = {{HOME}};

export default function (pi: ExtensionAPI) {
  /** agent 侧的一段文本（assistant message 的 content 可能是字符串，也可能是 content parts）。 */
  const textOf = (content: unknown): string => {
    if (typeof content === "string") return content;
    if (Array.isArray(content)) {
      return content
        .map((p) => (p && typeof p === "object" && (p as { type?: string; text?: string }).type === "text" ? ((p as { text?: string }).text ?? "") : ""))
        .join("");
    }
    return "";
  };

  /** 正在结束的这一轮的最后一条 assistant 文本（手机卡片与桌面 `↳` 的输入）。 */
  const lastAssistant = (ctx: unknown): string => {
    try {
      const sm = (ctx as { sessionManager?: { getBranch?: () => unknown[] } })?.sessionManager;
      const branch = sm?.getBranch?.() ?? [];
      for (let i = branch.length - 1; i >= 0; i--) {
        const e = branch[i] as { type?: string; message?: { role?: string; content?: unknown } };
        if (e?.type === "message" && e.message?.role === "assistant") {
          const t = textOf(e.message.content).trim();
          if (t) return t;
        }
      }
    } catch {
      /* 读不到就算了：turn_done 仍然要报 */
    }
    return "";
  };

  const session = (ctx: unknown) => {
    const sm = (ctx as { sessionManager?: Record<string, () => unknown> })?.sessionManager;
    const get = (m: string): unknown => {
      try {
        return sm?.[m]?.() ?? undefined;
      } catch {
        return undefined;
      }
    };
    const c = ctx as { cwd?: string; mode?: string };
    return {
      session_id: get("getSessionId"),
      session_file: get("getSessionFile"),
      session_name: get("getSessionName"),
      cwd: c?.cwd ?? get("getCwd"),
      mode: c?.mode,
    };
  };

  /** 登记这一刻 agent 在不在跑（pi 的 `ExtensionContext.isIdle`；没有这个方法就不报）。 */
  const idleOf = (ctx: unknown): boolean | undefined => {
    try {
      const f = (ctx as { isIdle?: unknown })?.isIdle;
      return typeof f === "function" ? (f as () => unknown).call(ctx) === true : undefined;
    } catch {
      return undefined;
    }
  };

  let queue: Promise<void> = Promise.resolve();

  /** 投递一件：等到子进程退出（投递箱落盘完成）、最多 5 s，出错也 resolve。 */
  const send = (payload: Record<string, unknown>): Promise<void> =>
    new Promise((resolve) => {
      let settled = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const done = () => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve();
      };
      try {
        const child = spawn(AGORA, ["hook", "--host", "pi", "--home", AGORA_HOME], {
          stdio: ["pipe", "ignore", "ignore"],
          detached: true,
          env: {
            ...process.env,
            // pi 给 shell 工具的那一族变量（docs/environment-variables.md）；信封据此认出宿主，
            // PI_PID 是无运行时句柄的行探活的进程号。
            PI_SESSION_ID: String(payload.session_id ?? ""),
            PI_SESSION_FILE: String(payload.session_file ?? ""),
            PI_PID: String(process.pid),
          },
        });
        timer = setTimeout(() => {
          try {
            child.kill("SIGKILL");
          } catch {
            /* 已经退了 */
          }
          done();
        }, 5000);
        timer.unref?.();
        child.on("error", done);
        child.on("exit", done);
        child.stdin?.on("error", () => {});
        child.stdin?.end(JSON.stringify(payload));
        child.unref();
      } catch {
        done();
      }
    });

  const emit = (event: string, extra: Record<string, unknown>, ctx: unknown) => {
    try {
      const payload: Record<string, unknown> = { hook_event_name: event, ...session(ctx), ...extra };
      for (const k of Object.keys(payload)) if (payload[k] === undefined) delete payload[k];
      if (event === "session_shutdown") {
        // 同步 spawn，不过队列：pi 收到 session_shutdown 后可能立刻 `process.exit()`，队列里的
        // 微任务不保证跑得到（2026-10-07 实测：`pi -p` 的 quit 一条没落盘）。
        void send(payload);
        return;
      }
      queue = queue.then(() => send(payload)).catch(() => {});
    } catch {
      /* 投递是旁路，永不打断 pi */
    }
  };

  pi.on("session_start", (e, ctx) =>
    emit("session_start", { reason: e.reason, idle: idleOf(ctx) }, ctx),
  );
  pi.on("before_agent_start", (e, ctx) => emit("before_agent_start", { prompt: e.prompt ?? "" }, ctx));
  pi.on("tool_execution_start", (e, ctx) => emit("tool_execution_start", { tool_name: e.toolName ?? "tool" }, ctx));
  // agent_settled = pi 不会再自动继续（重试 / 压缩 / 排队都做完了）——这就是 TURN_DONE 的边界；
  // turn_end 每个模型轮都发，一轮里会有多次，不能拿来当"这一轮做完了"。
  pi.on("agent_settled", (_e, ctx) =>
    emit("agent_settled", { last_assistant_message: lastAssistant(ctx).slice(0, 8000) }, ctx),
  );
  pi.on("session_shutdown", (e, ctx) => emit("session_shutdown", { reason: e.reason }, ctx));
}
