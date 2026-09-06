import { expect, it } from "vitest";
import { taskPrompt } from "./taskPrompt";

it("the template carries the id, the title and the AGENTS.md discipline in order", () => {
  // MISSION §6.4：任务 id 进首条 prompt，claim 由 agent 自己做——模板得把纪律说全，且顺序是
  // 开工（claim → 读任务书）→ 干活（commit 引用）→ 收工（close 写证据）。
  const p = taskPrompt("agora-h1k.2", "从 bd ready 选任务起会话");
  expect(p.startsWith("任务 agora-h1k.2：从 bd ready 选任务起会话")).toBe(true);
  const steps = [
    "`bd update agora-h1k.2 --claim`",
    "`bd show agora-h1k.2`",
    "AGENTS.md",
    "(agora-h1k.2)",
    "`bd close agora-h1k.2 --reason",
  ];
  const at = steps.map((s) => p.indexOf(s));
  for (const [i, s] of steps.entries()) expect(at[i], s).toBeGreaterThanOrEqual(0);
  expect(at).toEqual([...at].sort((a, b) => a - b));
  // 整段落在启动命令的一对单引号里（docs/spec/api.md「从就绪任务起会话」）：模板自己不带单引号。
  expect(p).not.toContain("'");
});
