/**
 * 从就绪任务起会话时预填的首条 prompt（MISSION §6.4；A43；agora-h1k.2）。模板原文在
 * docs/spec/ux.md「New Agent 对话框线框」，改这里要一起改那里。
 *
 * 要素：issue id、标题、"先 `bd update <id> --claim`"（claim 是 agent 开工的纪律，agora 自己对
 * beads 零写入）、"`bd show <id>` 读任务书"、"按 AGENTS.md 任务纪律，commit subject 末尾带 (<id>)"、
 * "做完 `bd close <id> --reason` 写证据"。整段不含单引号：节点把它单引号包住接到启动命令尾
 * （docs/spec/api.md「从就绪任务起会话」），用户改出单引号也没事——节点按 sh 的写法拆接。
 */
export function taskPrompt(id: string, title: string): string {
  return [
    `任务 ${id}：${title}`,
    "",
    `先 \`bd update ${id} --claim\`，再 \`bd show ${id}\` 读任务书（description / notes / acceptance）。`,
    `按 AGENTS.md 的任务纪律干活：commit subject 末尾带 (${id})；干活中发现的别的问题用 \`bd create ... --deps discovered-from:${id}\` 另立。`,
    `做完 \`bd close ${id} --reason "<证据>"\` 写证据。`,
  ].join("\n");
}
