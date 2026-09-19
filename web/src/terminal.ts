/**
 * `WS /api/sessions/:id/terminal` 的协议客户端（docs/spec/api.md；MISSION §3.2）。
 *
 * 只负责帧的编解码与生命周期，不碰 DOM：xterm.js 在 TerminalView 里接上。
 * 语义（MISSION §4.6）：`close()` 只是 detach——关掉这一条 attach 流，agent 继续跑；
 * 这里**没有**自动重连：重连由用户或页面刷新触发，绝不 restart / recreate。
 */

export interface TerminalSocketLike {
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  send(data: string): void;
  close(): void;
}

export type ExitInfo = { kind: "code"; value: number } | { kind: "signal"; value: string };

export type ServerFrame =
  | { type: "output"; data: string }
  | { type: "status"; status: string }
  | { type: "exit"; exit: ExitInfo }
  | { type: "pong" };

/**
 * 这一行的运行时会话没了（agora-u5p）：`status=finished` 且 `reason` 说 `runtime session gone`。
 * 判的是 agora 自己写进 reason 的那半句，两种「没了」都算（server gone / session gone），
 * `killed by user (runtime session gone…)` 也算——在 Dashboard 按过 Kill、后来连 pane 一起消失的
 * 行同样没有可连的东西（Mac 2026-09-18 现场 b226b5：库里 killed_at 在、状态说看不清）。
 *
 * 为什么放在连接层而不是 `TerminalView.tsx`：这一条说的是「这一行还能不能 attach」。挂在组件文件
 * 里的话，`vi.mock("./TerminalView")` 的局部替身（Workspace.test.tsx、keyboard.test.tsx）会把它
 * 换成 undefined，Workspace 一渲染就抛——守卫抓到的是测试脚手架，不是行为。
 */
export function runtimeSessionGone(
  row: { status?: unknown; reason?: unknown } | null | undefined,
): boolean {
  return (
    row?.status === "finished" &&
    typeof row.reason === "string" &&
    row.reason.includes("runtime session gone")
  );
}

export interface TerminalClientOptions {
  /** 建连；默认同源 `/api/sessions/<id>/terminal?cols=&rows=`。 */
  connect?: (id: string, cols: number, rows: number) => TerminalSocketLike;
  onOutput: (data: string) => void;
  onStatus?: (status: string) => void;
  onExit?: (exit: ExitInfo) => void;
  /** 连接结束（任何原因）；`exited` 表示之前收到过 exit。 */
  onClose?: (exited: boolean) => void;
}

export function defaultTerminalSocket(id: string, cols: number, rows: number): TerminalSocketLike {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const q = `cols=${cols}&rows=${rows}`;
  // 一行里同时出现 `new WebSocket(` 与 `/api/`：tests/arch_boundary.rs 逐行守卫前端只连 /api。
  return new WebSocket(`${proto}//${window.location.host}/api/sessions/${encodeURIComponent(id)}/terminal?${q}`) as unknown as TerminalSocketLike;
}

/**
 * `WS /api/sessions/:id/diff`（MISSION §6.3 看结果；A41，agora-h1k.5）：在会话工作目录跑 git diff 的只读终端，
 * 服务端首帧 `status: read_only`、丢弃一切 input。同一行同时出现 `new WebSocket(` 与 `/api/`（守卫同上）。
 */
export function defaultDiffSocket(id: string, cols: number, rows: number): TerminalSocketLike {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const q = `cols=${cols}&rows=${rows}`;
  return new WebSocket(`${proto}//${window.location.host}/api/sessions/${encodeURIComponent(id)}/diff?${q}`) as unknown as TerminalSocketLike;
}

export class TerminalClient {
  private socket: TerminalSocketLike | null = null;
  private open = false;
  private exited = false;
  /** 连上之前的尺寸变化只记最后一次，open 后补发一条 resize。 */
  private pendingSize: { cols: number; rows: number } | null = null;

  constructor(private readonly opts: TerminalClientOptions) {}

  connect(id: string, cols: number, rows: number): void {
    if (this.socket) return;
    const s = (this.opts.connect ?? defaultTerminalSocket)(id, cols, rows);
    this.socket = s;
    s.onopen = () => {
      this.open = true;
      if (this.pendingSize) {
        this.sendResize(this.pendingSize.cols, this.pendingSize.rows);
        this.pendingSize = null;
      }
    };
    s.onmessage = (ev) => {
      let frame: ServerFrame;
      try {
        frame = JSON.parse(ev.data) as ServerFrame;
      } catch {
        return; // 畸形帧忽略
      }
      switch (frame.type) {
        case "output":
          this.opts.onOutput(frame.data);
          break;
        case "status":
          this.opts.onStatus?.(frame.status);
          break;
        case "exit":
          this.exited = true;
          this.opts.onExit?.(frame.exit);
          break;
        case "pong":
          break;
      }
    };
    s.onclose = () => this.finish();
    s.onerror = () => {
      /* onclose 紧随其后 */
    };
  }

  get connected(): boolean {
    return this.open;
  }

  sendInput(data: string): void {
    if (!this.open || !this.socket) return;
    this.socket.send(JSON.stringify({ type: "input", data }));
  }

  sendResize(cols: number, rows: number): void {
    if (cols <= 0 || rows <= 0) return;
    if (!this.open || !this.socket) {
      this.pendingSize = { cols, rows };
      return;
    }
    this.socket.send(JSON.stringify({ type: "resize", cols, rows }));
  }

  ping(): void {
    if (this.open && this.socket) this.socket.send(JSON.stringify({ type: "ping" }));
  }

  /** Detach：只关这一条流。 */
  close(): void {
    const s = this.socket;
    if (!s) return;
    s.close();
    this.finish();
  }

  private finish(): void {
    if (!this.socket) return;
    const exited = this.exited;
    this.socket.onopen = this.socket.onmessage = this.socket.onclose = this.socket.onerror = null;
    this.socket = null;
    this.open = false;
    this.opts.onClose?.(exited);
  }
}
