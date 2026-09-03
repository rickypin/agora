import { useEffect, useRef, useState } from "react";
import type { AgentInfo, CatalogApi, ProjectInfo, SessionApi, WorktreeInfo } from "./api";

/** 前端唯一写死的"agent"：它没有 Adapter，命令必须由用户填（§6.4 线框的 custom）。 */
const CUSTOM = "custom";

interface Props {
  api: SessionApi;
  catalog: CatalogApi;
  onClose: () => void;
  /** 创建成功：把新会话打开成 Tab。 */
  onCreated: (id: string) => void;
}

/**
 * Screen C：New Agent（MISSION §6.4，线框 docs/spec/ux.md）。
 *
 * Project 不靠手写配置——列表来自 `project_roots` 扫描、按最近使用排序；但输入框仍可
 * 直接打路径（`project_roots` 默认是空的，只给下拉的话新装的 agora 一个会话都起不了）。
 * Agent 与默认命令来自 `/api/agents`，前端不认识任何具体 agent（ADR-002 D2）。
 */
export function NewAgentDialog({ api, catalog, onClose, onCreated }: Props) {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [worktrees, setWorktrees] = useState<WorktreeInfo[]>([]);
  const [node, setNode] = useState<string>("");

  const [project, setProject] = useState("");
  const [worktree, setWorktree] = useState("");
  const [agent, setAgent] = useState("");
  const [task, setTask] = useState("");
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  // 用户手改过 Name / Command 之后，换项目 / 换 agent 就不再覆盖它——默认值是便利，
  // 不是主人。
  const nameEdited = useRef(false);
  const commandEdited = useRef(false);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [p, a, s] = await Promise.all([catalog.projects(), catalog.agents(), catalog.system()]);
      if (cancelled) return;
      if (p.ok) {
        setProjects(p.value.projects);
        // 最近用过的项目排在最前，所以默认选它——常用项目 2–3 次操作起会话（§6.4）。
        const first = p.value.projects[0];
        if (first) {
          setProject(first.path);
          if (!nameEdited.current) setName(first.name);
        }
      }
      if (a.ok) {
        setAgents(a.value.agents);
        const first = a.value.agents[0];
        if (first) {
          setAgent(first.name);
          if (!commandEdited.current) setCommand(first.command);
        }
      }
      if (s.ok) setNode(s.value.node);
    })();
    return () => {
      cancelled = true;
    };
  }, [catalog]);

  // 项目变了就重新列 worktree；不是 git 仓库（或手打的路径）就没有 worktree 可选。
  useEffect(() => {
    let cancelled = false;
    setWorktrees([]);
    setWorktree("");
    if (!project.trim()) return;
    void catalog.worktrees(project.trim()).then((r) => {
      if (cancelled || !r.ok) return;
      setWorktrees(r.value.worktrees);
    });
    return () => {
      cancelled = true;
    };
  }, [catalog, project]);

  function pickProject(path: string) {
    setProject(path);
    if (!nameEdited.current) {
      const known = projects.find((p) => p.path === path);
      setName(known?.name ?? basename(path));
    }
  }

  function pickAgent(next: string) {
    setAgent(next);
    if (!commandEdited.current) {
      setCommand(agents.find((a) => a.name === next)?.command ?? "");
    }
  }

  const selected = worktrees.find((w) => w.path === worktree);
  // 主 worktree = 仓库本身：working_directory 就是它，worktree 字段留空。
  const cwd = selected && !selected.main ? selected.path : project.trim();
  const needsCommand = agent === CUSTOM && command.trim() === "";
  const canCreate = !busy && project.trim() !== "" && name.trim() !== "" && agent !== "" && !needsCommand;

  async function create() {
    setBusy(true);
    setError(null);
    const r = await api.create({
      display_name: name.trim(),
      agent_type: agent,
      working_directory: cwd,
      worktree: selected && !selected.main ? (selected.branch ?? selected.path) : null,
      task_ref: task.trim() || null,
      command: command.trim() || undefined,
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
        className="dialog wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="new-agent-title"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h2 id="new-agent-title">New Agent</h2>
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault();
            if (canCreate) void create();
          }}
        >
          <label htmlFor="na-node">Node</label>
          {/* peer 归 M2（ADR-004）：V1 只能在本机起会话。 */}
          <select id="na-node" disabled>
            <option>{node || "本机"}</option>
          </select>

          <label htmlFor="na-project">Project</label>
          <input
            id="na-project"
            list="na-projects"
            value={project}
            autoFocus
            placeholder="~/code/agora"
            onChange={(e) => pickProject(e.target.value)}
            disabled={busy}
          />
          <datalist id="na-projects">
            {projects.map((p) => (
              <option key={p.path} value={p.path}>
                {p.name}
              </option>
            ))}
          </datalist>

          <label htmlFor="na-worktree">Worktree</label>
          <select
            id="na-worktree"
            value={worktree}
            onChange={(e) => setWorktree(e.target.value)}
            disabled={busy || worktrees.length === 0}
          >
            {worktrees.length === 0 && <option value="">—</option>}
            {worktrees.map((w) => (
              <option key={w.path} value={w.path}>
                {(w.branch ?? w.path) + (w.main ? "" : " ↗")}
              </option>
            ))}
          </select>

          <label htmlFor="na-agent">Agent</label>
          <select id="na-agent" value={agent} onChange={(e) => pickAgent(e.target.value)} disabled={busy}>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.name}
              </option>
            ))}
            <option value={CUSTOM}>{CUSTOM}</option>
          </select>

          <label htmlFor="na-task">Task</label>
          {/* 有 bd 的仓库从 issue 列表选归 M3 A43；V1 只有一句话。 */}
          <input
            id="na-task"
            value={task}
            placeholder="一句话（可留空）"
            onChange={(e) => setTask(e.target.value)}
            disabled={busy}
          />

          <label htmlFor="na-name">Name</label>
          <input
            id="na-name"
            value={name}
            onChange={(e) => {
              nameEdited.current = true;
              setName(e.target.value);
            }}
            disabled={busy}
          />

          <label htmlFor="na-command">Command</label>
          <input
            id="na-command"
            value={command}
            placeholder={agent === CUSTOM ? "自己填（必填）" : ""}
            onChange={(e) => {
              commandEdited.current = true;
              setCommand(e.target.value);
            }}
            disabled={busy}
          />

          <div className="dialog-actions span">
            <button type="button" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button type="submit" disabled={!canCreate} data-testid="create">
              Create
            </button>
          </div>
        </form>
        {error && <p className="error">{error}</p>}
      </div>
    </div>
  );
}

function basename(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}
