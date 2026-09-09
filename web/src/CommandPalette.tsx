import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentInfo, CatalogApi, ProjectInfo, SessionApi } from "./api";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
import type { NodeStatus } from "./Header";
import { rowName } from "./Sidebar";

/**
 * Command Palette（MISSION §6.5；键位表 docs/spec/ux.md）：fuzzy 搜 sessions / projects /
 * nodes / actions，`New Claude in agora @ mac` 这样的条目直接起会话。
 *
 * 起会话走的是和 New Agent 对话框同一个 `POST /api/sessions`，字段取默认值（项目名当
 * display_name、agent 的默认命令）——面板的意义就是不填表；要填 Task / Worktree 的走对话框。
 */

export type Entry =
  | { kind: "session"; id: string; label: string }
  /** `node` 只在 peer 上起时有（A45）：body 带它、节点一跳转发；本机不带。 */
  | { kind: "create"; label: string; agent: AgentInfo; project: ProjectInfo; node?: string }
  | { kind: "action"; label: string; run: () => void };

/** 一个在线 peer 的项目与 agent（各从那台机器取，`?node=`）。 */
interface RemoteCatalog {
  projects: ProjectInfo[];
  agents: AgentInfo[];
}

interface Props {
  rows: SessionRow[];
  api: SessionApi;
  catalog: CatalogApi;
  /** Header 同一份节点状态（A45）：在线的 peer 各出一组 `New <agent> in <project> @ <peer>`。 */
  nodes?: NodeStatus[];
  onOpen: (id: string) => void;
  onNewAgent: () => void;
  /** 切换侧栏视图 attention ↔ tree（A47，agora-uvd.2）。 */
  onToggleMode?: () => void;
  onCreated: (id: string) => void;
  onClose: () => void;
}

export function CommandPalette({ rows, api, catalog, nodes, onOpen, onNewAgent, onToggleMode, onCreated, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [node, setNode] = useState("");
  const [remote, setRemote] = useState<Record<string, RemoteCatalog>>({});
  // 只有在线的 peer 才去拉（离线的拉也是 502）；按名字串做依赖，peer 只是 last_seen 变了不重拉。
  const onlinePeers = (nodes ?? []).filter((n) => !n.local && n.online).map((n) => n.name);
  const peerKey = onlinePeers.join("\u0000");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [p, a, s] = await Promise.all([catalog.projects(), catalog.agents(), catalog.system()]);
      if (cancelled) return;
      if (p.ok) setProjects(p.value.projects);
      if (a.ok) setAgents(a.value.agents);
      if (s.ok) setNode(s.value.node);
    })();
    return () => {
      cancelled = true;
    };
  }, [catalog]);

  // 每个在线 peer 的项目与 agent（A45）：面板开着时 peer 上线 / 掉线，条目跟着出现 / 消失。
  useEffect(() => {
    let cancelled = false;
    const names = peerKey === "" ? [] : peerKey.split("\u0000");
    void (async () => {
      const got = await Promise.all(
        names.map(async (name) => {
          const [p, a] = await Promise.all([catalog.projects(name), catalog.agents(name)]);
          return [name, { projects: p.ok ? p.value.projects : [], agents: a.ok ? a.value.agents : [] }] as const;
        }),
      );
      if (cancelled) return;
      setRemote(Object.fromEntries(got));
    })();
    return () => {
      cancelled = true;
    };
  }, [catalog, peerKey]);

  const entries = useMemo<Entry[]>(() => {
    const list: Entry[] = rows.map((r) => ({
      kind: "session",
      id: r.id,
      label: `${rowName(r)} / ${String(r.agent_type ?? "")} @ ${r.node}`,
    }));
    // node 在条目文字里，所以搜节点名也搜得到；本机一组，每个在线 peer 各一组（A45）。
    for (const project of projects) {
      for (const agent of agents) {
        list.push({
          kind: "create",
          label: `New ${agent.name} in ${project.name} @ ${node || "本机"}`,
          agent,
          project,
        });
      }
    }
    for (const [peer, cat] of Object.entries(remote)) {
      for (const project of cat.projects) {
        for (const agent of cat.agents) {
          list.push({
            kind: "create",
            label: `New ${agent.name} in ${project.name} @ ${peer}`,
            agent,
            project,
            node: peer,
          });
        }
      }
    }
    if (onToggleMode) {
      list.push({ kind: "action", label: "切换侧栏视图（需要我 / 按项目）", run: onToggleMode });
    }
    list.push({ kind: "action", label: "New Agent…（完整对话框）", run: onNewAgent });
    return list;
  }, [rows, projects, agents, node, remote, onNewAgent, onToggleMode]);

  const hits = useMemo(() => fuzzyFilter(entries, query, (e) => e.label), [entries, query]);
  const shown = hits.slice(0, 20);
  useEffect(() => setCursor(0), [query]);

  useEffect(() => {
    input.current?.focus();
  }, []);

  async function run(entry: Entry | undefined) {
    if (!entry || busy) return;
    if (entry.kind === "session") {
      onOpen(entry.id);
      onClose();
      return;
    }
    if (entry.kind === "action") {
      entry.run();
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    const r = await api.create({
      ...(entry.node ? { node: entry.node } : {}),
      display_name: entry.project.name,
      agent_type: entry.agent.name,
      working_directory: entry.project.path,
      worktree: null,
      task_ref: null,
      command: entry.agent.command || undefined,
    });
    setBusy(false);
    if (r.ok) {
      onCreated(r.value.id);
      onClose();
      return;
    }
    setError(r.needsConfirmation ? "需要确认" : `${r.error.error}: ${r.error.message}`);
  }

  return (
    <div className="overlay" role="presentation" onClick={onClose}>
      <div
        className="dialog palette"
        role="dialog"
        aria-modal="true"
        aria-label="Command Palette"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            onClose();
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            setCursor((c) => Math.min(c + 1, shown.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setCursor((c) => Math.max(c - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            void run(shown[cursor]);
          }
        }}
      >
        <input
          ref={input}
          className="palette-input"
          value={query}
          placeholder="搜会话，或 New <agent> in <project>"
          aria-label="命令面板"
          data-testid="palette-input"
          onChange={(e) => setQuery(e.target.value)}
          disabled={busy}
        />
        {shown.length === 0 && <p className="muted pad">没有匹配。</p>}
        <ul className="palette-list" role="listbox">
          {shown.map((e, i) => (
            <li key={`${e.kind}:${e.label}`} role="option" aria-selected={i === cursor}>
              <button
                className={i === cursor ? "palette-hit on" : "palette-hit"}
                onMouseEnter={() => setCursor(i)}
                onClick={() => void run(e)}
                data-testid={`palette-${i}`}
              >
                {e.label}
              </button>
            </li>
          ))}
        </ul>
        {error && <p className="error">{error}</p>}
      </div>
    </div>
  );
}
