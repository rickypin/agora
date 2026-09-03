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
