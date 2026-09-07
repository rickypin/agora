// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { catalogApi, sessionApi, type FetchLike } from "./api";
import { CommandPalette } from "./CommandPalette";
import type { NodeStatus } from "./Header";

afterEach(cleanup);

const peer = (name: string, extra: Partial<NodeStatus> = {}): NodeStatus => ({
  name,
  online: true,
  last_seen: null,
  retrying: false,
  last_error: null,
  ...extra,
});

function setup(nodes?: NodeStatus[]) {
  const requests: { url: string; method: string; body?: string }[] = [];
  const f: FetchLike = async (url, init) => {
    requests.push({ url, method: init.method ?? "GET", body: init.body as string | undefined });
    const onZuan = url.includes("node=zuan");
    const body = url.startsWith("/api/projects")
      ? { projects: onZuan ? [{ path: "/home/z/beta", name: "beta", last_used_at: null }] : [{ path: "/u/agora", name: "agora", last_used_at: null }] }
      : url.startsWith("/api/agents")
        ? { agents: onZuan ? [{ name: "zb", command: "zb-bin", prompt: false }] : [{ name: "a1", command: "a1", prompt: true }] }
        : url.startsWith("/api/system")
          ? { node: "mac" }
          : { id: (init.body as string | undefined)?.includes('"node":"zuan"') ? "zuan:new7" : "mac:new1" };
    return new Response(JSON.stringify(body), {
      status: init.method === "POST" ? 201 : 200,
      headers: { "content-type": "application/json" },
    });
  };
  const onCreated = vi.fn();
  render(
    <CommandPalette
      rows={[]}
      api={sessionApi(f)}
      catalog={catalogApi(f)}
      nodes={nodes}
      onOpen={vi.fn()}
      onNewAgent={vi.fn()}
      onCreated={onCreated}
      onClose={vi.fn()}
    />,
  );
  return { requests, onCreated };
}

const labels = () => screen.getAllByRole("option").map((o) => o.textContent);

it("offers a New <agent> in <project> @ <peer> entry per online peer and creates there", async () => {
  // MISSION §6.5 / A45：面板搜节点名也搜得到；在线的 peer 各一组条目，离线的没有（拉也是 502）；
  // 选 peer 的条目 → body 带 node，节点一跳转发，响应的 id 带 zuan 前缀。
  const { requests, onCreated } = setup([
    peer("mac", { local: true }),
    peer("zuan"),
    peer("old", { online: false, last_seen: "2026-09-07T01:00:00Z", retrying: true, last_error: "unreachable" }),
  ]);
  await waitFor(() => expect(labels()).toContain("New zb in beta @ zuan"));
  expect(labels()).toContain("New a1 in agora @ mac");
  expect(labels().some((l) => l?.includes("@ old"))).toBe(false);
  // 离线的 peer 一个请求都不发。
  expect(requests.some((r) => r.url.includes("node=old"))).toBe(false);

  fireEvent.change(screen.getByTestId("palette-input"), { target: { value: "zuan" } });
  await waitFor(() => expect(labels()[0]).toBe("New zb in beta @ zuan"));
  fireEvent.click(screen.getByTestId("palette-0"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("zuan:new7"));
  const body = JSON.parse(requests.find((r) => r.method === "POST")!.body!);
  expect(body).toMatchObject({ node: "zuan", display_name: "beta", agent_type: "zb", working_directory: "/home/z/beta", command: "zb-bin" });
});

it("without nodes there is only the local group and the body carries no node", async () => {
  const { requests, onCreated } = setup();
  await waitFor(() => expect(labels()).toContain("New a1 in agora @ mac"));
  expect(labels().filter((l) => l?.startsWith("New ") && !l.includes("完整对话框"))).toHaveLength(1);
  fireEvent.click(screen.getByTestId("palette-0"));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith("mac:new1"));
  expect(JSON.parse(requests.find((r) => r.method === "POST")!.body!)).not.toHaveProperty("node");
});
