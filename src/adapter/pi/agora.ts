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
 *
 * 另一半是**输入队列**（agora-t5kf.2 / ADR-002 D11）：扩展每 1 s 看一遍
 * `$AGORA_HOME/input/<session hex>/`，把 daemon 写下的 `<id>.json` 认领（`.claimed`）、
 * `pi.sendUserMessage` 注入、改名 `.done` 回报；这才是"手机给终端里的 pi 发下一条指令"。
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";

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
      // 输入通道版本（ADR-002 D11）：这个扩展会收 $AGORA_HOME/input 队列并 `sendUserMessage`；
      // 没有这个字段（旧扩展 / 别的宿主）的行 text_via = none，只能到终端。
      input_channel: 1,
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

  // ── 宿主文本通道：收 daemon 的输入队列（agora-t5kf.2 / ADR-002 D11）────────────────────
  // daemon 把手机 / 桌面发的下一条指令写进 `$AGORA_HOME/input/<session hex>/<id>.json`；这里取件、
  // `pi.sendUserMessage` 注入、改名回报。先改名再注入（`.json` → `.claimed`）：daemon 的超时清理
  // 只删还没被取走的 `.json`，在途的那件不会被误删；成功 `.done`（ack），抛错 `.failed`（内容
  // 是一句给人看的话）。轮询 1 s：fs.watch 在部分文件系统上不可靠，延迟 1 s 还能接受。
  const INPUT_DIR = `${AGORA_HOME}/input`;
  const INPUT_POLL_MS = 1000;
  let mySession: string | undefined;

  const hexOf = (s: string): string => Buffer.from(s, "utf8").toString("hex");
  const sibling = (dir: string, name: string, suffix: string): string =>
    path.join(dir, `${name.replace(/\.json$/, "")}.${suffix}`);

  /** 取一件：认领 → 注入 → 回报。每步失败都静默（投递是旁路，永不打断 pi）。 */
  const takeOne = (dir: string): void => {
    let names: string[];
    try {
      names = fs.readdirSync(dir);
    } catch {
      return;
    }
    const name = names.filter((n) => n.endsWith(".json")).sort()[0];
    if (!name) return;
    const from = path.join(dir, name);
    const claimed = sibling(dir, name, "claimed");
    let text: string;
    try {
      text = fs.readFileSync(from, "utf8");
      fs.renameSync(from, claimed);
    } catch {
      return; // daemon 超时已删、或另一个取件者拿走了
    }
    try {
      // followUp：在跑就排队到这一轮的工具调用之后；空闲时照常起一轮（两种都实测过）。
      pi.sendUserMessage(text, { deliverAs: "followUp" });
      fs.renameSync(claimed, sibling(dir, name, "done"));
    } catch (err) {
      try {
        fs.writeFileSync(claimed, `pi.sendUserMessage 失败：${String(err)}\n`);
      } catch {
        /* 旁路 */
      }
      try {
        fs.renameSync(claimed, sibling(dir, name, "failed"));
      } catch {
        /* 旁路 */
      }
    }
  };

  /** 每轮最多取 8 件，免得一次塞很多时把 pi 拖住。 */
  const scan = (): void => {
    if (!mySession) return;
    const dir = path.join(INPUT_DIR, hexOf(mySession));
    for (let i = 0; i < 8; i++) {
      let pending: string[];
      try {
        pending = fs.readdirSync(dir).filter((n) => n.endsWith(".json"));
      } catch {
        return; // 目录不在：还没人发过东西
      }
      if (pending.length === 0) return;
      takeOne(dir);
    }
  };

  const inputTimer = setInterval(scan, INPUT_POLL_MS);
  (inputTimer as { unref?: () => void }).unref?.();

  pi.on("session_start", (e, ctx) => {
    try {
      const id = (session(ctx) as { session_id?: unknown }).session_id;
      if (typeof id === "string" && id) mySession = id;
    } catch {
      /* 旁路 */
    }
    scan();
    emit("session_start", { reason: e.reason, idle: idleOf(ctx) }, ctx);
  });
  pi.on("before_agent_start", (e, ctx) => emit("before_agent_start", { prompt: e.prompt ?? "" }, ctx));
  pi.on("tool_execution_start", (e, ctx) => emit("tool_execution_start", { tool_name: e.toolName ?? "tool" }, ctx));
  // agent_settled = pi 不会再自动继续（重试 / 压缩 / 排队都做完了）——这就是 TURN_DONE 的边界；
  // turn_end 每个模型轮都发，一轮里会有多次，不能拿来当"这一轮做完了"。
  pi.on("agent_settled", (_e, ctx) =>
    emit("agent_settled", { last_assistant_message: lastAssistant(ctx).slice(0, 8000) }, ctx),
  );
  pi.on("session_shutdown", (e, ctx) => emit("session_shutdown", { reason: e.reason }, ctx));
}
