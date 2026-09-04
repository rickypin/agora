import { describe, expect, it } from "vitest";
import { Notifier, type NotificationLike, type NotifierDeps, type Permission } from "./notify";

function deps(initial: Permission) {
  let permission = initial;
  const created: { title: string; body: string; tag: string; note: NotificationLike }[] = [];
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
      const note: NotificationLike = { onclick: null, close() {} };
      created.push({ title, body: opts.body, tag: opts.tag, note });
      return note;
    },
    focus: () => {
      focused += 1;
    },
  };
  return { d, created, get requests() { return requests; }, get focused() { return focused; }, set: (p: Permission) => { permission = p; } };
}

describe("Notifier", () => {
  it("没权限时静默；granted 才弹，tag 是会话 id，点击回到该会话并带上状态", () => {
    const x = deps("default");
    const opened: [string, string | null][] = [];
    const n = new Notifier(x.d, (id, st) => opened.push([id, st]));
    expect(n.show({ id: "n:1", title: "t", body: "b", status: "waiting" })).toBe(false);
    expect(x.created).toHaveLength(0);

    x.set("granted");
    expect(n.show({ id: "n:1", title: "Claude / x @ n needs input", body: "Bash: rm", status: "waiting" })).toBe(true);
    expect(x.created).toHaveLength(1);
    expect(x.created[0]).toMatchObject({ title: "Claude / x @ n needs input", body: "Bash: rm", tag: "n:1" });
    x.created[0]!.note.onclick?.({});
    expect(opened).toEqual([["n:1", "waiting"]]);
    expect(x.focused).toBe(1);

    // denied：不弹也不抛。
    x.set("denied");
    expect(n.show({ id: "n:1", title: "t", body: "", status: "failed" })).toBe(false);
    expect(n.shown).toBe(1);
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
