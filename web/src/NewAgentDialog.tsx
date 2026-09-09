import { useEffect, useRef, useState } from "react";
import type {
  AgentInfo,
  CatalogApi,
  ProjectInfo,
  ReadyTask,
  SessionApi,
  WorktreeInfo,
} from "./api";
import type { NodeStatus } from "./Header";
import { taskPrompt } from "./taskPrompt";
import { WorktreeSelect } from "./WorktreeSelect";

/** 前端唯一写死的"agent"：它没有 Adapter，命令必须由用户填（§6.4 线框的 custom）。 */
const CUSTOM = "custom";

/** Task 下拉里「一句话…」那一项的 value；issue id 永远不会是空串。 */
const FREE_TEXT = "";

/**
 * 打开对话框时的预填（A48，agora-uvd.4）：树视图的 worktree 组头「+」已经站在某个 worktree 上，
 * 三个下拉不该再让人选一遍（MISSION §6.4「常用项目最多 2–3 次操作」）。
 * 只是**初值**：三项都只在挂载后第一次拿到对应列表时生效一次，用户改过就再也不覆盖。
 */
export interface NewAgentInitial {
  /** 在哪个节点起；null / 不给 = 本机（与 create body 的 `node` 同一口径）。 */
  node?: string | null;
  /** 仓库路径；不在 `/api/projects` 的列表里就当没给（仍选最近用过的那个）。 */
  project?: string;
  /** worktree 路径；不在该仓库的 worktree 列表里就当没给（保持默认）。 */
  worktree?: string | null;
}

interface Props {
  api: SessionApi;
  catalog: CatalogApi;
  /** 打开时的预填（agora-uvd.4）：不给 = 今天的默认（最近用过的项目 + 第一个 agent）。 */
  initial?: NewAgentInitial;
  /**
   * Node 下拉的数据源（A45，agora-fna）：Header 同一份 `nodeStatuses`——本机 + 每个已配置的 peer 及其
   * 在线 / 离线 / 错误类型。没给（单机、旧测试）就只有本机。
   */
  nodes?: NodeStatus[];
  onClose: () => void;
  /** 创建成功：把新会话打开成 Tab。 */
  onCreated: (id: string) => void;
}

/**
 * Node 下拉里一个 peer 的文字：在线只有名字；离线按**类型**加一段（MISSION §2.3 规则 10，不解析消息）——
 * 版本不兼容是自己的原因（A33：不读它的数据），见过的离线是 stale（MISSION §3.5 的措辞），
 * 没见过的是还没连上。离线的一律不可选：选了也只会得到 502，不如在下拉里就说清楚（A45）。
 */
export function nodeOptionLabel(n: NodeStatus): string {
  if (n.online) return n.name;
  if (n.last_error === "incompatible_version") return `${n.name} · 版本不兼容`;
  return n.last_seen ? `${n.name} · stale` : `${n.name} · 未连接`;
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
 *
 * Node（A45，agora-fna）：本机 + 已配置的 peer，在线的才可选。选了 peer，Project / Worktree / Task /
 * Agent 四个下拉全部改从那台机器取（五个 catalog 端点带 `node=`，节点经一跳转发），Create 与
 * 「新建 worktree」也带 `node` 在那边执行；响应的会话 id 带它的前缀，新行随 peer 视图进侧栏。
 *
 * 预填（A48，agora-uvd.4）：`initial` 的 Node / Project / Worktree 各在自己的列表到达时选中一次——
 * 树视图的 worktree 组头「+」已经站在某个 worktree 上，再让人选三遍下拉就不是「2–3 次操作」了。
 * 不是受控 prop：三项都只生效一次，用户改选之后 initial 再也不出现（换节点、换项目都不回头）。
 */
export function NewAgentDialog({ api, catalog, initial, nodes, onClose, onCreated }: Props) {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [worktrees, setWorktrees] = useState<WorktreeInfo[]>([]);
  /** 本机 node.id（`/api/system`）：Node 下拉第一项的名字，也是"选了本机"的 value。 */
  const [localName, setLocalName] = useState<string>("");
  /** 选中的 peer 名；null = 本机。catalog 请求与 create body 的 `node` 都从这里来。 */
  const [node, setNode] = useState<string | null>(initial?.node ?? null);
  const target = node ?? undefined;
  const peers = (nodes ?? []).filter((n) => !n.local);
  const selectedPeer = node === null ? null : peers.find((n) => n.name === node);
  // 对话框开着时 peer 掉线：下拉里它变灰，Create 也别按——按了只会得到 502。
  const nodeOnline = node === null || selectedPeer?.online === true;

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

  // 预填的项目（agora-uvd.4）：只等**第一份**项目列表，用掉就作废——不然用户换个节点、换个项目，
  // 下一份列表到达时又被 initial 拉回去（"预填只在挂载时生效一次"）。
  const pendingProject = useRef<string | null>(initial?.project ?? null);

  // 打开时、以及每次换节点：项目与 agent 列表从所选节点拉（`target`），默认值跟着换。
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [p, a, s] = await Promise.all([catalog.projects(target), catalog.agents(target), catalog.system()]);
      if (cancelled) return;
      if (p.ok) {
        setProjects(p.value.projects);
        // 最近用过的项目排在最前，所以默认选它——常用项目 2–3 次操作起会话（§6.4）；
        // 预填给的那个在列表里就选它（树视图组头「+」，A48），不在就还是最近用过的。
        const wanted = pendingProject.current;
        pendingProject.current = null;
        const first = (wanted === null ? undefined : p.value.projects.find((x) => x.path === wanted)) ?? p.value.projects[0];
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
      if (s.ok) setLocalName(s.value.node);
    })();
    return () => {
      cancelled = true;
    };
  }, [catalog, target]);

  /** 换节点：上一台机器的项目 / agent / worktree 一个都不能留——路径在那边，这边没有。 */
  function pickNode(value: string) {
    const next = value === localName ? null : value;
    if (next === node) return;
    setNode(next);
    setProjects([]);
    setAgents([]);
    setProject("");
    setWorktree("");
    setAgent("");
  }

  // 「新建…」建成之后重拉一次并选中新项（A44）：bump 这个计数让下面的 effect 再跑，
  // 想选中的路径先寄在 ref 里，等新列表到了再选——直接 setWorktree 会被 effect 开头的清空冲掉。
  // 同一个 ref 也担着预填（agora-uvd.4）：`created` 是刚建好的那项（列表没它就补上再选），
  // 只有 `path` 的是组头带来的预填（命中才选，没命中保持默认——那多半是别的仓库的路径）。
  const [worktreeGen, setWorktreeGen] = useState(0);
  const pendingWorktree = useRef<{ path: string; created: WorktreeInfo | null } | null>(
    initial?.worktree ? { path: initial.worktree, created: null } : null,
  );
  const [worktreeCreating, setWorktreeCreating] = useState(false);

  // 项目变了就重新列 worktree；不是 git 仓库（或手打的路径）就没有 worktree 可选。
  useEffect(() => {
    let cancelled = false;
    setWorktrees([]);
    setWorktree("");
    if (!project.trim()) return;
    void catalog.worktrees(project.trim(), target).then((r) => {
      if (cancelled || !r.ok) return;
      const pending = pendingWorktree.current;
      pendingWorktree.current = null;
      if (!pending) {
        setWorktrees(r.value.worktrees);
        return;
      }
      const known = r.value.worktrees.some((w) => w.path === pending.path);
      // 预填的路径列表里没有：不补、不选，保持今天的默认（主 worktree）——补上去只会给一个
      // 这个仓库里并不存在的选项。刚建好的那项则相反：它应该已经在 git 的登记里，万一列表还没它
      // （比如另一路径形态）也补上再选，否则用户刚建的 worktree 选不着（A44）。
      if (!known && !pending.created) {
        setWorktrees(r.value.worktrees);
        return;
      }
      setWorktrees(known ? r.value.worktrees : [...r.value.worktrees, pending.created!]);
      setWorktree(pending.path);
    });
    return () => {
      cancelled = true;
    };
  }, [catalog, project, target, worktreeGen]);

  // 与 worktrees 并行拉该仓库的就绪任务（A43）。与上面分开的 effect：新建 worktree 之后不必再
  // 敲一次 bd（embedded dolt 冷启动要几秒）。换项目就清掉上一个仓库的任务与选择。
  useEffect(() => {
    let cancelled = false;
    setTasks([]);
    setTasksReason(null);
    setTaskPick(FREE_TEXT);
    if (!promptEdited.current) setPrompt("");
    if (!project.trim()) return;
    void catalog.tasks(project.trim(), target).then((r) => {
      if (cancelled) return;
      // 400（不是已知项目）之类：没有列表也没有可说的原因——Task 就是一句话，不提示。
      if (!r.ok) return;
      setTasks(r.value.tasks ?? []);
      setTasksReason(r.value.reason ?? null);
    });
    return () => {
      cancelled = true;
    };
  }, [catalog, project, target]);

  function worktreeCreated(created: WorktreeInfo) {
    pendingWorktree.current = { path: created.path, created };
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
    !busy &&
    !worktreeCreating &&
    nodeOnline &&
    project.trim() !== "" &&
    name.trim() !== "" &&
    agent !== "" &&
    !needsCommand;

  async function create() {
    setBusy(true);
    setError(null);
    const r = await api.create({
      // 选了 peer 才带 node：本机不发这个键，老节点（不认识 node 的）也照常。
      ...(target ? { node: target } : {}),
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
          {/* 本机 + 已配置的 peer（MISSION §6.4、A45）：离线 / 版本不兼容的列出来但不可选，选中的 peer 经一跳转发执行。 */}
          <select id="na-node" value={node ?? localName} onChange={(e) => pickNode(e.target.value)} disabled={busy}>
            <option value={localName}>{localName || "本机"}</option>
            {peers.map((n) => (
              <option key={n.name} value={n.name} disabled={!n.online}>
                {nodeOptionLabel(n)}
              </option>
            ))}
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
            // 撞上已有目录名 / 分支时 WorktreeSelect 换成 -2（agora-h5x），这里仍传原始默认。
            defaultName={taskPick || name}
            // 选了 peer 就在那台机器的仓库里建（A45）。
            create={(path, wtName) => catalog.createWorktree(path, wtName, undefined, target)}
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
