// @vitest-environment jsdom
/**
 * /m 手机收件箱（agora-thc.5；MISSION §6.9；A37 / A52）。
 *
 * 钉住的四件事：四段与 attention 同源（headless 收进已完成、不虚增「在跑」）、A52 的 DOM 预算
 * （没有终端 / 创建 / diff / 验收 / 改动列表 / 消息流与历史翻页）、推送深链定位（落在收起的
 * 已完成区也要先展开再选中）、行上的时长与摘要口径。
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { sessionApi, type FetchLike, type PresetInfo } from "./api";
import { SEEN_STORAGE_KEY, seenKey } from "./attention";
import type { SessionRow, SocketLike } from "./events";
import { HealthWatcher, VersionWatcher } from "./health";
import { MobileApp, mobileSummary } from "./MobileApp";
import { SessionStore } from "./store";

class FakeSocket implements SocketLike {
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  close(): void {}
}

/** A52 的桌面形态名单（agora-uqpi 的拍板只把"创建"改成"只有预设按钮"，这一批一个都不删）。 */
const FORBIDDEN_DESKTOP_SURFACES = [
  '[data-testid^="term"]',
  '[data-testid="create"]',
  '[data-testid^="diff-"]',
  '[data-testid^="acceptance-"]',
  '[data-testid^="changes-"]',
  '[data-testid^="respond-"]',
  '[data-testid^="new-agent"]',
  '[data-testid^="mobile-stream"]',
  '[data-testid="mobile-load-more"]',
  ".xterm",
  ".terminal",
  ".mobile-stream",
  ".mobile-history",
];

function row(id: string, patch: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    node: "zuan",
    status: "running",
    alive: true,
    display_name: id,
    agent_type: "claude",
    reason: null,
    respond_via: "hook",
    // 服务端的普通行：活着、由 agora 管的运行时（agora-prdg.2 起 composer 门看 text_via；
    // 采纳 / 死 pane 的行是 none，`managed = false` 也会否决）。
    runtime_ref: `tmux:agora:${id}`,
    text_via: "runtime",
    managed: true,
    ...patch,
  };
}

function setup(rows: SessionRow[], fetchImpl?: FetchLike, now = 1000) {
  const sock = new FakeSocket();
  const store = new SessionStore({
    connect: () => sock,
    fetchSnapshot: async () => ({ sessions: rows, unregistered: [] }),
    coalesceMs: 0,
  });
  const health = new HealthWatcher({ fetchHealth: async () => ({ status: "ok" }) });
  const version = new VersionWatcher({ fetchSystem: async () => ({ node: "zuan", api_version: { major: 1, minor: 9 } }) });
  const ui = render(
    <MobileApp store={store} health={health} version={version} now={now} api={fetchImpl ? sessionApi(fetchImpl) : undefined} />,
  );
  return { ui, store, sock };
}

/** WS 连上：EventsClient 在 onopen 里拉全量，VersionWatcher 也在那一刻比版本。 */
async function online(t: ReturnType<typeof setup>) {
  await act(async () => {
    t.sock.onopen?.({});
  });
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  window.history.replaceState(null, "", "/");
});

describe("手机端字号档位（agora-x70t.xsgz）", () => {
  it("data-text 说出现在这一档，且跟着 localStorage 走", async () => {
    // `data-text` 是字号档位与 CSS 的接口（--m-fs 一族都挂在它上面）。默认档是"标准"=17px。
    const t = setup([row("n:a")]);
    await online(t);
    expect(screen.getByTestId("mobile-inbox").getAttribute("data-text")).toBe("m");
    cleanup();

    localStorage.setItem("agora.mobile-text", "xl");
    const t2 = setup([row("n:a")]);
    await online(t2);
    expect(screen.getByTestId("mobile-inbox").getAttribute("data-text")).toBe("xl");
  });
});

describe("mobile inbox sections", () => {
  it("keeps attention's four-section order and never counts headless as working", async () => {
    const t = setup([
      row("n:a", { status: "waiting", status_since: 820 }),
      row("n:b", { status: "unknown" }),
      row("n:c", { status: "running" }),
      row("n:d", { status: "running", origin: "headless" }),
      row("n:e", { status: "finished", origin: "external" }),
    ]);
    await online(t);

    const order = [...t.ui.container.querySelectorAll('[data-testid^="mobile-section-"]')].map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(order).toEqual([
      "mobile-section-attention",
      "mobile-section-unclear",
      "mobile-section-working",
      "mobile-section-finished",
    ]);
    expect(screen.getByTestId("mobile-section-attention").textContent).toContain("n:a");
    expect(screen.getByTestId("mobile-section-unclear").textContent).toContain("n:b");
    expect(screen.getByTestId("mobile-section-working").textContent).toContain("n:c");
    // headless 在跑的行收进已完成（与桌面 finishedCollapsed 同源），不虚增「在跑」。
    expect(screen.getByTestId("mobile-section-working").textContent).not.toContain("n:d");
    expect(screen.getByTestId("mobile-section-finished").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("mobile-finished-toggle"));
    expect(screen.getByTestId("mobile-row-n:d")).toBeTruthy();
    expect(screen.getByTestId("mobile-row-n:e")).toBeTruthy();
  });

  it("shows duration, the peer ≥ bound, agent badge and node chips", async () => {
    const t = setup([
      row("n:a", { status: "waiting", status_since: 820 }),
      row("n:p", { node: "mac", status: "waiting", status_since: 820, stale: false }),
    ]);
    await online(t);

    const local = screen.getByTestId("mobile-row-n:a");
    const peer = screen.getByTestId("mobile-row-n:p");
    expect(local.textContent).toContain("等你 3m");
    expect(peer.textContent).toContain("等你 ≥3m");
    expect(local.textContent).toContain("Claude");
    expect(screen.getByTestId("mobile-node-n:a").className).toContain("local");
    const peerChip = screen.getByTestId("mobile-node-n:p");
    expect(peerChip.className).toContain("peer");
    expect(peerChip.getAttribute("data-node")).toBe("mac");
  });

  it("locates a deep-linked row by opening its card", async () => {
    window.history.replaceState(null, "", "/m?session=zuan:n:e");
    const t = setup([row("zuan:n:a", { status: "waiting" }), row("zuan:n:e", { status: "finished", origin: "external" })]);
    await online(t);

    expect(screen.getByTestId("mobile-card-zuan:n:e")).toBeTruthy();
    fireEvent.click(screen.getByTestId("mobile-back"));
    // 回到收件箱：这一行是 external finished，在收起的已完成区里；展开后能看到它。
    fireEvent.click(screen.getByTestId("mobile-finished-toggle"));
    expect(screen.getByTestId("mobile-row-zuan:n:e")).toBeTruthy();
  });

  it("opens the card from a row tap and goes back to the inbox", async () => {
    const t = setup([row("n:a", { status: "waiting" })]);
    await online(t);

    fireEvent.click(screen.getByTestId("mobile-row-n:a"));
    expect(screen.getByTestId("mobile-card-n:a")).toBeTruthy();
    fireEvent.click(screen.getByTestId("mobile-back"));
    expect(screen.getByTestId("mobile-inbox").textContent).toContain("需要我");
  });
});

describe("手机端状态词与段名（agora-o975.4）", () => {
  // 2026-10-08 真机反馈第四条：段名「在跑」装的是所有「不需要你」的行——用户在「需要我」里点开一行
  // 只记了「看过」（agora-5gg.21 的降段），行本身没在跑，却被段名说成在跑；行上的词还是英文
  // （working / turn done），看不出看过没看过。
  //
  // 时刻用真实墙上钟附近的值：段位（turn_done 的 12 h 新鲜度窗口，attention.ts）按 wallClock 判，
  // 拿假的 1000 当 now 会让所有 turn_done 都过期降段；`now` 参数同时喂给显示时长，两边就都对了。
  const NOW = Math.floor(Date.now() / 1000);
  const since = (secs: number) => NOW - secs;

  it("段名说「不用你」；没看过的 turn_done 说「回完了」、看过一次的说「已看过」", async () => {
    const seenDone = row("n:seen", { status: "turn_done", status_since: since(180) });
    localStorage.setItem(SEEN_STORAGE_KEY, JSON.stringify([seenKey(seenDone)]));
    const t = setup([seenDone, row("n:done", { status: "turn_done", status_since: since(180) })], undefined, NOW);
    await online(t);

    // 看过的降进「不用你」段（sectionOf 与桌面同源），行上带记号；没看过的还在「需要我」。
    const working = screen.getByTestId("mobile-section-working");
    expect(working.getAttribute("aria-label")).toBe("不用你");
    expect(working.textContent).toContain("已看过");
    expect(working.textContent).not.toContain("回完了");
    const attention = screen.getByTestId("mobile-section-attention");
    expect(attention.textContent).toContain("回完了");
    // 段名不再是「在跑」：跑着的行也在这一段，但段名说的是要不要我管。
    expect(t.ui.container.textContent).not.toContain("在跑 0");
  });

  it("逐状态词：等你 / 等你批准 / 在跑 / 闲着 / 已结束 / 失败 / 说不清", async () => {
    const t = setup(
      [
        row("n:wait", { status: "waiting", status_since: since(180) }),
        row("n:perm", { status: "waiting", status_since: since(180), reason: "permission" }),
        row("n:run", { status: "running", status_since: since(180) }),
        row("n:start", { status: "starting", status_since: since(180) }),
        row("n:idle", { status: "idle", status_since: since(180) }),
        row("n:fail", { status: "failed", status_since: since(180) }),
        row("n:unknown", { status: "unknown", status_since: since(180) }),
        row("n:fin", { status: "finished", status_since: since(180), origin: "external" }),
      ],
      undefined,
      NOW,
    );
    await online(t);

    const status = (id: string) => screen.getByTestId(`mobile-row-${id}`).querySelector(".mobile-status")?.textContent;
    expect(status("n:wait")).toBe("等你 3m");
    expect(status("n:perm")).toBe("等你批准 3m");
    expect(status("n:run")).toBe("在跑 3m");
    expect(status("n:start")).toBe("启动中 3m");
    expect(status("n:idle")).toBe("闲着 3m");
    expect(status("n:fail")).toBe("失败 3m");
    expect(status("n:unknown")).toBe("说不清 3m");
    // FINISHED 的行在收起的一段里：展开才画（行文字与其他段同一条规则）。
    fireEvent.click(screen.getByTestId("mobile-finished-toggle"));
    expect(status("n:fin")).toBe("已结束 3m");
  });

  it("卡片头也用同一份手机端状态词（不是桌面英文）", async () => {
    const t = setup([row("n:wait", { status: "waiting", status_since: since(180) })], undefined, NOW);
    await online(t);
    fireEvent.click(screen.getByTestId("mobile-row-n:wait"));
    expect(screen.getByTestId("mobile-card-n:wait").querySelector(".mobile-status")?.textContent).toBe("等你 3m");
  });
});

describe("手机端运行动态信号（agora-o975.3）", () => {
  it("列表里 running / starting 的符号带 .live，其余不带（与卡片同一类名与同一条 CSS）", async () => {
    const t = setup([
      row("n:run", { status: "running" }),
      row("n:start", { status: "starting" }),
      row("n:done", { status: "turn_done" }),
    ]);
    await online(t);
    const sym = (id: string) => screen.getByTestId(`mobile-row-${id}`).querySelector(".mobile-symbol")?.className ?? "";
    expect(sym("n:run")).toContain("live");
    expect(sym("n:start")).toContain("live");
    expect(sym("n:done")).not.toContain("live");
  });
});

describe("mobile information budget (A52)", () => {
  it("never renders desktop or message-stream surfaces", async () => {
    const t = setup([row("n:a", { status: "waiting" })]);
    await online(t);

    for (const selector of FORBIDDEN_DESKTOP_SURFACES) {
      expect(t.ui.container.querySelectorAll(selector), selector).toHaveLength(0);
    }
  });

  it("mobileSummary takes the first non-empty line and caps at 80 chars", () => {
    expect(mobileSummary(row("x", { progress: "\n第一行\n第二行" }))).toBe("第一行");
    expect(mobileSummary(row("x", { reason: "运行时会话没了" }))).toBe("运行时会话没了");
    const s = mobileSummary(row("x", { detail: "字".repeat(120) }));
    expect(s.endsWith("…")).toBe(true);
    expect(s.length).toBe(80);
  });
});

describe("mobile push entry (agora-thc.7)", () => {
  /** 假 SW 消息总线：jsdom 没有 navigator.serviceWorker，这里只造 add/remove。 */
  function stubServiceWorker() {
    const listeners = new Set<(ev: MessageEvent) => void>();
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: {
        addEventListener: (type: string, fn: (ev: MessageEvent) => void) => {
          if (type === "message") listeners.add(fn);
        },
        removeEventListener: (type: string, fn: (ev: MessageEvent) => void) => {
          if (type === "message") listeners.delete(fn);
        },
      },
    });
    return (data: unknown) => {
      for (const fn of listeners) fn({ data } as MessageEvent);
    };
  }

  it("a push click on a sendable row opens its card with focus in the composer", async () => {
    const fire = stubServiceWorker();
    const t = setup([row("n:done", { status: "turn_done", detail: "做完了" })]);
    await online(t);
    await act(async () => {
      fire({ type: "agora-open-session", session: "n:done" });
    });
    const input = await screen.findByTestId("mobile-next-input");
    expect(document.activeElement).toBe(input);
  });

  it("a push click on a WAITING row opens the decision card and does not focus a composer", async () => {
    const fire = stubServiceWorker();
    const t = setup([
      row("n:wait", {
        status: "waiting",
        reason: "permission",
        pending_decision: { request_id: "r1", summary: "Bash: rm", epoch: 1, host: "claude" },
      }),
    ]);
    await online(t);
    await act(async () => {
      fire({ type: "agora-open-session", session: "n:wait" });
    });
    expect(await screen.findByTestId("mobile-decision-text")).toBeTruthy();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
  });

  it("布局诊断：描边状态跟着存储走，且报告里带着收件箱快照（agora-xu12）", async () => {
    // 收件箱才是问题现场，而设置屏把它卸载掉——所以 (a) 描边开关存起来、切屏后还在，
    // (b) 进收件箱时抓一份布局快照，设置页里复制的报告同时带着收件箱与当前屏的盒子数字。
    const t = setup([row("n:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("mobile-settings-open"));
    const outline = await screen.findByTestId("mobile-diag-outline");
    fireEvent.click(outline);
    expect(outline.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByTestId("mobile-settings-back"));
    expect(await screen.findByTestId("mobile-inbox").then(() => document.querySelector(".mobile")?.getAttribute("data-outline"))).toBe("1");

    fireEvent.click(screen.getByTestId("mobile-settings-open"));
    fireEvent.click(await screen.findByTestId("mobile-diag-report"));
    const text = await screen.findByTestId("mobile-diag-text");
    const parsed = JSON.parse((text as HTMLTextAreaElement).value);
    expect(parsed.inbox.rows.length).toBeGreaterThan(0);
    expect(parsed.inbox.viewport.inner).toHaveLength(2);
    expect(parsed.current).toBeTruthy();
  });

  it("the gear opens settings", async () => {
    const t = setup([row("n:a")]);
    await online(t);
    fireEvent.click(screen.getByTestId("mobile-settings-open"));
    expect(await screen.findByTestId("mobile-settings")).toBeTruthy();
    fireEvent.click(screen.getByTestId("mobile-settings-back"));
    expect(screen.queryByTestId("mobile-settings")).toBeNull();
  });
});

describe("手机端「新建」（预设，agora-prdg.4；A52 回写）", () => {
  // agora-uqpi 的拍板（2026-10-08）：手机要能"开始一件事"，但能起什么冻结在桌面侧的 CLI 预设里——
  // 所以 A52 从那句"永远没有 New Agent"改成"只有预设按钮这一屏、且整屏无文本域"，桌面形态的
  // testid 名单（FORBIDDEN_DESKTOP_SURFACES）一个都不放宽。
  const preset = (name: string, patch: Partial<PresetInfo> = {}): PresetInfo => ({
    name,
    agent_type: "claude",
    working_directory: "/Users/r/code/PktMask",
    args: null,
    prompt: null,
    updated_at: "2026-10-08T00:00:00Z",
    ...patch,
  });

  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

  /** 记录每次请求；GET /api/presets 给列表，POST /api/sessions 给 201。 */
  function presetFetch(
    requests: { url: string; method: string; body: unknown }[],
    presets: PresetInfo[],
    created = "n:new",
  ): FetchLike {
    return async (url, init) => {
      const method = init.method ?? "GET";
      requests.push({ url, method, body: init.body ? JSON.parse(String(init.body)) : null });
      if (url === "/api/presets") return json({ presets });
      if (url === "/api/sessions") return json({ id: created }, 201);
      return json({});
    };
  }

  async function openNewScreen() {
    fireEvent.click(screen.getByTestId("mobile-new-open"));
    await screen.findByTestId("mobile-preset-screen");
    // 打开只触发 GET /api/presets；列表（或空态 / 错误）到了才算这一屏就绪。
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("整屏没有 input / textarea，只列 GET /api/presets 的行，桌面形态的 testid 仍为 0", async () => {
    const requests: { url: string; method: string; body: unknown }[] = [];
    const t = setup(
      [row("n:a", { status: "waiting" })],
      presetFetch(requests, [
        preset("pktmask", { args: "--model opus" }),
        preset("run-tests", { agent_type: "pi", working_directory: "/Users/r/code/agora", prompt: "跑一遍测试\n第二行" }),
      ]),
    );
    await online(t);
    await openNewScreen();

    // 只读：打开这一屏只发生一次 GET /api/presets，没有别的请求。
    expect(requests).toEqual([{ url: "/api/presets", method: "GET", body: null }]);
    const list = screen.getByTestId("mobile-preset-list");
    expect([...list.querySelectorAll("button")].map((b) => b.getAttribute("data-testid"))).toEqual([
      "mobile-preset-pktmask",
      "mobile-preset-run-tests",
    ]);
    // 名称 + agent 徽标 + 目录名 + 参数摘要 + 固定首句（只取第一行）。
    const pktmask = screen.getByTestId("mobile-preset-pktmask");
    expect(pktmask.textContent).toContain("pktmask");
    expect(pktmask.textContent).toContain("Claude");
    expect(pktmask.textContent).toContain("PktMask");
    expect(pktmask.textContent).toContain("--model opus");
    const runTests = screen.getByTestId("mobile-preset-run-tests");
    expect(runTests.textContent).toContain("pi");
    expect(runTests.textContent).toContain("agora");
    expect(runTests.textContent).toContain("跑一遍测试");
    expect(runTests.textContent).not.toContain("第二行");

    // 零打字是这一屏的验收：整屏没有任何 input / textarea。
    expect(t.ui.container.querySelectorAll("input, textarea")).toHaveLength(0);
    // 桌面形态的名单一个都不能放宽（A52 回写只是把"创建"换成"只有预设按钮"）。
    for (const selector of FORBIDDEN_DESKTOP_SURFACES) {
      expect(t.ui.container.querySelectorAll(selector), selector).toHaveLength(0);
    }
  });

  it("点一条直接发 POST /api/sessions {preset}（没有二次确认），行进列表后落在卡片", async () => {
    const requests: { url: string; method: string; body: unknown }[] = [];
    const t = setup([row("n:a")], presetFetch(requests, [preset("pktmask", { args: "--model opus" })]));
    await online(t);
    await openNewScreen();

    fireEvent.click(screen.getByTestId("mobile-preset-pktmask"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(requests.filter((r) => r.method === "POST")).toEqual([
      { url: "/api/sessions", method: "POST", body: { preset: "pktmask" } },
    ]);
    // 不要第二段确认（预设本身就是"预先批准"）。
    expect(screen.queryByRole("dialog")).toBeNull();
    // 201 先到、行还没来：停在原屏不闪回收件箱（与桌面 pendingOpen 同一条 race）。
    expect(screen.getByTestId("mobile-preset-screen")).toBeTruthy();

    // 新行随 session_created 进列表 → 落在它的卡片；可发送的行把焦点放进现成的 composer（第一句在那儿说）。
    await act(async () => {
      t.sock.onmessage?.({
        data: JSON.stringify([{ type: "session_created", id: "n:new", session: row("n:new", { status: "turn_done" }) }]),
      });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(screen.getByTestId("mobile-card-n:new")).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByTestId("mobile-next-input"));
  });

  it("空预设：显示 agora preset add 的 CLI 指引，仍然零打字", async () => {
    const t = setup([row("n:a")], presetFetch([], []));
    await online(t);
    await openNewScreen();
    const empty = screen.getByTestId("mobile-preset-empty");
    expect(empty.textContent).toContain("agora preset add");
    expect(empty.textContent).toContain("--agent");
    expect(t.ui.container.querySelectorAll("input, textarea")).toHaveLength(0);
  });

  it("失败留在原屏 + 中文错误：未知预设不落卡片", async () => {
    const t = setup([row("n:a")], async (url) => {
      if (url === "/api/presets") return json({ presets: [preset("gone")] });
      return json({ error: "preset_unknown", message: "未知预设 gone" }, 404);
    });
    await online(t);
    await openNewScreen();

    fireEvent.click(screen.getByTestId("mobile-preset-gone"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(screen.getByTestId("mobile-preset-error").textContent).toContain("未知预设 gone");
    expect(screen.getByTestId("mobile-preset-screen")).toBeTruthy();
    expect(screen.queryByTestId("mobile-card-n:new")).toBeNull();
  });
});

describe("手机端清理「已完成」（agora-off0）", () => {
  /** FINISHED 且还在新鲜度窗口内：没看过的 agora 行才可能留在「需要我」（agora-82x7）。 */
  const freshSince = () => Math.floor(Date.now() / 1000) - 60;

  /** `origin = external` 的 FINISHED 不看「看过」，直接进「已完成」段。 */
  function external(id: string, patch: Partial<SessionRow> = {}): SessionRow {
    return row(id, { status: "finished", status_since: freshSince(), origin: "external", ...patch });
  }

  /** 已看过的 agora FINISHED 行：给 localStorage 种上 seenKey，才落进「已完成」段（与桌面同一条「看过」）。 */
  function seenAgora(id: string, patch: Partial<SessionRow> = {}): SessionRow {
    const r = row(id, { status: "finished", status_since: freshSince(), origin: "agora", ...patch });
    localStorage.setItem(SEEN_STORAGE_KEY, JSON.stringify([seenKey(r)]));
    return r;
  }

  /** 记录写端点：删除走真实的 `api.deleteMetadata`（enc 出的路径与桌面逐行 DELETE 同一条）。 */
  function recordingFetch(requests: { url: string; method: string }[]): FetchLike {
    return async (url, init) => {
      const method = init.method ?? "GET";
      requests.push({ url, method });
      return method === "DELETE"
        ? new Response(null, { status: 204 })
        : new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
    };
  }

  const deletes = (requests: { url: string; method: string }[]) =>
    requests.filter((r) => r.method === "DELETE").map((r) => r.url).sort();

  async function confirm(name: string) {
    fireEvent.click(screen.getByRole("button", { name }));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("①清理入口只在「已完成」段有行时出现（判据同 sectionOf(r, seen)）", async () => {
    // 没看过的 agora FINISHED（且在新鲜度窗口内）留在「需要我」，不在「已完成」段 → 不出入口。
    const t = setup([row("n:run", { status: "running" }), row("n:own", { status: "finished", status_since: freshSince(), origin: "agora" })]);
    await online(t);
    expect(screen.getByTestId("mobile-section-attention").textContent).toContain("n:own");
    expect(screen.queryByTestId("mobile-clear-finished")).toBeNull();
    cleanup();

    const t2 = setup([row("n:run", { status: "running" }), external("n:ext")]);
    await online(t2);
    // 段默认收起：行不画但计数照数（与桌面同一条），入口与段一起出现。
    expect(screen.getByTestId("mobile-finished-toggle").textContent).toContain("1");
    expect(screen.getByTestId("mobile-clear-finished")).toBeTruthy();
  });

  it("②确认后对「已完成」段每一行逐行发 DELETE /api/sessions/:id（没有批量端点）", async () => {
    const requests: { url: string; method: string }[] = [];
    const t = setup([external("n:ext1"), external("n:ext2"), seenAgora("n:own")], recordingFetch(requests));
    await online(t);

    fireEvent.click(screen.getByTestId("mobile-clear-finished"));
    await confirm("删除 3 行");
    expect(deletes(requests)).toEqual(["/api/sessions/n%3Aext1", "/api/sessions/n%3Aext2", "/api/sessions/n%3Aown"]);
    expect(screen.getByTestId("mobile-clear-note").textContent).toBe("已清理 3 行");
  });

  it("③跳过 stale 的 peer 行并计入 skipped", async () => {
    const requests: { url: string; method: string }[] = [];
    const t = setup([external("n:ext"), external("z:peer", { node: "z", stale: true })], recordingFetch(requests));
    await online(t);

    // 确认框与入口都只数实际会删的行：stale 的一跳转发到不了，不算。
    fireEvent.click(screen.getByTestId("mobile-clear-finished"));
    expect(screen.getByRole("dialog").textContent).toContain("1 行");
    await confirm("删除 1 行");
    expect(deletes(requests)).toEqual(["/api/sessions/n%3Aext"]);
    expect(screen.getByTestId("mobile-clear-note").textContent).toBe("已清理 1 行，跳过 1 行（节点离线）");
  });

  it("④确认框报实际会删的行数（去掉 stale）并提示其中多少行是 agora 起的会话", async () => {
    const t = setup(
      [external("n:ext"), seenAgora("n:own"), row("n:hd", { status: "running", origin: "headless" }), external("z:peer", { node: "z", stale: true })],
      recordingFetch([]),
    );
    await online(t);

    // 4 行在「已完成」里，实际会删 3 行；headless / external 没有运行时会话，不算「agora 起的会话」。
    expect(screen.getByTestId("mobile-clear-finished").textContent).toBe("清理 3 行");
    fireEvent.click(screen.getByTestId("mobile-clear-finished"));
    const dialog = screen.getByRole("dialog");
    expect(dialog.textContent).toContain("将删除「已完成」区里 3 行的记录");
    expect(dialog.textContent).toContain("其中 1 行是 agora 起的会话");
  });

  it("⑤没看过的 FINISHED 行不在该段、因此清不到", async () => {
    const requests: { url: string; method: string }[] = [];
    const unseen = row("n:own", { status: "finished", status_since: freshSince(), origin: "agora" });
    const t = setup([unseen, external("n:ext")], recordingFetch(requests));
    await online(t);
    expect(screen.getByTestId("mobile-section-attention").textContent).toContain("n:own");

    fireEvent.click(screen.getByTestId("mobile-clear-finished"));
    await confirm("删除 1 行");
    expect(deletes(requests)).toEqual(["/api/sessions/n%3Aext"]);
    // 清完之后没看过的那一行还在「需要我」，行数也没变。
    expect(screen.getByTestId("mobile-section-attention").textContent).toContain("n:own");
  });
});

/**
 * 回到前台就对齐（agora-f068）：iOS 把 PWA 挂起后 WS 半死、onclose 可能几十秒后才来，
 * 这期间列表停在旧状态。可见性事件是主力（挂起时定时器被冻结，心跳救不了锁屏）；
 * 心跳负责“页面活着但 socket 半死”那一类。这里用 jsdom 派发真事件，走 EventsClient 装的监听。
 */
describe("回到前台与断线重连（agora-f068）", () => {
  /** 快照可换、socket 可造多个的现场：复用不了上面那个只认一份固定快照的 setup。 */
  function setupLive(initial: SessionRow[]) {
    let snapshot = initial;
    const sockets: FakeSocket[] = [];
    const store = new SessionStore({
      connect: () => {
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
      fetchSnapshot: async () => ({ sessions: snapshot, unregistered: [] }),
      coalesceMs: 0,
      reconnectMinMs: 100,
    });
    const health = new HealthWatcher({ fetchHealth: async () => ({ status: "ok" }) });
    const version = new VersionWatcher({ fetchSystem: async () => ({ node: "zuan", api_version: { major: 1, minor: 9 } }) });
    const ui = render(<MobileApp store={store} health={health} version={version} now={1000} />);
    return { ui, store, sockets, setSnapshot: (rows: SessionRow[]) => void (snapshot = rows) };
  }

  async function open(t: ReturnType<typeof setupLive>) {
    await act(async () => {
      t.sockets[0].onopen?.({});
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("深链的行晚到一步：快照带上它之后要补开一次", async () => {
    window.history.replaceState(null, "", "/m?session=zuan:n:e");
    const t = setupLive([row("zuan:n:a", { status: "waiting" })]);
    await open(t);
    // 第一份快照里没有那一行（推送录的是刚起的会话，列表可能晚一步）——深链不能就这么沉默。
    expect(screen.queryByTestId("mobile-card-zuan:n:e")).toBeNull();

    // 下一次全量带来了那一行：自动补开一次（deepLinked 只在真找到行时才置位）。
    t.setSnapshot([row("zuan:n:a", { status: "waiting" }), row("zuan:n:e", { status: "turn_done" })]);
    await act(async () => {
      await t.store.client.refresh();
    });
    expect(screen.getByTestId("mobile-card-zuan:n:e")).toBeTruthy();
  });

  it("深链与回到前台串成一条：重拉带来的那一行也要能打开", async () => {
    window.history.replaceState(null, "", "/m?session=zuan:n:e");
    const t = setupLive([row("zuan:n:a", { status: "waiting" })]);
    await open(t);
    expect(screen.queryByTestId("mobile-card-zuan:n:e")).toBeNull();

    // 推送就是点给刚起的会话的：第一份快照没能带上它。回到前台重拉（pageshow）后才拿到，
    // 深链要找的那一行到了就必须开。
    t.setSnapshot([row("zuan:n:a", { status: "waiting" }), row("zuan:n:e", { status: "turn_done" })]);
    await act(async () => {
      window.dispatchEvent(new Event("pageshow"));
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(t.store.client.snapshots).toBe(2);
    expect(screen.getByTestId("mobile-card-zuan:n:e")).toBeTruthy();
  });

  it("jsdom 里派 visibilitychange 就重拉：回到前台列表立刻是新的", async () => {
    const t = setupLive([row("n:a", { status: "waiting" })]);
    await open(t);
    expect(t.store.client.snapshots).toBe(1);
    expect(screen.getByTestId("mobile-inbox").textContent).toContain("n:a");

    // 挂起期间起了新会话；回到前台派发真事件（EventsClient 装在 document 上）。
    t.setSnapshot([row("n:a", { status: "waiting" }), row("n:b", { status: "turn_done", detail: "新完成" })]);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(t.store.client.snapshots).toBe(2);
    expect(screen.getByTestId("mobile-inbox").textContent).toContain("n:b");
  });

  it("断线重连后列表是新的，不是停在断流前的旧状态", async () => {
    const t = setupLive([row("n:a", { status: "waiting" })]);
    await open(t);
    expect(t.store.client.snapshots).toBe(1);

    // 断流期间一条新会话进来；重连（注入 100 ms 退避）后靠 resync 拿到。
    t.setSnapshot([row("n:a", { status: "waiting" }), row("n:b", { status: "turn_done", detail: "断流期间完成" })]);
    await act(async () => {
      t.sockets[0].onclose?.({});
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 150));
    });
    expect(t.sockets.length).toBe(2);
    await act(async () => {
      t.sockets[1].onopen?.({});
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(t.store.client.snapshots).toBe(2);
    expect(screen.getByTestId("mobile-inbox").textContent).toContain("n:b");
  });
});
