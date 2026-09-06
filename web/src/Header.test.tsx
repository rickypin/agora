// @vitest-environment jsdom
/**
 * Header 对本机与每个 peer 的状态渲染（MISSION §10.3；agora-7ku.12 验收点名的四态：
 * online / stale(last_seen) / fingerprint_mismatch / incompatible_version）。
 * 原因只按 last_error 的类型选文案（规则 10），"上次见到"用本节点打的时间。
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { clockText, Header, nodeStatuses, peerErrorLabel, type NodeStatus } from "./Header";

const SEEN = "2026-09-02T23:10:00Z";

function peer(name: string, over: Partial<NodeStatus> = {}): NodeStatus {
  return { name, online: false, last_seen: null, retrying: false, last_error: null, ...over };
}

function chip(name: string) {
  return screen.getByTestId(`node-${name}`);
}

afterEach(cleanup);

describe("Header 节点状态", () => {
  it("online: name and a green dot, nothing else to read", () => {
    render(<Header agents={3} nodes={[peer("zuan", { online: true, last_seen: SEEN })]} />);
    const c = chip("zuan");
    expect(c.textContent).toBe("zuan●");
    expect(c.className).toContain("ok");
    expect(c.title).toBe(`在线，上次见到 ${SEEN}`);
    // 标题行原样还在。
    expect(screen.getByRole("heading").textContent).toBe("agora");
    expect(screen.getByText("AGENTS 3")).toBeTruthy();
  });

  it("stale: offline peer keeps its last view and says when it was last seen (A29; invariant 8)", () => {
    render(<Header agents={0} nodes={[peer("zuan", { last_seen: SEEN, retrying: true })]} />);
    const c = chip("zuan");
    expect(c.textContent).toBe(`zuan○上次见到 ${clockText(SEEN)}`);
    expect(c.className).toContain("stale");
    expect(c.title).toBe(`离线，上次见到 ${SEEN}，重试中`);
    // 本地时区的 HH:MM——时间是本节点打的，浏览器就在旁边（MISSION §3.5）。
    const d = new Date(SEEN);
    const pad = (n: number) => String(n).padStart(2, "0");
    expect(clockText(SEEN)).toBe(`${pad(d.getHours())}:${pad(d.getMinutes())}`);
    expect(clockText("garbage")).toBe("garbage");
  });

  it("fingerprint_mismatch: its own reason, not '离线' (A30/A34)", () => {
    render(<Header agents={0} nodes={[peer("zuan", { last_seen: SEEN, retrying: true, last_error: "fingerprint_mismatch" })]} />);
    const c = chip("zuan");
    expect(c.textContent).toBe(`zuan✗指纹不匹配 · 上次见到 ${clockText(SEEN)}`);
    expect(c.className).toContain("bad");
    expect(c.title).toBe(`指纹不匹配，上次见到 ${SEEN}，重试中`);
    expect(c.textContent).not.toContain("离线");
  });

  it("incompatible_version: its own reason, not '离线', even when never seen (A33)", () => {
    render(<Header agents={0} nodes={[peer("zuan", { retrying: true, last_error: "incompatible_version" })]} />);
    const c = chip("zuan");
    expect(c.textContent).toBe("zuan✗版本不兼容");
    expect(c.className).toContain("bad");
    expect(c.title).toBe("版本不兼容，重试中");
  });

  it("every error type has a label and only those four exist (rule 10)", () => {
    expect(peerErrorLabel("unauthorized")).toBe("未授权");
    expect(peerErrorLabel("unreachable")).toBe("不可达");
    render(<Header agents={0} nodes={[peer("a", { last_error: "unauthorized" }), peer("b")]} />);
    expect(chip("a").textContent).toBe("a✗未授权");
    // 配置了、还没连上、也没失败过：未连接，不是错误。
    expect(chip("b").textContent).toBe("b○未连接");
    expect(chip("b").className).toContain("unknown");
  });

  it("nodeStatuses puts 本机 first and reports it unreachable when the last poll failed", () => {
    const peers = { zuan: { online: true, last_seen: SEEN, retrying: false, last_error: null } };
    expect(nodeStatuses({ reachable: null, peers: {} }).map((n) => n.name)).toEqual(["本机"]);
    const ok = nodeStatuses({ reachable: true, peers });
    expect(ok.map((n) => n.name)).toEqual(["本机", "zuan"]);
    expect(ok[0]).toMatchObject({ local: true, online: true, last_error: null });
    const down = nodeStatuses({ reachable: false, peers });
    expect(down[0]).toMatchObject({ local: true, online: false, last_error: "unreachable" });
    // peer 的最后一眼不因本机拉不到而消失（不变量 8）。
    expect(down[1]).toMatchObject({ name: "zuan", online: true });

    render(<Header agents={0} nodes={nodeStatuses({ reachable: null, peers: {} })} />);
    expect(chip("本机").textContent).toBe("本机…探测中");
    cleanup();
    render(<Header agents={0} nodes={down} />);
    expect(chip("本机").textContent).toBe("本机✗不可达");
  });

  it("names the local chip after /api/system's node once known, 本机 until then (agora-7ku.5)", () => {
    // MISSION §3.5 线框 `mac ● zuan ●`：两枚都是节点名，与侧栏行上的 `@ zuan` 对得上。
    const peers = { zuan: { online: true, last_seen: SEEN, retrying: false, last_error: null } };
    expect(nodeStatuses({ reachable: true, peers }, "mac").map((n) => n.name)).toEqual(["mac", "zuan"]);
    expect(nodeStatuses({ reachable: true, peers }, null).map((n) => n.name)).toEqual(["本机", "zuan"]);
    render(<Header agents={0} nodes={nodeStatuses({ reachable: true, peers }, "mac")} />);
    expect(chip("mac").textContent).toBe("mac●");
    expect(chip("mac").className).toContain("ok");
    expect(screen.queryByTestId("node-本机")).toBeNull();
  });

  it("renders no node row at all when nodes are not given (the pre-7ku.12 shape)", () => {
    render(<Header agents="2/5" />);
    expect(screen.queryByTestId("nodes")).toBeNull();
    expect(screen.getByText("AGENTS 2/5")).toBeTruthy();
  });
});
