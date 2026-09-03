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
    expect(r).toEqual({ ok: false, needsConfirmation: false, error: { error: "no_runtime", message: "external" } });
    const d = await api.deleteMetadata("n:x");
    expect(d.ok).toBe(true);
    expect(calls[1].init.method).toBe("DELETE");
    expect(calls[1].url).toBe("/api/sessions/n%3Ax");
  });
});
