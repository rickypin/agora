import { useState } from "react";
import type { ApiErrorBody, WorktreeInfo, WriteResult } from "./api";

/** 下拉里「新建…」那一项的 value；不是路径，永远不会和 git 报的 path 撞。 */
export const NEW_WORKTREE = "__new__";

interface Props {
  worktrees: WorktreeInfo[];
  /** 选中的 worktree 路径；列表为空时是 ""。 */
  value: string;
  onChange: (path: string) => void;
  disabled: boolean;
  /** 仓库路径，`POST /api/projects/worktrees` 的 `path`。 */
  project: string;
  /**
   * 名字输入框的默认值：Name 栏（MISSION §6.4：名字默认 issue id，无 bd 用 Name；issue id
   * 随 A43 的 Task 下拉落地后由它填进 Name）。打开框时若与已有 worktree 目录名 / 分支撞了，
   * `suggestWorktreeName` 会换成 -2，不把这个值原样送进 POST。
   */
  defaultName: string;
  /** 真正去建的那一步（`catalogApi().createWorktree`）；注进来是为了测试不开网络。 */
  create: (path: string, name: string) => Promise<WriteResult<WorktreeInfo>>;
  /** 建成了：父组件重拉列表并选中它。 */
  onCreated: (created: WorktreeInfo) => void;
  /** 名字框打开着（填名字 / 等 POST）时为 true：父组件据此先别让 Create 起会话。 */
  onCreatingChange?: (creating: boolean) => void;
}

/** 路径最后一段：节点判 worktree_exists 用的就是这个（`Path::file_name`）。 */
function worktreeDirName(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  const slash = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return slash >= 0 ? trimmed.slice(slash + 1) : trimmed;
}

/**
 * 打开「新建…」时填进名字框的默认值。
 *
 * 节点把已有 worktree 的目录名和分支都算进「同名已登记」（`src/project/worktree.rs`：
 * `file_name == name` 或 `branch == name` → 409 `worktree_exists`）。没选任务时
 * `defaultName` = Name 栏 = 项目名，几乎总会和主 worktree 目录撞——2026-09-07 代检
 * agora-fna 在 `~/code/demo` 上就是 demo vs demo，用户每次都得手改，和 §6.4「2–3 次
 * 操作起会话」相悖。撞了就换成 `name-2`、`-3`…（issue 点名的那条；留空会禁用「建」）。
 * 空串原样：建按钮本就会因空禁用。
 */
export function suggestWorktreeName(desired: string, worktrees: WorktreeInfo[]): string {
  const taken = new Set<string>();
  for (const w of worktrees) {
    const dir = worktreeDirName(w.path);
    if (dir) taken.add(dir);
    if (w.branch) taken.add(w.branch);
  }
  const base = desired.trim();
  if (!base) return "";
  if (!taken.has(base)) return base;
  for (let n = 2; n < 100; n++) {
    const candidate = `${base}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
  return "";
}

/** 失败按错误类型给文案（docs/spec/api.md），不做字符串匹配（MISSION §2.3 规则 10）。 */
export function describeWorktreeError(err: ApiErrorBody): string {
  switch (err.error) {
    case "worktree_exists":
      return "同名 worktree 已存在";
    case "branch_exists":
      return "同名分支已存在";
    case "path_exists":
      return "目录已存在";
    case "bad_request":
      // 节点的 message 已经说明了哪条规则没过（空、含 / 或 ..、空白…）。
      return err.message;
    case "git":
      return `git 失败：${err.message}`;
    default:
      return `${err.error}: ${err.message}`;
  }
}

/**
 * New Agent 对话框的 Worktree 下拉（MISSION §6.4；线框 docs/spec/ux.md）。
 *
 * 从 NewAgentDialog 抽出来是并行施工的接缝（AGENTS.md「热点文件规矩」）：h1k.1 在这里加
 * 「新建…」，h1k.2 在同一对话框加 Task 下拉，各改各的组件。
 *
 * 「新建…」（A44，agora-h1k.1）：选中它不是选了一个 worktree（不调 onChange），而是打开名字框；
 * 确认后 `POST /api/projects/worktrees`，成功把新项交给父组件（重拉列表并选中），失败按错误
 * 类型显示文案、名字框留着改。agora 只管"生"：这里没有删除、没有合并（§1.4 Git GUI 边界）。
 */
export function WorktreeSelect({
  worktrees,
  value,
  onChange,
  disabled,
  project,
  defaultName,
  create,
  onCreated,
  onCreatingChange,
}: Props) {
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function setMode(next: boolean) {
    setCreating(next);
    setError(null);
    onCreatingChange?.(next);
  }

  function pick(v: string) {
    if (v === NEW_WORKTREE) {
      setNewName(suggestWorktreeName(defaultName, worktrees));
      setMode(true);
      return;
    }
    if (creating) setMode(false);
    onChange(v);
  }

  async function confirm() {
    const name = newName.trim();
    if (!name || busy) return;
    setBusy(true);
    setError(null);
    const r = await create(project, name);
    setBusy(false);
    if (r.ok) {
      setMode(false);
      onCreated(r.value);
      return;
    }
    setError(r.needsConfirmation ? "需要确认" : describeWorktreeError(r.error));
  }

  // 列表为空 = 不是 git 仓库（或还没拉到），没有可以"新建"的对象。
  const offerNew = worktrees.length > 0 && project !== "";

  return (
    <>
      <select
        id="na-worktree"
        value={creating ? NEW_WORKTREE : value}
        onChange={(e) => pick(e.target.value)}
        disabled={disabled || worktrees.length === 0}
      >
        {/*
          空项无条件渲染（agora-tl5）：value === "" 是「没选中任何 worktree」，而它以前只在
          列表为空时才有匹配项——列表非空时 select 找不到 value，浏览器落到第一项，DOM 上写着
          「main」而内部状态是空串，写断言会被骗。文案仍是 —（不是「仓库根目录」）：空串的意思
          是「没选」，cwd 由 NewAgentDialog 落回 Project 那个目录，而项目本身可以是一个 linked
          worktree（用过一次就进 projects 表，docs/spec/api.md），那时「仓库根目录」是假话；
          常见情形下它又与列表里的主 worktree 指同一处，一个下拉里两项说同一件事只是噪音。
        */}
        <option value="">—</option>
        {worktrees.map((w) => (
          <option key={w.path} value={w.path}>
            {(w.branch ?? w.path) + (w.main ? "" : " ↗")}
          </option>
        ))}
        {offerNew && <option value={NEW_WORKTREE}>新建…</option>}
      </select>
      {creating && (
        <>
          {/* 占住 .form 网格的 label 列，让名字框对齐在控件列里。 */}
          <span aria-hidden="true" />
          <div style={{ display: "flex", gap: 6, alignItems: "center", flexWrap: "wrap" }}>
            <input
              id="na-worktree-name"
              value={newName}
              placeholder="worktree / 分支名"
              autoFocus
              disabled={disabled || busy}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                // 回车是"建 worktree"，不是提交整个对话框去起会话。
                if (e.key === "Enter") {
                  e.preventDefault();
                  void confirm();
                }
              }}
            />
            <button
              type="button"
              data-testid="worktree-create"
              disabled={disabled || busy || newName.trim() === ""}
              onClick={() => void confirm()}
            >
              建
            </button>
            <button type="button" data-testid="worktree-cancel" disabled={busy} onClick={() => setMode(false)}>
              取消
            </button>
            {error && (
              <span className="error" data-testid="worktree-error">
                {error}
              </span>
            )}
          </div>
        </>
      )}
    </>
  );
}
