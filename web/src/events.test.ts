import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EventsClient, type SessionRow, type SocketLike } from "./events";

class FakeSocket implements SocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  closed = false;
  close(): void {
    this.closed = true;
  }
  serverOpen(): void {
    this.onopen?.({});
  }
  serverSend(events: unknown[]): void {
    this.onmessage?.({ data: JSON.stringify(events) });
  }
  serverDrop(): void {
    this.onclose?.({});
  }
}

function row(id: string, status = "running"): SessionRow {
  return { id, node: "n", status, alive: true };
}

describe("EventsClient", () => {
  const sockets: FakeSocket[] = [];
  let snapshot: SessionRow[];
  let onChange: ReturnType<typeof vi.fn<(s: Map<string, SessionRow>) => void>>;
  let client: EventsClient;

  beforeEach(() => {
    vi.useFakeTimers();
    sockets.length = 0;
    snapshot = [row("n:a")];
    onChange = vi.fn<(s: Map<string, SessionRow>) => void>();
    client = new EventsClient({
      connect: () => {
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
      fetchSnapshot: async () => ({ sessions: snapshot, unregistered: [] }),
      coalesceMs: 300,
      reconnectMinMs: 100,
      onChange,
    });
    client.start();
  });

  afterEach(() => {
    client.stop();
    vi.useRealTimers();
  });

  it("pulls a full snapshot on open, then patches in place with coalescing", async () => {
    sockets[0].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(client.snapshots).toBe(1);
    expect([...client.sessions.keys()]).toEqual(["n:a"]);

    // 三条状态变化 + 一条创建：300 ms 内不渲染，之后只渲染一次。
    const calls = onChange.mock.calls.length;
    sockets[0].serverSend([
      { type: "status_changed", id: "n:a", status: "starting", source: "process", reason: null, alive: true },
      { type: "status_changed", id: "n:a", status: "waiting", source: "process", reason: null, alive: true },
    ]);
    sockets[0].serverSend([{ type: "session_created", id: "n:b", session: row("n:b", "starting") }]);
    expect(onChange.mock.calls.length).toBe(calls);
    await vi.advanceTimersByTimeAsync(300);
    expect(onChange.mock.calls.length).toBe(calls + 1);
    expect(client.sessions.get("n:a")?.status).toBe("waiting");
    expect(client.sessions.get("n:b")?.status).toBe("starting");
    expect(client.snapshots).toBe(1);
  });

  it("re-pulls the snapshot after a reconnect and on resync, never polling", async () => {
    sockets[0].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(client.snapshots).toBe(1);

    // 断流期间服务端有了新会话；重连后靠全量对齐拿到它。
    snapshot = [row("n:a"), row("n:c")];
    sockets[0].serverDrop();
    await vi.advanceTimersByTimeAsync(100);
    expect(sockets.length).toBe(2);
    sockets[1].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(client.snapshots).toBe(2);
    expect([...client.sessions.keys()].sort()).toEqual(["n:a", "n:c"]);

    // 服务端说落后了：缓冲里的增量作废，重拉全量。
    snapshot = [row("n:c")];
    sockets[1].serverSend([{ type: "session_created", id: "n:zzz", session: row("n:zzz") }]);
    sockets[1].serverSend([{ type: "resync" }]);
    await vi.advanceTimersByTimeAsync(300);
    expect(client.snapshots).toBe(3);
    expect([...client.sessions.keys()]).toEqual(["n:c"]);

    // 安静 10 s：没有任何额外的全量拉取（不轮询）。
    await vi.advanceTimersByTimeAsync(10_000);
    expect(client.snapshots).toBe(3);
  });
});
