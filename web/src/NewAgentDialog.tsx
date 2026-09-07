import { useEffect, useRef, useState } from "react";
import type {
  AgentInfo,
  CatalogApi,
  ProjectInfo,
  ReadyTask,
  SessionApi,
  WorktreeInfo,
} from "./api";
import { taskPrompt } from "./taskPrompt";
import { WorktreeSelect } from "./WorktreeSelect";

/** 前端唯一写死的"agent"：它没有 Adapter，命令必须由用户填（§6.4 线框的 custom）。 */
const CUSTOM = "custom";

/** Task 下拉里「一句话…」那一项的 value；issue id 永远不会是空串。 */
const FREE_TEXT = "";

interface Props {
  api: SessionApi;
  catalog: CatalogApi;
  onClose: () => void;
  /** 创建成功：把新会话打开成 Tab。 */
  onCreated: (id: string) => void;
}

/**
 * `GET /api/projects/tasks` 没给出列表时的一行灰字：按 `reason` 的**类型**给文案
 * （docs/spec/api.md「从就绪任务起会话」），不解析任何消息文本（MISSION §2.3 规则 10）。
 * 不认识的值不提示——那不是本页面知道的事。
 */
export function describeTasksReason(reason: string): string | null {
  switch (reason) {
    case "no_bd":
      return "没装 bd";
    case "no_beads":
      return "该仓库没有 beads";
    case "timeout":
      return "bd 没有在 10 s 内应答";
    case "bad_output":
      return "bd 的输出读不懂";
    default:
      return null;
  }
}

/**
 * Screen C：New Agent（MISSION §6.4，线框 docs/spec/ux.md）。
 *
 * Project 不靠手写配置——列表来自 `project_roots` 扫描、按最近使用排序；但输入框仍可
 * 直接打路径（`project_roots` 默认是空的，只给下拉的话新装的 agora 一个会话都起不了）。
 * Agent 与默认命令来自 `/api/agents`，前端不认识任何具体 agent（ADR-002 D2）。
 *
 * Task（A43，agora-h1k.2）：有 bd 的仓库列 `bd ready` 的就绪任务可选，否则一句话。选中任务 →
 * task_ref = issue id、Name = 标题、Worktree「新建…」的默认名 = issue id、Prompt 预填模板
 * （`taskPrompt`）——都是默认值，用户手改过的不覆盖。claim 由 agent 自己做（模板里写着），
 * 页面对 beads 什么都不写。
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
  const [prompt, setPrompt] = useState("");
  // 用户手改过 Name / Command / Prompt 之后，换项目 / 换 agent / 换任务就不再覆盖它——
  // 默认值是便利，不是主人。
  const nameEdited = useRef(false);
  const commandEdited = useRef(false);
  const promptEdited = useRef(false);

  // `bd ready` 的就绪任务与"为什么没有"（按类型）；taskPick 是选中的 issue id，"" = 一句话。
  const [tasks, setTasks] = useState<ReadyTask[]>([]);
  const [tasksReason, setTasksReason] = useState<string | null>(null);
  const [taskPick, setTaskPick] = useState(FREE_TEXT);

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

  // 「新建…」建成之后重拉一次并选中新项（A44）：bump 这个计数让下面的 effect 再跑，
  // 想选中的路径先寄在 ref 里，等新列表到了再选——直接 setWorktree 会被 effect 开头的清空冲掉。
  const [worktreeGen, setWorktreeGen] = useState(0);
  const pendingWorktree = useRef<WorktreeInfo | null>(null);
  const [worktreeCreating, setWorktreeCreating] = useState(false);

  // 项目变了就重新列 worktree；不是 git 仓库（或手打的路径）就没有 worktree 可选。
  useEffect(() => {
    let cancelled = false;
    setWorktrees([]);
    setWorktree("");
    if (!project.trim()) return;
    void catalog.worktrees(project.trim()).then((r) => {
      if (cancelled || !r.ok) return;
      const created = pendingWorktree.current;
      pendingWorktree.current = null;
      if (!created) {
        setWorktrees(r.value.worktrees);
        return;
      }
      // 刚建好的那项应该已经在 git 的登记里；万一列表还没它（比如另一路径形态），补上再选。
      const list = r.value.worktrees.some((w) => w.path === created.path)
        ? r.value.worktrees
        : [...r.value.worktrees, created];
      setWorktrees(list);
      setWorktree(created.path);
    });
    return () => {
      cancelled = true;
    };
  }, [catalog, project, worktreeGen]);

  // 与 worktrees 并行拉该仓库的就绪任务（A43）。与上面分开的 effect：新建 worktree 之后不必再
  // 敲一次 bd（embedded dolt 冷启动要几秒）。换项目就清掉上一个仓库的任务与选择。
  useEffect(() => {
    let cancelled = false;
    setTasks([]);
    setTasksReason(null);
    setTaskPick(FREE_TEXT);
    if (!promptEdited.current) setPrompt("");
    if (!project.trim()) return;
    void catalog.tasks(project.trim()).then((r) => {
      if (cancelled) return;
      // 400（不是已知项目）之类：没有列表也没有可说的原因——Task 就是一句话，不提示。
      if (!r.ok) return;
      setTasks(r.value.tasks ?? []);
      setTasksReason(r.value.reason ?? null);
    });
    return () => {
      cancelled = true;
    };
  }, [catalog, project]);

  function worktreeCreated(created: WorktreeInfo) {
    pendingWorktree.current = created;
    setWorktreeGen((g) => g + 1);
  }

  function projectName(path: string): string {
    const known = projects.find((p) => p.path === path);
    return known?.name ?? basename(path);
  }

  function pickProject(path: string) {
    setProject(path);
    if (!nameEdited.current) setName(projectName(path));
  }

  function pickAgent(next: string) {
    setAgent(next);
    if (!commandEdited.current) {
      setCommand(agents.find((a) => a.name === next)?.command ?? "");
    }
  }

  function pickTask(id: string) {
    setTaskPick(id);
    const picked = tasks.find((t) => t.id === id);
    if (picked) {
      if (!nameEdited.current) setName(picked.title);
      if (!promptEdited.current) setPrompt(taskPrompt(picked.id, picked.title));
      return;
    }
    // 回到「一句话…」：默认值跟着撤——Name 回项目名、Prompt 清空；用户手改过的不动。
    if (!nameEdited.current) setName(projectName(project));
    if (!promptEdited.current) setPrompt("");
  }

  const selected = worktrees.find((w) => w.path === worktree);
  // 主 worktree = 仓库本身：working_directory 就是它，worktree 字段留空。
  const cwd = selected && !selected.main ? selected.path : project.trim();
  const needsCommand = agent === CUSTOM && command.trim() === "";
  // 只有 Adapter 说接受首条 prompt 的 agent 才有 Prompt 框（`/api/agents` 的 prompt 标志）；
  // custom 没有 Adapter，也就没有。
  const agentAcceptsPrompt = agents.find((a) => a.name === agent)?.prompt === true;
  const tasksHint = tasksReason === null ? null : describeTasksReason(tasksReason);
  // worktree 名字框开着时先别起会话：cwd 会落回仓库本身，而用户明明想在新 worktree 里干。
  const canCreate =
    !busy && !worktreeCreating && project.trim() !== "" && name.trim() !== "" && agent !== "" && !needsCommand;

  async function create() {
    setBusy(true);
    setError(null);
    const r = await api.create({
      display_name: name.trim(),
      agent_type: agent,
      working_directory: cwd,
      worktree: selected && !selected.main ? (selected.branch ?? selected.path) : null,
      task_ref: taskPick || task.trim() || null,
      command: command.trim() || undefined,
      // 非空才发，且只对接受它的 agent 发——节点对不接受的类型回 400，别把用户挡在这一步。
      prompt: agentAcceptsPrompt && prompt.trim() !== "" ? prompt : undefined,
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
          {/* 目前只能在本机起会话；选 peer 经一跳转发（MISSION §6.4、A45）归 agora-fna。 */}
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
          <WorktreeSelect
            worktrees={worktrees}
            value={worktree}
            onChange={setWorktree}
            disabled={busy}
            project={project.trim()}
            // MISSION §6.4：worktree 名默认 issue id，无 bd（没选任务）用 Name。
            defaultName={taskPick || name}
            create={catalog.createWorktree}
            onCreated={worktreeCreated}
            onCreatingChange={setWorktreeCreating}
          />

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
          <div className="task-field">
            {tasks.length > 0 ? (
              <>
                {/* 有 bd 的仓库：从 `bd ready` 选；第一项回到一句话。 */}
                <select id="na-task" value={taskPick} onChange={(e) => pickTask(e.target.value)} disabled={busy}>
                  <option value={FREE_TEXT}>一句话…</option>
                  {tasks.map((t) => (
                    <option key={t.id} value={t.id}>
                      {`${t.id} · ${t.title}`}
                    </option>
                  ))}
                </select>
                {taskPick === FREE_TEXT && (
                  <input
                    id="na-task-text"
                    value={task}
                    placeholder="一句话（可留空）"
                    onChange={(e) => setTask(e.target.value)}
                    disabled={busy}
                  />
                )}
              </>
            ) : (
              <>
                <input
                  id="na-task"
                  value={task}
                  placeholder="一句话（可留空）"
                  onChange={(e) => setTask(e.target.value)}
                  disabled={busy}
                />
                {tasksHint && (
                  <span className="muted task-hint" data-testid="tasks-hint">
                    {tasksHint}
                  </span>
                )}
              </>
            )}
          </div>

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

          {agentAcceptsPrompt && (
            <>
              <label htmlFor="na-prompt">Prompt</label>
              <textarea
                id="na-prompt"
                rows={4}
                value={prompt}
                placeholder="首条指令（可留空）"
                onChange={(e) => {
                  promptEdited.current = true;
                  setPrompt(e.target.value);
                }}
                disabled={busy}
              />
            </>
          )}

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
