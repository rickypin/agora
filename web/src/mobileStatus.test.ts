/**
 * 手机端的状态词与时长（agora-o975.4 / agora-o975.3；MISSION §6.9）。
 *
 * 为什么要自己一份：桌面共用的 `attention.ts` `STATUS_TEXT` 是英文（waiting / turn done），手机上
 * 用户看不懂「看过没看过、排队没排队」；「在跑」段名还会把「点开看过一次、其实没在跑」的行说成在跑。
 * 词表只在这里，桌面一个字不动（守卫：attention.test.ts 的既有用例照旧断言英文）。
 */
import { describe, expect, it } from "vitest";
import type { SessionRow } from "./events";
import { mobileAgo, mobileStatusLine, mobileStatusText } from "./mobileStatus";

function row(patch: Partial<SessionRow> = {}): SessionRow {
  return {
    id: "n:a",
    node: "zuan",
    status: "running",
    alive: true,
    display_name: "a",
    agent_type: "claude",
    reason: null,
    respond_via: "hook",
    ...patch,
  };
}

describe("手机端状态词（agora-o975.4）", () => {
  it("逐状态给中文词：waiting / turn_done / running / starting / idle / finished / failed / unknown", () => {
    expect(mobileStatusText(row({ status: "waiting" }), false)).toBe("等你");
    expect(mobileStatusText(row({ status: "turn_done" }), false)).toBe("回完了");
    expect(mobileStatusText(row({ status: "turn_done", status_since: 500 }), true)).toBe("已看过");
    expect(mobileStatusText(row({ status: "running" }), false)).toBe("在跑");
    expect(mobileStatusText(row({ status: "starting" }), false)).toBe("启动中");
    expect(mobileStatusText(row({ status: "idle" }), false)).toBe("闲着");
    expect(mobileStatusText(row({ status: "finished" }), false)).toBe("已结束");
    expect(mobileStatusText(row({ status: "failed" }), false)).toBe("失败");
    expect(mobileStatusText(row({ status: "unknown" }), false)).toBe("说不清");
  });

  it("等你批准只给 reason=permission 的 waiting；别的 reason 还是「等你」", () => {
    // 权限请求与提问都落 WAITING，但人的动作不一样：一个是照命令批准 / 拒绝，一个是回答问题。
    expect(mobileStatusText(row({ status: "waiting", reason: "permission" }), false)).toBe("等你批准");
    expect(mobileStatusText(row({ status: "waiting", reason: "needs input" }), false)).toBe("等你");
  });

  it("「已看过」只改 turn_done；finished 是「已结束」，不受看过记号影响", () => {
    // 看过的 TURN_DONE 降到「不用你」段（agora-5gg.21），行上要有记号；FINISHED 的归宿是
    // 「已完成」折叠区，行上的词只管说它结束了。
    expect(mobileStatusText(row({ status: "finished" }), true)).toBe("已结束");
    // 记号对别的状态没有副作用。
    expect(mobileStatusText(row({ status: "waiting" }), true)).toBe("等你");
  });

  it("认不出的状态原样显示（旧行 / 新状态），不猜也不翻译", () => {
    expect(mobileStatusText(row({ status: "paused" }), false)).toBe("paused");
  });

  it("行上拼时长：本机精确、peer 带 ≥ 下界（与桌面同一条语义，只是词换了）", () => {
    const local = row({ status: "waiting", status_since: 1000 - 180 });
    expect(mobileStatusLine(local, false, 1000)).toBe("等你 3m");
    const peer = row({ status: "waiting", status_since: 1000 - 180, stale: false });
    expect(mobileStatusLine(peer, false, 1000)).toBe("等你 ≥3m");
    // 没有 status_since 的行（旧节点 / 测试桩）：只说状态，不编时长。
    expect(mobileStatusLine(row({ status: "running" }), false, 1000)).toBe("在跑");
  });

  it("看到过的那一行：行文案说「已看过」，段位交给 sectionOf（同一个记号）", () => {
    const done = row({ status: "turn_done", status_since: 500 });
    expect(mobileStatusLine(done, true, 1000)).toContain("已看过");
    expect(mobileStatusLine(done, false, 1000)).toContain("回完了");
  });
});

describe("手机端时长（agora-o975.3）", () => {
  it("不到一分钟显示秒：42s；60 秒起沿用共用的分钟 / 小时 / 天", () => {
    // 桌面共用的 formatAgo 不到 60s 返回空串——刚发出去的几十秒界面完全静止（真机反馈第 3 条）。
    expect(mobileAgo(0)).toBe("0s");
    expect(mobileAgo(42)).toBe("42s");
    expect(mobileAgo(59.9)).toBe("59s");
    expect(mobileAgo(60)).toBe("1m");
    expect(mobileAgo(180)).toBe("3m");
    expect(mobileAgo(3600)).toBe("1h");
    expect(mobileAgo(49 * 3600)).toBe("2d");
  });

  it("负时长（页面钟比节点快）夹到 0s，不显示 -3s", () => {
    expect(mobileAgo(-3)).toBe("0s");
  });

  it("非有限值不显示（与 formatAgo 一致）", () => {
    expect(mobileAgo(Number.NaN)).toBe("");
    expect(mobileAgo(Number.POSITIVE_INFINITY)).toBe("");
  });

  it("行上在跑的行用秒：刚发出去的 42 秒看得见在走", () => {
    expect(mobileStatusLine(row({ status: "running", status_since: 958 }), false, 1000)).toBe("在跑 42s");
  });

  it("列表用分钟粒度（审查修订 ①）：不到一分钟只写状态词，不写秒", () => {
    // 列表 30 s 才走一格，显示秒会「冻住」反而更像死了；秒级只留给卡片（那里有 1 s 心跳）。
    expect(mobileStatusLine(row({ status: "running", status_since: 958 }), false, 1000, "minutes")).toBe("在跑");
    expect(mobileStatusLine(row({ status: "running", status_since: 820 }), false, 1000, "minutes")).toBe("在跑 3m");
    // peer 的下界语义在分钟粒度下也在。
    expect(mobileStatusLine(row({ status: "waiting", status_since: 820, stale: false }), false, 1000, "minutes")).toBe("等你 ≥3m");
  });
});
