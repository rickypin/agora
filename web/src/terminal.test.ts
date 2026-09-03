import { describe, expect, it, vi } from "vitest";
import { TerminalClient, type TerminalSocketLike } from "./terminal";

class FakeSocket implements TerminalSocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  sent: string[] = [];
  closed = false;
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  serverOpen(): void {
    this.onopen?.({});
  }
  serverSend(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
  serverDrop(): void {
    this.onclose?.({});
  }
}

function setup() {
  let sock: FakeSocket | null = null;
  let url = "";
  const onOutput = vi.fn<(d: string) => void>();
  const onExit = vi.fn();
  const onClose = vi.fn<(exited: boolean) => void>();
  const onStatus = vi.fn();
  const client = new TerminalClient({
    connect: (id, cols, rows) => {
      url = `${id}?${cols}x${rows}`;
      sock = new FakeSocket();
      return sock;
    },
    onOutput,
    onExit,
    onClose,
    onStatus,
  });
  client.connect("n:s1", 80, 24);
  return { client, sock: sock as unknown as FakeSocket, url: () => url, onOutput, onExit, onClose, onStatus };
}

describe("TerminalClient", () => {
  it("connects with the initial size and forwards output/status/exit frames", () => {
    const t = setup();
    expect(t.url()).toBe("n:s1?80x24");
    t.sock.serverOpen();
    t.sock.serverSend({ type: "status", status: "attached" });
    t.sock.serverSend({ type: "output", data: "hi" });
    t.sock.serverSend({ type: "pong" });
    t.sock.serverSend({ type: "exit", exit: { kind: "code", value: 0 } });
    expect(t.onStatus).toHaveBeenCalledWith("attached");
    expect(t.onOutput).toHaveBeenCalledWith("hi");
    expect(t.onExit).toHaveBeenCalledWith({ kind: "code", value: 0 });
    t.sock.serverDrop();
    expect(t.onClose).toHaveBeenCalledWith(true);
  });

  it("encodes input and resize per the spec, and buffers resize until open", () => {
    const t = setup();
    t.client.sendResize(100, 30);
    t.client.sendInput("x"); // 未连上：丢弃，输入没有缓冲的意义
    expect(t.sock.sent).toEqual([]);
    t.sock.serverOpen();
    expect(t.sock.sent).toEqual([JSON.stringify({ type: "resize", cols: 100, rows: 30 })]);
    t.client.sendInput("ls\r");
    t.client.sendResize(0, 5); // 非法尺寸不发
    t.client.ping();
    expect(t.sock.sent.slice(1)).toEqual([
      JSON.stringify({ type: "input", data: "ls\r" }),
      JSON.stringify({ type: "ping" }),
    ]);
  });

  it("close() is a detach: closes the socket once, no reconnect, reports not-exited", () => {
    const t = setup();
    t.sock.serverOpen();
    t.client.close();
    expect(t.sock.closed).toBe(true);
    expect(t.onClose).toHaveBeenCalledTimes(1);
    expect(t.onClose).toHaveBeenCalledWith(false);
    t.sock.serverDrop(); // 迟到的 close 事件不再回调
    expect(t.onClose).toHaveBeenCalledTimes(1);
    expect(t.client.connected).toBe(false);
  });

  it("ignores malformed frames", () => {
    const t = setup();
    t.sock.serverOpen();
    t.sock.onmessage?.({ data: "{not json" });
    expect(t.onOutput).not.toHaveBeenCalled();
  });
});
