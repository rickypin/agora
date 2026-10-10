import { describe, expect, it } from "vitest";
import { sessionApi, type FetchLike } from "./api";

function fakeFetch(handler: (url: string, init: RequestInit) => { status: number; body?: unknown }) {
  const calls: { url: string; init: RequestInit }[] = [];
  const f: FetchLike = async (url, init) => {
    calls.push({ url, init });
    const r = handler(url, init);
    return new Response(r.body === undefined ? null : JSON.stringify(r.body), {
      status: r.status,
      headers: { "content-type": "application/json" },
    });
  };
  return { f, calls };
}

describe("sessionApi", () => {
  it("rename sends PATCH even when the name is unchanged", async () => {
    const { f, calls } = fakeFetch(() => ({ status: 200, body: { id: "n:a" } }));
    const r = await sessionApi(f).rename("n:a", "same");
    expect(r.ok).toBe(true);
    expect(calls[0].url).toBe("/api/sessions/n%3Aa");
    expect(calls[0].init.method).toBe("PATCH");
    expect(calls[0].init.body).toBe(JSON.stringify({ display_name: "same" }));
  });

  it("kill: 409 needs_confirmation is surfaced, then resent with confirmed", async () => {
    const { f, calls } = fakeFetch((_u, init) => {
      const body = JSON.parse(String(init.body)) as { confirmed?: boolean };
      return body.confirmed
        ? { status: 200, body: { alive: false } }
        : { status: 409, body: { error: "needs_confirmation", message: "会杀" } };
    });
    const api = sessionApi(f);
    const first = await api.kill("n:a");
    expect(first).toEqual({ ok: false, needsConfirmation: true });
    const second = await api.kill("n:a", true);
    expect(second.ok).toBe(true);
    expect(calls.map((c) => c.init.body)).toEqual(["{}", JSON.stringify({ confirmed: true })]);
    expect(calls.every((c) => c.url.endsWith("/kill"))).toBe(true);
  });

  it("other errors come back typed by `error`, and DELETE hits the metadata endpoint only", async () => {
    const { f, calls } = fakeFetch((u) =>
      u.endsWith("/restart")
        ? { status: 409, body: { error: "no_runtime", message: "external" } }
        : { status: 204 },
    );
    const api = sessionApi(f);
    const r = await api.restart("n:x");
    expect(r).toEqual({ ok: false, needsConfirmation: false, error: { error: "no_runtime", message: "external" }, status: 409 });
    const d = await api.deleteMetadata("n:x");
    expect(d.ok).toBe(true);
    expect(calls[1].init.method).toBe("DELETE");
    expect(calls[1].url).toBe("/api/sessions/n%3Ax");
  });

  it("uploadImage posts base64 to /images and hands back the node's path (agora-lmz2)", async () => {
    const { f, calls } = fakeFetch(() => ({ status: 201, body: { path: "/w/.agora-uploads/1-abcdef.png" } }));
    const r = await sessionApi(f).uploadImage("n:x", "iVBORw0KGgo=");
    expect(r).toEqual({ ok: true, value: { path: "/w/.agora-uploads/1-abcdef.png" } });
    expect(calls[0].url).toBe("/api/sessions/n%3Ax/images");
    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].init.body).toBe(JSON.stringify({ data: "iVBORw0KGgo=" }));
  });

  it("an older node without /images answers 405 with no JSON: the status is what tells it apart", async () => {
    const { f } = fakeFetch(() => ({ status: 405 }));
    const r = await sessionApi(f).uploadImage("n:x", "AA==");
    expect(r).toEqual({ ok: false, needsConfirmation: false, error: { error: "unknown", message: "HTTP 405" }, status: 405 });
  });

  it("turns GETs /turns with the limit; an older node's empty 404 is just not ok (agora-2mff)", async () => {
    const turn = { prompt: "p", injected: false, reply: "r", outcome: "done", failure: null, started_at: 1, ended_at: 2 };
    const { f, calls } = fakeFetch((u) => (u.startsWith("/api/sessions/n%3Ax/") ? { status: 200, body: { turns: [turn], keep: 20 } } : { status: 404 }));
    const r = await sessionApi(f).turns("n:x", 4);
    expect(r).toEqual({ ok: true, value: { turns: [turn], keep: 20 } });
    expect(calls[0].url).toBe("/api/sessions/n%3Ax/turns?limit=4");
    expect(calls[0].init.method).toBe("GET");
    const old = await sessionApi(f).turns("old:y", 4);
    expect(old.ok).toBe(false);
  });
});

import { restartNoteOf } from "./SessionSettings";

describe("restartNoteOf", () => {
  it("说清楚 resume 了谁、或为什么没 resume（ADR-002 D7）", () => {
    expect(restartNoteOf({ restart: { resumed: true, agent_session_id: "conv-1" } })).toContain("conv-1");
    expect(restartNoteOf({ restart: { resumed: false, reason: "版本不可解析" } })).toContain("版本不可解析");
    expect(restartNoteOf({})).toBe("已 Restart。");
  });
});
