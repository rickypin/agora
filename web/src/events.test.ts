import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  EventsClient,
  isProcessState,
  rowProcess,
  type SessionRow,
  type SocketLike,
} from "./events";

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
  /** 服务端带码关闭（真 WebSocket 的 CloseEvent 有 `code`）。 */
  serverClose(code: number): void {
    this.onclose?.({ code });
  }
}

function row(id: string, status = "running"): SessionRow {
  return { id, node: "n", status, alive: true, process: "alive" };
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

    // session_updated（改名）：整行替换；内容相等不触发 onChange。
    sockets[0].serverSend([{ type: "session_updated", id: "n:b", session: { ...row("n:b", "starting"), name: "renamed" } }]);
    await vi.advanceTimersByTimeAsync(300);
    expect(onChange.mock.calls.length).toBe(calls + 2);
    expect(client.sessions.get("n:b")?.name).toBe("renamed");
    sockets[0].serverSend([{ type: "session_updated", id: "n:b", session: { ...row("n:b", "starting"), name: "renamed" } }]);
    await vi.advanceTimersByTimeAsync(300);
    expect(onChange.mock.calls.length).toBe(calls + 2);
  });

  it("patches the process tri state in place and never leaves it disagreeing with alive (agora-5gg.18)", async () => {
    sockets[0].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(client.sessions.get("n:a")?.process).toBe("alive");

    // 升级了的节点：status_changed 带 process，就地 patch。这一行就是盘点 B2：turn_done
    // 而 agora 说不上进程在不在——旧布尔只能报 alive:false，看上去像「做完了且进程没了」。
    sockets[0].serverSend([
      { type: "status_changed", id: "n:a", status: "turn_done", source: "hook", reason: null, alive: false, process: "unknown" },
    ]);
    await vi.advanceTimersByTimeAsync(300);
    let r = client.sessions.get("n:a")!;
    expect([r.status, r.process, r.alive]).toEqual(["turn_done", "unknown", false]);
    expect(rowProcess(r)).toBe("unknown");

    // 不认识的取值（对端二进制比页面新）：不写进视图，保留上一眼。
    sockets[0].serverSend([
      { type: "status_changed", id: "n:a", status: "running", source: "hook", reason: null, alive: false, process: "sleeping" },
    ]);
    await vi.advanceTimersByTimeAsync(300);
    expect(client.sessions.get("n:a")?.process).toBe("unknown");

    // 没升级的节点（minor 更旧）不带 process：行上也不留旧值，否则会出现 alive 与 process
    // 自己打自己的行（patch 过 unknown 之后又来一条只带 alive 的事件）。
    sockets[0].serverSend([
      { type: "status_changed", id: "n:a", status: "finished", source: "process", reason: null, alive: false },
    ]);
    await vi.advanceTimersByTimeAsync(300);
    r = client.sessions.get("n:a")!;
    expect("process" in r).toBe(false);
    expect(rowProcess(r)).toBe("gone");
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

  it("hands peer_changed to onPeerChanged at once, outside the 300 ms batch, and never touches the rows (agora-c8h)", async () => {
    const onPeerChanged = vi.fn<(name: string, peer: unknown) => void>();
    client.stop();
    client = new EventsClient({
      connect: () => {
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
      fetchSnapshot: async () => ({ sessions: snapshot, unregistered: [] }),
      coalesceMs: 300,
      onChange,
      onPeerChanged,
    });
    client.start();
    sockets[sockets.length - 1].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    const changes = onChange.mock.calls.length;
    const peer = { online: false, last_seen: "2026-09-02T23:10:00Z", retrying: true, last_error: "unreachable" };
    sockets[sockets.length - 1].serverSend([{ type: "peer_changed", name: "zuan", peer }]);
    // 立刻到，不等合并批；行列表没有变化、onChange 不多叫。
    expect(onPeerChanged).toHaveBeenCalledWith("zuan", peer);
    await vi.advanceTimersByTimeAsync(300);
    expect(onChange.mock.calls.length).toBe(changes);
    expect(client.snapshots).toBe(1); // 不因它重拉全量
  });

  it("stops reconnecting and reports revoked when the server closes with 4401 (agora-0jt)", async () => {
    const onRevoked = vi.fn();
    client.stop();
    client = new EventsClient({
      connect: () => {
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
      fetchSnapshot: async () => ({ sessions: snapshot, unregistered: [] }),
      reconnectMinMs: 100,
      onChange,
      onRevoked,
    });
    client.start();
    const first = sockets.length - 1;
    sockets[first].serverOpen();
    await vi.advanceTimersByTimeAsync(0);

    // 普通断流：退避后重连。
    sockets[first].serverClose(1006);
    await vi.advanceTimersByTimeAsync(100);
    expect(sockets.length).toBe(first + 2);
    expect(onRevoked).not.toHaveBeenCalled();

    // 4401：本设备被吊销——回调一次、之后再久也不重连（重连只会吃 401）。
    sockets[first + 1].serverOpen();
    await vi.advanceTimersByTimeAsync(0);
    sockets[first + 1].serverClose(4401);
    expect(onRevoked).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets.length).toBe(first + 2);
  });
});

describe("进程三态的类型守卫（Q4，agora-5gg.18）", () => {
  it("只认 api.md 锁定的那三个词", () => {
    // 词表是 API 形态：`docs/spec/api.md`「会话形态」+ api_version 1.7。守卫钉住浏览器侧认的字面量，
    // 服务端改名（或改名一半）会在这里红，而不是静默把 `gone` 当成「没说」。
    for (const w of ["alive", "gone", "unknown"]) expect(isProcessState(w)).toBe(true);
    for (const v of [null, undefined, "", "dead", "ALIVE", 0, 1, true, {}]) {
      expect(isProcessState(v)).toBe(false);
    }
  });

  it("rowProcess 优先读三值，缺键的旧 peer 行退回 alive 投影", () => {
    const base = { id: "n:a", node: "n", status: "turn_done", alive: false };
    expect(rowProcess({ ...base, process: "unknown" })).toBe("unknown");
    expect(rowProcess({ ...base, process: "gone" })).toBe("gone");
    // 没升级的节点：minor 更旧，不发 process。`alive: false` 在旧形态里可能是"没了"也可能是
    // "不知道"，这里读成 gone——少说一次未知，不会把未知说成活着。
    expect(rowProcess({ ...base })).toBe("gone");
    expect(rowProcess({ ...base, alive: true })).toBe("alive");
    // 行上有 process 就以它为准：alive 只是投影，两者不一致时（对端 bug）信三值。
    expect(rowProcess({ ...base, alive: true, process: "gone" })).toBe("gone");
  });
});
