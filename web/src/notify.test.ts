import { describe, expect, it } from "vitest";
import { Notifier, type NotificationLike, type NotifierDeps, type Permission } from "./notify";

function deps(initial: Permission) {
  let permission = initial;
  const created: { title: string; body: string; tag: string; closed: number; note: NotificationLike }[] = [];
  let requests = 0;
  let focused = 0;
  const d: NotifierDeps = {
    permission: () => permission,
    request: async () => {
      requests += 1;
      permission = "granted";
      return permission;
    },
    create: (title, opts) => {
      const entry = { title, body: opts.body, tag: opts.tag, closed: 0, note: null as unknown as NotificationLike };
      entry.note = { onclick: null, close: () => void (entry.closed += 1) };
      created.push(entry);
      return entry.note;
    },
    focus: () => {
      focused += 1;
    },
  };
  return { d, created, get requests() { return requests; }, get focused() { return focused; }, set: (p: Permission) => { permission = p; } };
}

describe("Notifier", () => {
  it("没权限时静默；granted 才弹，同会话新的替旧的（先 close 再弹、tag 唯一），点击回到该会话并带上状态", () => {
    const x = deps("default");
    const opened: [string, string | null][] = [];
    const n = new Notifier(x.d, (id, st) => opened.push([id, st]));
    expect(n.show({ id: "n:1", title: "t", body: "b", status: "waiting" })).toBe(false);
    expect(x.created).toHaveLength(0);

    x.set("granted");
    expect(n.show({ id: "n:1", title: "Claude / x @ n needs input", body: "Bash: rm", status: "waiting" })).toBe(true);
    expect(x.created).toHaveLength(1);
    expect(x.created[0]).toMatchObject({ title: "Claude / x @ n needs input", body: "Bash: rm", tag: "n:1#1" });
    // 同一会话再来一条：旧的先 close（macOS 上同 tag 替换不弹横幅，见 notify.ts），tag 换新。
    expect(n.show({ id: "n:1", title: "Claude / x @ n finished its turn", body: "ok", status: "turn_done" })).toBe(true);
    expect(x.created[0]!.closed).toBe(1);
    expect(x.created[1]!.tag).toBe("n:1#2");
    x.created[1]!.note.onclick?.({});
    expect(opened).toEqual([["n:1", "turn_done"]]);
    expect(x.focused).toBe(1);

    // denied：不弹也不抛。
    x.set("denied");
    expect(n.show({ id: "n:1", title: "t", body: "", status: "failed" })).toBe(false);
    expect(n.shown).toBe(2);
  });

  it("权限只问一次：default 才请求，已答过直接返回", async () => {
    const x = deps("default");
    const n = new Notifier(x.d, () => {});
    expect(await n.request()).toBe("granted");
    expect(await n.request()).toBe("granted");
    expect(x.requests).toBe(1);
    const y = deps("denied");
    const m = new Notifier(y.d, () => {});
    expect(await m.request()).toBe("denied");
    expect(y.requests).toBe(0);
  });
});
