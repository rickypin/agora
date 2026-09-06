import type { WorktreeInfo } from "./api";

interface Props {
  worktrees: WorktreeInfo[];
  /** 选中的 worktree 路径；列表为空时是 ""。 */
  value: string;
  onChange: (path: string) => void;
  disabled: boolean;
}

/**
 * New Agent 对话框的 Worktree 下拉（MISSION §6.4；线框 docs/spec/ux.md）。
 *
 * 从 NewAgentDialog 抽出来是并行施工的接缝（AGENTS.md「热点文件规矩」）：h1k.1 在这里加
 * 「新建…」，h1k.2 在同一对话框加 Task 下拉，各改各的组件。抽取这一步不改行为、DOM 不变。
 */
export function WorktreeSelect({ worktrees, value, onChange, disabled }: Props) {
  return (
    <select
      id="na-worktree"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      disabled={disabled || worktrees.length === 0}
    >
      {worktrees.length === 0 && <option value="">—</option>}
      {worktrees.map((w) => (
        <option key={w.path} value={w.path}>
          {(w.branch ?? w.path) + (w.main ? "" : " ↗")}
        </option>
      ))}
    </select>
  );
}
