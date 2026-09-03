import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentInfo, CatalogApi, ProjectInfo, SessionApi } from "./api";
import type { SessionRow } from "./events";
import { fuzzyFilter } from "./fuzzy";
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
  | { kind: "create"; label: string; agent: AgentInfo; project: ProjectInfo }
  | { kind: "action"; label: string; run: () => void };

interface Props {
  rows: SessionRow[];
  api: SessionApi;
  catalog: CatalogApi;
  onOpen: (id: string) => void;
  onNewAgent: () => void;
  onCreated: (id: string) => void;
  onClose: () => void;
}

export function CommandPalette({ rows, api, catalog, onOpen, onNewAgent, onCreated, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [node, setNode] = useState("");
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

  const entries = useMemo<Entry[]>(() => {
    const list: Entry[] = rows.map((r) => ({
      kind: "session",
      id: r.id,
      label: `${rowName(r)} / ${String(r.agent_type ?? "")} @ ${r.node}`,
    }));
    // node 只在条目文字里出现（V1 只有本机，peer 归 M2/ADR-004），所以搜节点名也搜得到。
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
    list.push({ kind: "action", label: "New Agent…（完整对话框）", run: onNewAgent });
    return list;
  }, [rows, projects, agents, node, onNewAgent]);

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
