// @vitest-environment jsdom
/**
 * /m 会话卡（agora-thc.10）：IM 语法（两气泡 / 底部 composer / 乐观发送）、composer 状态门、
 * WAITING 三种分支、Kill / Restart 的两步确认、A52 的 DOM 预算。
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sessionApi, type FetchLike } from "./api";
import type { SessionRow } from "./events";
import { FOLD_LINES, MobileCard } from "./MobileCard";
import { seenKey } from "./attention";

function row(id: string, patch: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    node: "zuan",
    status: "turn_done",
    alive: true,
    display_name: id,
    agent_type: "claude",
    reason: null,
    respond_via: "hook",
    // 默认是一条服务端的普通行：活着、由 agora 管的运行时（agora-prdg.2 起只有这一种是
    // `text_via = runtime`；采纳 / 死 pane 的行是 none，见 "read-only rows" 那组用例）。
    runtime_ref: `tmux:agora:${id}`,
    text_via: "runtime",
    managed: true,
    ...patch,
  };
}

function setup(r: SessionRow, killRequiresConfirm = true, onSeen = vi.fn()) {
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    if (url.endsWith("/kill") && killRequiresConfirm && init.body === "{}") {
      return json({ error: "needs_confirmation", message: "会杀掉运行中的进程" }, 409);
    }
    return json({});
  };
  const ui = render(<MobileCard row={r} api={sessionApi(f)} now={1000} onBack={vi.fn()} onSeen={onSeen} />);
  return { ui, requests, onSeen };
}

/**
 * 发送失败的桩（agora-jidm）：`/input` 回指定的错误体，其余端点 200。
 * 手机能不能发是**行上** `text_via` 说了算（服务端的老 / 新看法允许不一致），所以这些用例用
 * 能开出 composer 的行去撞服务端的 4xx/5xx——这正是日期不一致或运行时刚没的现场。
 */
function setupInputFailure(r: SessionRow, status: number, body: { error: string; message: string }) {
  const requests: { url: string; method: string; body: string | undefined }[] = [];
  const f: FetchLike = async (url, init) => {
    const method = init.method ?? "GET";
    requests.push({ url, method, body: init.body as string | undefined });
    const json = (payload: unknown, code = 200) =>
      new Response(JSON.stringify(payload), { status: code, headers: { "content-type": "application/json" } });
    return url.endsWith("/input") ? json(body, status) : json({});
  };
  const ui = render(<MobileCard row={r} api={sessionApi(f)} now={1000} onBack={vi.fn()} onSeen={vi.fn()} />);
  return { ui, requests };
}

async function typeAndSend(text: string) {
  fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: text } });
  fireEvent.click(screen.getByTestId("mobile-send"));
  await act(async () => {});
}

afterEach(cleanup);

describe("卡片吸顶结构（agora-o975.1）", () => {
  it("头与 composer 是卡片这一滚动容器的两端（sticky 在同一 scrollport 里才成立）", () => {
    // 旧写法把滚动交给 .mobile-card，但头不是 sticky、composer 没有 z-index：长内容一滚两者都
    // 会被气泡盖住 / 滚出视野。结构上必须仍然是「头在最前、composer 在最后、同一个滚动容器」。
    const { ui } = setup(row("n:a", { prompt: "q", detail: "a" }));
    const card = ui.container.querySelector(".mobile-card");
    expect(card).toBeTruthy();
    expect(card!.firstElementChild?.className).toContain("mobile-card-head");
    const composer = card!.querySelector(".mobile-composer");
    expect(composer, "composer 存在").toBeTruthy();
    expect(composer!.closest(".mobile-card"), "composer 与头共用同一个滚动容器").toBe(card);
  });
});

describe("运行动态信号（agora-o975.3）", () => {
  it("running / starting 的符号带 live 类，静态状态不带（列表与卡片同一类名）", () => {
    const a = setup(row("n:run", { status: "running", detail: "跑" }));
    expect(a.ui.container.querySelector(".mobile-symbol")?.className).toContain("live");
    cleanup();
    const b = setup(row("n:start", { status: "starting", detail: "起" }));
    expect(b.ui.container.querySelector(".mobile-symbol")?.className).toContain("live");
    cleanup();
    const c = setup(row("n:done", { status: "turn_done", detail: "完了" }));
    expect(c.ui.container.querySelector(".mobile-symbol")?.className).not.toContain("live");
  });

  it("在跑的行：卡片内的时长每秒走一格（1 s 心跳）", () => {
    // 真机反馈第 3 条：<1min 时长以前不显示，刚发出去的几十秒看着完全静止。卡片里这一行在跑时
    // 每秒重算一次时长；收件箱列表保持 30 s（别让整屏每秒重排）。
    vi.useFakeTimers();
    try {
      setup(row("n:run", { status: "running", status_since: 990, detail: "跑" }));
      const status = () => screen.getByTestId("mobile-card-n:run").querySelector(".mobile-status")?.textContent;
      expect(status()).toBe("在跑 10s");
      act(() => vi.advanceTimersByTime(1000));
      expect(status()).toBe("在跑 11s");
      act(() => vi.advanceTimersByTime(1000));
      expect(status()).toBe("在跑 12s");
    } finally {
      vi.useRealTimers();
    }
  });

  it("不在跑的行不心跳：同样的 2 s 推不出新时长（别让静态卡片白跑计时器）", () => {
    vi.useFakeTimers();
    try {
      setup(row("n:done", { status: "turn_done", status_since: 990, detail: "完了" }));
      const status = () => screen.getByTestId("mobile-card-n:done").querySelector(".mobile-status")?.textContent;
      expect(status()).toBe("待查看 10s");
      act(() => vi.advanceTimersByTime(1000));
      expect(status()).toBe("待查看 10s");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("在跑的行画活动行、不把工具名当「最新回复」（agora-dflx）", () => {
  it("running + progress=bash：说人话，工具名不再出现在 agent 槽；用户自己的话仍是气泡", () => {
    setup(row("n:run", { status: "running", prompt: "跑一下测试", progress: "bash", detail: "bash" }));
    const activity = screen.getByTestId("mobile-activity");
    expect(activity.textContent).toContain("进行中");
    expect(activity.textContent).toContain("正在运行命令");
    expect(activity.textContent).not.toContain("bash");
    expect(screen.queryByTestId("mobile-bubble-agent")).toBeNull();
    expect(screen.getByTestId("mobile-bubble-user").textContent).toContain("跑一下测试");
  });

  it("缺 progress 的旧节点退回 detail；detail 等于 prompt 首行时按没有活动处理", () => {
    setup(row("n:old", { status: "running", progress: null, detail: "read" }));
    expect(screen.getByTestId("mobile-activity").textContent).toContain("正在读取文件");
    cleanup();
    // prompt 刚提交、活动还没来：detail 就是用户自己的话，不能再画一遍
    setup(row("n:echo", { status: "running", prompt: "把配置迁到 yaml", detail: "把配置迁到 yaml" }));
    expect(screen.getByTestId("mobile-activity").textContent).toContain("正在处理…");
    expect(screen.queryByTestId("mobile-bubble-agent")).toBeNull();
    cleanup();
    // 没有活动 token（generic / 采纳的 shell）：也是「正在处理…」，不是空槽；starting 同理
    setup(row("n:none", { status: "running" }));
    expect(screen.getByTestId("mobile-activity").textContent).toContain("正在处理…");
    cleanup();
    setup(row("n:start", { status: "starting" }));
    expect(screen.getByTestId("mobile-activity").textContent).toContain("进行中");
  });

  it("未列出的工具保留原名", () => {
    setup(row("n:mcp", { status: "running", progress: "mcp__github__search" }));
    expect(screen.getByTestId("mobile-activity").textContent).toContain("正在使用 mcp__github__search");
  });

  it("turn_done 的真回复仍是「最新回复」气泡，不画活动行", () => {
    setup(row("n:done", { status: "turn_done", prompt: "问", detail: "答" }));
    expect(screen.queryByTestId("mobile-activity")).toBeNull();
    const agent = screen.getByTestId("mobile-bubble-agent");
    expect(agent.textContent).toContain("最新回复");
    expect(agent.textContent).toContain("答");
  });

  it("waiting 分支不动：决策区之外没有活动行", () => {
    setup(row("n:w", {
      status: "waiting",
      reason: "permission",
      respond_via: "hook",
      detail: "Bash: rm -rf build",
      pending_decision: { request_id: "r1", summary: "Bash: rm -rf build", epoch: 1 },
    }));
    expect(screen.queryByTestId("mobile-activity")).toBeNull();
    expect(screen.getByTestId("mobile-decision")).toBeTruthy();
  });
});

describe("发送链路（agora-o975.2）", () => {
  /**
   * 可控的假 api：`/input` 的响应挂着不收，直到 `release()`——把「在途」定格。真机上宿主 ack 最长
   * 等 10 s（`input_ack_wait`），这段时间以前只有气泡里一行小字，看不出「在路上」。(2026-10-08)
   */
  function setupPending(r: SessionRow) {
    const inputBodies: string[] = [];
    let release!: (body: unknown) => void;
    const input = new Promise<Response>((resolve) => {
      release = (body: unknown) =>
        resolve(new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } }));
    });
    const f: FetchLike = async (url, init) => {
      if (url.endsWith("/input")) {
        inputBodies.push(String(init.body));
        return input;
      }
      return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
    };
    const api = sessionApi(f);
    const ui = render(<MobileCard row={r} api={api} now={1000} onBack={vi.fn()} onSeen={vi.fn()} />);
    return { ui, release, api, inputBodies };
  }

  const card = (r: SessionRow, api: ReturnType<typeof sessionApi>) => (
    <MobileCard row={r} api={api} now={1000} onBack={vi.fn()} onSeen={vi.fn()} />
  );

  it("①在途：按钮变「发送中…」+ 转圈禁用、输入框仍可编辑、气泡标签仍是「发送中…」", async () => {
    const { release } = setupPending(row("n:a", { status: "turn_done" }));
    await typeAndSend("把菜单加到侧栏");

    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("发送中…");
    const send = screen.getByTestId("mobile-send") as HTMLButtonElement;
    expect(send.textContent).toContain("发送中…");
    expect(send.querySelector(".mobile-spinner"), "转圈元素").toBeTruthy();
    expect(send.disabled, "在途只锁发送按钮").toBe(true);
    // 审查修订 ①（2026-10-08）：在途锁输入框会挡住「接着打下一句」，所以输入框不锁。
    expect((screen.getByTestId("mobile-next-input") as HTMLInputElement).readOnly).toBe(false);
    // 收尾：把悬挂的 promise 放掉，不留一个永远在途的请求。
    await act(async () => {
      release({});
    });
  });

  it("在途防重：Enter 连击 / 重复提交不得发两遍，第二次的草稿不丢", async () => {
    // 审查修订 ②（2026-10-08）：发送按钮在途禁用，但输入框可编辑（修订 ①），Enter 还能提交表单——
    // submit() 必须自己挡住第二次；挡的时候草稿留在框里（等 ack 回来还能发），不能吞掉。
    const { release, inputBodies } = setupPending(row("n:a", { status: "turn_done" }));
    await typeAndSend("第一条");
    const input = screen.getByTestId("mobile-next-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "第二条" } });
    fireEvent.submit(input.closest("form")!);
    await act(async () => {});
    expect(inputBodies, "在途只发一次").toHaveLength(1);
    expect(input.value).toBe("第二条");
    await act(async () => {
      release({});
    });
    // ack 回来之后按钮又能按：这一句还能发出去（不是扣着不放）。
    expect((screen.getByTestId("mobile-send") as HTMLButtonElement).disabled).toBe(false);
  });

  it("③等它接手 20 s 还没动静：补一句出口话（别让提示永远挂着）", async () => {
    // 审查修订 ③（2026-10-08）：prompt 首行没回显、状态还在 turn_done/idle，20 s 后补一句
    // 「它还没接手——一直没动静就去桌面看看」；撤销条件不变（这里还没撤，所以只测兜底那句话）。
    vi.useFakeTimers();
    try {
      const { release } = setupPending(row("n:a", { status: "turn_done", prompt: "上一句" }));
      await typeAndSend("再跑一遍测试");
      await act(async () => {
        release({});
      });
      expect(screen.getByTestId("mobile-await-takeover")).toBeTruthy();
      expect(screen.queryByTestId("mobile-await-stalled")).toBeNull();
      act(() => vi.advanceTimersByTime(19_000));
      expect(screen.queryByTestId("mobile-await-stalled"), "不到 20 s 不吓人").toBeNull();
      act(() => vi.advanceTimersByTime(1_000));
      expect(screen.getByTestId("mobile-await-stalled").textContent).toBe("它还没接手——一直没动静就去桌面看看");
    } finally {
      vi.useRealTimers();
    }
  });

  it("②ack 成功（turn_done 行）：标签「已发出」+ aria-live 提示「已发出，等它接手…」带动态点", async () => {
    const { release } = setupPending(row("n:a", { status: "turn_done", prompt: "上一句" }));
    await typeAndSend("再跑一遍测试");
    await act(async () => {
      release({});
    });

    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发出");
    const hint = screen.getByTestId("mobile-await-takeover");
    expect(hint.getAttribute("aria-live")).toBe("polite");
    expect(hint.textContent).toContain("已发出，等它接手…");
    expect(hint.querySelector(".mobile-pending-dot"), "动态点").toBeTruthy();
    // 输入框全程可写（审查修订 ①：在途也不锁）。
    expect((screen.getByTestId("mobile-next-input") as HTMLInputElement).readOnly).toBe(false);
  });

  it("②ack 成功（host 且在跑）：标签「已排队」，原有排队文案不动，不弹「等它接手」", async () => {
    const { release } = setupPending(
      row("zuan:bb", { agent_type: "pi", origin: "external", status: "running", text_via: "host", detail: "bash" }),
    );
    await typeAndSend("做完顺手跑一下测试");
    await act(async () => {
      release({});
    });

    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已排队");
    expect(screen.getByTestId("mobile-host-running-note").textContent).toContain("等它跑完这一轮就交进去");
    // 它已经在跑：接手提示是给「我发完它才动起来」那一段的（turn_done / idle）。
    expect(screen.queryByTestId("mobile-await-takeover")).toBeNull();
  });

  it("③等它接手：row.prompt 首行回显时提示与乐观气泡一起擂掉", async () => {
    const { release, api, ui } = setupPending(row("n:a", { status: "turn_done", prompt: "上一句" }));
    await typeAndSend("再跑一遍测试");
    await act(async () => {
      release({});
    });
    expect(screen.getByTestId("mobile-await-takeover")).toBeTruthy();

    // 服务端把这句话收进 prompt（首行相等）：乐观气泡让位给真实投影，提示随之擂掉。
    ui.rerender(card(row("n:a", { status: "turn_done", prompt: "再跑一遍测试" }), api));
    expect(screen.queryByTestId("mobile-await-takeover")).toBeNull();
    expect(screen.queryByTestId("mobile-sent-state")).toBeNull();
  });

  it("③等它接手：row.status 离开 turn_done/idle（它动起来了）时擂掉", async () => {
    const { release, api, ui } = setupPending(row("n:a", { status: "turn_done", prompt: "上一句" }));
    await typeAndSend("再跑一遍测试");
    await act(async () => {
      release({});
    });
    expect(screen.getByTestId("mobile-await-takeover")).toBeTruthy();

    ui.rerender(card(row("n:a", { status: "running", prompt: "上一句" }), api));
    expect(screen.queryByTestId("mobile-await-takeover")).toBeNull();
  });
});

describe("thread", () => {
  it("renders the last turn as two bubbles and folds the reply to 6 lines with one expand", () => {
    const reply = Array.from({ length: 10 }, (_, i) => `第 ${i + 1} 行`).join("\n");
    setup(row("n:a", { prompt: "加一个菜单", detail: reply }));

    expect(screen.getByTestId("mobile-bubble-user").textContent).toContain("加一个菜单");
    const agent = screen.getByTestId("mobile-bubble-agent");
    expect(agent.textContent).toContain(`第 ${FOLD_LINES} 行`);
    expect(agent.textContent).not.toContain(`第 ${FOLD_LINES + 1} 行`);

    fireEvent.click(screen.getByTestId("mobile-expand"));
    expect(screen.getByTestId("mobile-bubble-agent").textContent).toContain("第 10 行");
    expect(screen.queryByTestId("mobile-expand")).toBeNull();
    // 只允许展开最后一条：没有更早消息与翻页的出口。
    expect(screen.queryByTestId(/mobile-history|mobile-stream|mobile-load-more/)).toBeNull();
  });
});

describe("composer", () => {
  it("explains an empty thread instead of leaving the card blank", () => {
    // 现场（2026-10-07，agora-sd0b）：pi 的 external 行 reload 后只有登记——没有 ❯ 也没有 ↳，
    // 手机打开就是一张空卡。无句柄行的说明现在由 mobile-terminal-only 统一承担
    // （agora-71p2：空 thread、有内容、在跑都用这一条），有句柄的才是 mobile-empty-thread。
    setup(row("zuan:aa2462", { agent_type: "pi", origin: "external", status: "idle" }));
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("桌面终端");
    expect(screen.queryByTestId("mobile-bubble-user")).toBeNull();
    expect(screen.queryByTestId("mobile-bubble-agent")).toBeNull();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    cleanup();

    // 有句柄的行同样空 thread（idle）：说"回一条就会出现在这里"，不让它误以为要去桌面。
    setup(row("n:b", { status: "idle" }));
    const handled = screen.getByTestId("mobile-empty-thread").textContent ?? "";
    expect(handled).not.toContain("桌面终端");
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    expect(screen.getByTestId("mobile-next-input")).toBeTruthy();
  });

  it("an external row with content still says where to reply", () => {
    // MISSION §5.5：external 没有终端与文本输入。以前只有空 thread 才说话（agora-sd0b），
    // 有内容时看着像"输入框没画出来"（agora-71p2）。
    setup(row("zuan:x", { agent_type: "pi", origin: "external", status: "turn_done", prompt: "问", detail: "答" }));
    expect(screen.getByTestId("mobile-bubble-agent").textContent).toContain("答");
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("桌面终端");
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();

    // 在跑也一样说清（且不再说"等它停下来再发"——手机永远发不了）。
    cleanup();
    setup(row("zuan:y", { agent_type: "pi", origin: "external", status: "running", detail: "bash" }));
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("在跑");
    expect(screen.queryByTestId("mobile-running-note")).toBeNull();
  });

  it("sends optimistically and settles to 已发出（agora-o975.2 起按是否在跑分「已发出 / 已排队」）", async () => {
    const { requests } = setup(row("n:a"));
    const input = screen.getByTestId("mobile-next-input");
    fireEvent.change(input, { target: { value: "把菜单加到侧栏" } });
    fireEvent.click(screen.getByTestId("mobile-send"));

    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("发送中…");
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "把菜单加到侧栏\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发出");
  });

  it("host text channel: a handleless pi row can be answered from the phone", async () => {
    // ADR-002 D11 / agora-t5kf.3：无句柄但宿主收文本（pi 的扩展自报 input_channel → text_via=host）
    // → 手机能发，走 daemon 的 input/ 队列；只有 text_via=none 的才说"到桌面"。
    const { requests } = setup(
      row("zuan:aa2462", { agent_type: "pi", origin: "external", status: "turn_done", text_via: "host", prompt: "问", detail: "答" }),
    );
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "从手机发一条" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "从手机发一条\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已发出");
  });

  it("host + running: the composer stays open and the note says queued until this run ends", () => {
    // agora-shze：pi 的扩展在会话非空闲时走 `deliverAs: "followUp"`，语义是**等当前这一轮跑完
    // （不再有工具调用）再交进去**，不是"做完当前工具批次"（那是 pi 的 `steer`）。文案必须与这个
    // 事实一致：说"跑完这一轮"；issue 里原本要求的"这一步"是 steer 的时机，写上去就是假话
    // （ADR-002 D11 的实测记录：两次真 TUI 实测 + pi 1.0.4 自己的 SDK 注释）。
    setup(row("zuan:bb", { agent_type: "pi", origin: "external", status: "running", text_via: "host", detail: "bash" }));
    expect(screen.getByTestId("mobile-next-input")).toBeTruthy();
    const note = screen.getByTestId("mobile-host-running-note").textContent ?? "";
    expect(note).toContain("排队");
    expect(note).toContain("跑完这一轮");
    expect(screen.queryByTestId("mobile-terminal-only")).toBeNull();
    expect(screen.queryByTestId("mobile-running-note")).toBeNull();
  });

  it("runtime + running keeps the old gate: no composer, wait for it to stop", () => {
    // runtime（PTY）没有队列：文本直接落进 TUI 的输入区，各家对"跑着的时候打字"解释不同
    // （Claude 排队、别的可能当快捷键），不值得赌——门与文案都不动（agora-shze，issue 的 ①）。
    setup(row("n:rt", { status: "running", runtime_ref: "tmux:agora:n-rt", command: "claude", detail: "在跑" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.getByTestId("mobile-running-note").textContent).toContain("等它停下来");
    expect(screen.queryByTestId("mobile-host-running-note")).toBeNull();
  });

  it("host + running: sending still lands on 已排队", async () => {
    // 发送链路不变：POST /api/sessions/:id/input（daemon 对 host 通道 200 + 排队），
    // 卡片与小节里 host-idle 路径同一套乐观态（agora-shze）。
    const { requests } = setup(
      row("zuan:bb", { agent_type: "pi", origin: "external", status: "running", text_via: "host", detail: "bash" }),
    );
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "做完这个之后顺手跑一下测试" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("发送中…");
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "text", data: "做完这个之后顺手跑一下测试\n" }));
    expect(screen.getByTestId("mobile-sent-state").textContent).toBe("已排队");
  });

  it("聚焦 composer 时把输入框滚进可见区（iOS 键盘不遮它）", () => {
    // agora-x70t.ten0：Safari 不认 interactive-widget，粘性底部的 composer 只能主动滚一下；
    // 等 300ms 是让键盘动画先走起来（jsdom 里 scrollIntoView 未必存在，代码必须可选调用）。
    vi.useFakeTimers();
    const calls: ScrollIntoViewOptions[] = [];
    const proto = Element.prototype as unknown as { scrollIntoView?: (o?: ScrollIntoViewOptions) => void };
    const orig = proto.scrollIntoView;
    proto.scrollIntoView = (o) => calls.push(o ?? {});
    try {
      setup(row("n:a"));
      fireEvent.focus(screen.getByTestId("mobile-next-input"));
      expect(calls).toEqual([]);
      act(() => vi.advanceTimersByTime(400));
      expect(calls).toEqual([{ block: "nearest" }]);
    } finally {
      if (orig) proto.scrollIntoView = orig;
      else delete proto.scrollIntoView;
      vi.useRealTimers();
    }
  });

  it("gates on status: running is disabled with a note, handleless has no composer", () => {
    setup(row("n:a", { status: "running", detail: "在跑" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.getByTestId("mobile-running-note")).toBeTruthy();
    cleanup();

    setup(row("n:b", { status: "turn_done", origin: "external" }));
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
  });
});

describe("read-only rows (agora-prdg.2)", () => {
  // 采纳行：服务端把 `text_via` 修成 `none`（只读 socket 不再谎报可写）；手机这边只给一句
  // 「到桌面」，不画 composer / 「更多」。
  it("an adopted row says 到桌面 instead of drawing a composer or 更多", () => {
    const { ui } = setup(
      row("zuan:adopt", {
        origin: "adopted",
        status: "turn_done",
        runtime_ref: "tmux:other:x",
        text_via: "none",
        managed: false,
        command: "claude",
        prompt: "手工起的",
        detail: "答",
      }),
    );
    expect(screen.getByTestId("mobile-terminal-only").textContent).toContain("桌面终端");
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    // 「更多」整块收掉：没有切换按钮，也不留一个占位的空壳。
    expect(screen.queryByTestId("mobile-more")).toBeNull();
    expect(ui.container.querySelector(".mobile-actions")).toBeNull();
  });

  it("a managed row with a dead pane also loses composer and 更多", () => {
    // 死 pane 的句柄还在列表里（scrollback 保留），但没有可写的 PTY：与采纳行同一档。
    setup(
      row("zuan:dead", {
        status: "finished",
        runtime_ref: "tmux:agora:dead",
        text_via: "none",
        managed: true,
        command: "claude",
      }),
    );
    expect(screen.getByTestId("mobile-terminal-only")).toBeTruthy();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.queryByTestId("mobile-more")).toBeNull();
  });

  it("an old peer still claiming runtime on an adopted row is vetoed by managed=false", () => {
    // agora-prdg.2 修前的服务端把采纳行报成 text_via=runtime；同一条 wire 上早就带着
    // managed=false。手机拿它否决：混版本 / 旧 peer 的行也不给必然 409 的 composer 与 Kill。
    setup(
      row("mac:adopt", {
        origin: "adopted",
        status: "turn_done",
        runtime_ref: "tmux:other:x",
        text_via: "runtime",
        managed: false,
        command: "claude",
      }),
    );
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.queryByTestId("mobile-more")).toBeNull();
  });
});

describe("send failures (agora-jidm)", () => {
  const hostRow = (patch: Partial<SessionRow> = {}) =>
    row("zuan:aa2462", {
      agent_type: "pi",
      origin: "external",
      status: "turn_done",
      text_via: "host",
      prompt: "问",
      detail: "答",
      ...patch,
    });

  it("504 host_timeout: the draft comes back and retry stays usable", async () => {
    // issue ①：失败时草稿不回填，想改一个字要重打整句。②host_timeout 的出口是重试——
    // 超时时队列里那件已经被 daemon 删掉，重试不会跑两遍（src/api/sessions.rs 的超时路径）。
    const { requests } = setupInputFailure(hostRow(), 504, {
      error: "host_timeout",
      message: "zuan:aa2462 的宿主没来取这条输入（扩展没装 / 旧版 / 没停在提示符上）；去它的终端里看看",
    });
    await typeAndSend("再试一句话");

    expect((screen.getByTestId("mobile-next-input") as HTMLInputElement).value).toBe("再试一句话");
    expect(screen.getByTestId("mobile-send-failure").textContent).toContain("宿主没来取这条输入");
    fireEvent.click(screen.getByTestId("mobile-retry"));
    await act(async () => {});
    expect(requests[1].body).toBe(JSON.stringify({ kind: "text", data: "再试一句话\n" }));
  });

  it("502 host_rejected: the host's own words are shown without a wrapper", async () => {
    // 扩展 `.failed` 文件里的原话比任何转述都准：照抄，别加「发送失败：」这类壳。
    setupInputFailure(hostRow(), 502, {
      error: "host_rejected",
      message: "pi.sendUserMessage 失败：当前没有活动会话",
    });
    await typeAndSend("跑一下测试");
    expect(screen.getByTestId("mobile-send-failure").textContent).toBe("pi.sendUserMessage 失败：当前没有活动会话");
  });

  it("409 no_runtime: the phone is told to go to the desktop", async () => {
    // 行上还说 runtime（老 view / 刚没），服务端回 409 no_runtime：手机要给与
    // mobile-terminal-only 同一句人话，不再显示「这一行没有可写的运行时」这种内部话。
    setupInputFailure(
      row("zuan:dead", { status: "turn_done", text_via: "runtime", runtime_ref: "tmux:agora:dead" }),
      409,
      { error: "no_runtime", message: "会话没有运行时会话: zuan:dead" },
    );
    await typeAndSend("你好");
    const failure = screen.getByTestId("mobile-send-failure").textContent ?? "";
    expect(failure).toContain("到桌面");
    expect(failure).not.toContain("没有运行时会话");
  });

  it("409 read_only: an adopted row says it can only be read", async () => {
    // 采纳行（agora-prdg.2）：只读与「没有运行时」是两回事，文案分开说，别显示
    // 「会话不由 agora 管理，拒绝写操作」这种内部话。
    setupInputFailure(
      row("zuan:adopt", { status: "turn_done", origin: "adopted", text_via: "runtime", runtime_ref: "tmux:other:x" }),
      409,
      { error: "read_only", message: "会话不由 agora 管理，拒绝写操作: tmux:other:x" },
    );
    await typeAndSend("你好");
    expect(screen.getByTestId("mobile-send-failure").textContent).toContain("只能看不能写");
  });

  it("404 runtime_session_not_found: a dead pane says its terminal is gone", async () => {
    // issue ③：text_via=runtime 但 pane 已经不在了（降级 / 陈旧行），不能说「会话不存在: <ref>」。
    setupInputFailure(
      row("zuan:dead", { status: "turn_done", text_via: "runtime", runtime_ref: "tmux:agora:dead" }),
      404,
      { error: "runtime_session_not_found", message: "会话不存在: tmux:agora:dead" },
    );
    await typeAndSend("你好");
    expect(screen.getByTestId("mobile-send-failure").textContent).toContain("终端已经不在了");
  });
});

describe("waiting", () => {
  it("decides via the hook with the pending request_id", async () => {
    const r = row("n:a", {
      status: "waiting",
      reason: "permission",
      respond_via: "hook",
      pending_decision: { request_id: "req-1", summary: "Bash: git push origin main", epoch: 1 },
    });
    const { requests } = setup(r);
    expect(screen.getByTestId("mobile-decision-text").textContent).toBe("Bash: git push origin main");
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();

    fireEvent.click(screen.getByTestId("mobile-allow"));
    await act(async () => {});
    expect(requests[0].body).toBe(JSON.stringify({ kind: "decision", decision: "allow", request_id: "req-1" }));
  });

  it("shows 需要到桌面 for terminal-only decisions instead of a composer", () => {
    setup(row("n:a", { status: "waiting", reason: "permission", respond_via: "terminal", detail: "Bash: rm -rf build" }));
    expect(screen.getByTestId("mobile-decision-text").textContent).toBe("Bash: rm -rf build");
    expect(screen.getByTestId("mobile-needs-desktop")).toBeTruthy();
    expect(screen.queryByTestId("mobile-next-input")).toBeNull();
    expect(screen.queryByTestId("mobile-allow")).toBeNull();
  });
});

describe("kill / restart", () => {
  it("confirms through the node's 409 before sending confirmed: true", async () => {
    const { requests } = setup(row("n:a", { status: "running", command: "claude" }));
    fireEvent.click(screen.getByTestId("mobile-more"));
    fireEvent.click(screen.getByTestId("mobile-kill"));
    await act(async () => {});
    expect(requests[0].body).toBe("{}");

    const dialogs = screen.getAllByRole("dialog");
    expect(dialogs.length).toBe(1);
    fireEvent.click(screen.getByText("Kill", { selector: ".dialog-actions button" }));
    await act(async () => {});
    expect(requests[1].body).toBe(JSON.stringify({ confirmed: true }));
  });

  it("hides actions for handleless rows", () => {
    setup(row("n:a", { origin: "headless" }));
    expect(screen.queryByTestId("mobile-more")).toBeNull();
  });
});

describe("information budget (A52)", () => {
  it("never renders terminal / creation / diff / history surfaces", () => {
    const { ui } = setup(row("n:a", { prompt: "q", detail: "a" }));
    const forbidden = [
      '[data-testid^="term"]',
      '[data-testid="create"]',
      '[data-testid^="diff-"]',
      '[data-testid^="acceptance-"]',
      '[data-testid^="changes-"]',
      '[data-testid^="respond-"]',
      '[data-testid^="mobile-stream"]',
      '[data-testid="mobile-load-more"]',
      ".xterm",
      ".terminal",
      ".mobile-stream",
      ".mobile-history",
    ];
    for (const selector of forbidden) expect(ui.container.querySelectorAll(selector), selector).toHaveLength(0);
  });

  it("marks a turn_done row as seen when the card opens", () => {
    const onSeen = vi.fn();
    setup(row("n:a", { status: "turn_done", status_since: 500 }), true, onSeen);
    expect(onSeen).toHaveBeenCalledWith(seenKey({ id: "n:a", status: "turn_done", status_since: 500 }));
  });
});


it("offline cards retain their content but disable sending and decisions (agora-d0r)", () => {
  setup(row("n:offline", { stale: true, detail: "上次的回复" }));
  expect(screen.getByRole("status").textContent).toContain("节点离线");
  expect((screen.getByLabelText("下一条指令") as HTMLTextAreaElement).disabled).toBe(true);
  expect((screen.getByTestId("mobile-send") as HTMLButtonElement).disabled).toBe(true);
  cleanup();
  setup(row("n:waiting", {stale: true, status: "waiting", reason: "permission", pending_decision: {request_id: "r", summary: "npm test", epoch: 1, host: "claude"}}));
  expect((screen.getByTestId("mobile-allow") as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByTestId("mobile-deny") as HTMLButtonElement).disabled).toBe(true);
});


it("a failed reply cannot be retried after its node goes offline (agora-d0r)", async () => {
  const input = vi.fn().mockResolvedValue({ ok: false, needsConfirmation: false, error: { error: "host_timeout", message: "节点未响应" } });
  const api = { ...sessionApi(), input };
  const renderCard = (stale: boolean) => <MobileCard row={row("n:a", { stale })} api={api} now={1000} onBack={vi.fn()} />;
  const ui = render(renderCard(false));
  await typeAndSend("再试一次");
  expect(input).toHaveBeenCalledTimes(1);
  ui.rerender(renderCard(true));
  expect((screen.getByTestId("mobile-retry") as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByTestId("mobile-retry"));
  expect(input).toHaveBeenCalledTimes(1);
});

describe("composer 附图（agora-lmz2）", () => {
  const PATH = "/w/.agora-uploads/1700000000000-abcdef.png";
  // jsdom 没有 object URL：缩略图只要一个可比的 src。
  const created: string[] = [];
  const original = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
  beforeEach(() => {
    created.length = 0;
    let n = 0;
    URL.createObjectURL = vi.fn(() => {
      const url = `blob:thumb-${++n}`;
      created.push(url);
      return url;
    });
    URL.revokeObjectURL = vi.fn();
  });
  afterEach(() => {
    URL.createObjectURL = original.create;
    URL.revokeObjectURL = original.revoke;
  });

  const png = (name = "shot.png") => new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3])], name, { type: "image/png" });
  const PNG_B64 = btoa(String.fromCharCode(0x89, 0x50, 0x4e, 0x47, 1, 2, 3));

  /** `/images` 与 `/input` 各自回什么由用例给；其余端点 200。 */
  function setupImages(images: (n: number) => Response, input: (n: number) => Response = () => json({})) {
    const requests: { url: string; method: string; body: string | undefined }[] = [];
    let ni = 0;
    let nt = 0;
    const f: FetchLike = async (url, init) => {
      requests.push({ url, method: init.method ?? "GET", body: init.body as string | undefined });
      if (url.endsWith("/images")) return images(++ni);
      if (url.endsWith("/input")) return input(++nt);
      return json({});
    };
    const renderCard = (r: SessionRow) => <MobileCard row={r} api={sessionApi(f)} now={1000} onBack={vi.fn()} onSeen={vi.fn()} />;
    const ui = render(renderCard(row("n:a")));
    return { ui, requests, renderCard };
  }
  function json(body: unknown, status = 200) {
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  }
  const pasteImages = (files: File[], text = "") =>
    fireEvent.paste(screen.getByTestId("mobile-next-input"), {
      clipboardData: { files, items: [], getData: (t: string) => (t === "text/plain" ? text : "") },
    });
  const thumbs = () => screen.queryByTestId("mobile-attachments")?.querySelectorAll("img") ?? [];
  const sendButton = () => screen.getByTestId("mobile-send") as HTMLButtonElement;

  it("粘贴截图出缩略图、可移除；只有图也能发", () => {
    setupImages(() => json({ path: PATH }, 201));
    expect(sendButton().disabled).toBe(true);
    pasteImages([png()]);
    expect(thumbs()).toHaveLength(1);
    expect(thumbs()[0].getAttribute("src")).toBe(created[0]);
    expect(sendButton().disabled).toBe(false);
    fireEvent.click(screen.getByTestId("mobile-attachment-remove"));
    expect(screen.queryByTestId("mobile-attachments")).toBeNull();
    expect(sendButton().disabled).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(created[0]);
  });

  it("选图按钮走隐藏的 file input；一条最多 4 张", () => {
    setupImages(() => json({ path: PATH }, 201));
    const input = screen.getByTestId("mobile-attach-input") as HTMLInputElement;
    expect(input.type).toBe("file");
    expect(input.accept).toBe("image/*");
    fireEvent.change(input, { target: { files: [png("1.png"), png("2.png"), png("3.png"), png("4.png"), png("5.png")] } });
    expect(thumbs()).toHaveLength(4);
    expect(screen.getByTestId("mobile-card-note").textContent).toContain("最多带 4 张");
    expect((screen.getByTestId("mobile-attach") as HTMLButtonElement).disabled).toBe(true);
  });

  it("先 /images 再 /input：input 的 data 接上节点交回的路径，气泡画文字与缩略图、不画路径", async () => {
    const { ui, requests, renderCard } = setupImages(() => json({ path: PATH }, 201));
    pasteImages([png()]);
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "看这张图" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    await waitFor(() => expect(requests.some((r) => r.url.endsWith("/input"))).toBe(true));

    expect(requests.map((r) => r.url)).toEqual(["/api/sessions/n%3Aa/images", "/api/sessions/n%3Aa/input"]);
    expect(JSON.parse(requests[0].body ?? "")).toEqual({ data: PNG_B64 });
    expect(JSON.parse(requests[1].body ?? "")).toEqual({ kind: "text", data: `看这张图 [image: ${PATH}]\n` });
    const bubble = screen.getByTestId("mobile-bubble-user");
    expect(bubble.textContent).toContain("看这张图");
    expect(bubble.textContent).not.toContain(".agora-uploads");
    expect(screen.getByTestId("mobile-bubble-images").querySelectorAll("img")).toHaveLength(1);
    expect(screen.queryByTestId("mobile-attachments")).toBeNull();
    expect((screen.getByTestId("mobile-next-input") as HTMLTextAreaElement).value).toBe("");

    // 服务端把整句收进 prompt：回显按 wire 比对，「等它接手」撤掉；气泡里的图引用说成［图片］。
    expect(screen.getByTestId("mobile-await-takeover")).toBeTruthy();
    ui.rerender(renderCard(row("n:a", { prompt: `看这张图 [image: ${PATH}]` })));
    expect(screen.queryByTestId("mobile-await-takeover")).toBeNull();
    expect(screen.getByTestId("mobile-bubble-user").textContent).toContain("看这张图 ［图片］");
  });

  it("图没传上去就什么都不发：文字与图回到 composer，气泡报原因、不给重试", async () => {
    const { requests } = setupImages(() => json({ error: "bad_image", message: "只收 PNG / JPEG / GIF / WebP 图片" }, 400));
    pasteImages([png()]);
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "这张" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    await waitFor(() => expect(screen.queryByTestId("mobile-send-failure")).toBeTruthy());

    expect(requests.filter((r) => r.url.endsWith("/input"))).toHaveLength(0);
    expect(screen.getByTestId("mobile-send-failure").textContent).toContain("读不出来");
    expect(screen.queryByTestId("mobile-retry")).toBeNull();
    expect((screen.getByTestId("mobile-next-input") as HTMLTextAreaElement).value).toBe("这张");
    expect(thumbs()).toHaveLength(1);
  });

  it("老节点没有 /images（405、正文不是 JSON）：说那台节点版本旧，不静默只发文字", async () => {
    const { requests } = setupImages(() => new Response(null, { status: 405 }));
    pasteImages([png()]);
    fireEvent.change(screen.getByTestId("mobile-next-input"), { target: { value: "带图" } });
    fireEvent.click(screen.getByTestId("mobile-send"));
    await waitFor(() => expect(screen.queryByTestId("mobile-send-failure")).toBeTruthy());
    expect(screen.getByTestId("mobile-send-failure").textContent).toContain("版本旧");
    expect(requests.filter((r) => r.url.endsWith("/input"))).toHaveLength(0);
  });

  it("文字那一步失败：重试只重发同一串，不重传图", async () => {
    const { requests } = setupImages(
      () => json({ path: PATH }, 201),
      (n) => (n === 1 ? json({ error: "host_timeout", message: "宿主没来取" }, 504) : json({})),
    );
    pasteImages([png()]);
    fireEvent.click(screen.getByTestId("mobile-send"));
    await waitFor(() => expect(screen.queryByTestId("mobile-retry")).toBeTruthy());
    fireEvent.click(screen.getByTestId("mobile-retry"));
    await waitFor(() => expect(requests.filter((r) => r.url.endsWith("/input"))).toHaveLength(2));

    expect(requests.filter((r) => r.url.endsWith("/images"))).toHaveLength(1);
    const inputs = requests.filter((r) => r.url.endsWith("/input")).map((r) => JSON.parse(r.body ?? ""));
    expect(inputs[0]).toEqual({ kind: "text", data: `[image: ${PATH}]\n` });
    expect(inputs[1]).toEqual(inputs[0]);
  });

  it("图文混贴：图进缩略图，文字照常进框（不 preventDefault）", () => {
    setupImages(() => json({ path: PATH }, 201));
    const ok = pasteImages([png()], "一段文字");
    expect(thumbs()).toHaveLength(1);
    expect(ok).toBe(true);
    const onlyImage = pasteImages([png()]);
    expect(onlyImage).toBe(false);
  });
});
