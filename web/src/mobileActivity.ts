/**
 * 手机端「它在干什么」的一句话（agora-dflx；MISSION §6.9）。
 *
 * 背景（2026-10-10 真机反馈）：pi 在跑时活动事件把 `detail` 换成当前工具名（`bash`、`read`），
 * 卡片把它当「最新回复」画出来——单个英文单词既不是回复也说不清状态。活动层给的只有工具名
 * （`progress` 字段；pi 的扩展只发 `tool_name`，见 `src/adapter/pi/agora.ts`），这里把它翻成
 * 手机上一眼能读的动作。工具参数摘要（哪条命令 / 哪个文件）是另一个决定（agora-pa5w）。
 *
 * 只翻译**动作**、不吞掉不认识的工具：已知工具给中文短语，未列出的（MCP、自定义工具）保留原
 * 名字说成「正在使用 <名字>」。名字按小写、去掉 `_` / `-` 归一，覆盖常见命名——
 * pi（bash / read / edit / write / grep / find / ls / todo_write…）、
 * Claude（Bash / Read / Edit / Grep / WebFetch / Task / TodoWrite…）、
 * Codex（shell / apply_patch / update_plan…）、Grok（run_terminal_command…）。
 *
 * 技术名称本身不在这里改写（设计系统「状态语言」）：改的是「在跑什么动作」这一层表达，
 * 工具身份在桌面仍原样可见。
 */
export const MOBILE_ACTIVITY_NO_TOKEN = "正在处理…";

/** 归一后的工具名 → 一句话；顺序即优先级，写窄一点避免误伤自定义工具。 */
const PHRASES: Array<[RegExp, string]> = [
  [/^(bash|shell|exec|command|run|powershell|cmd|runterminalcommand)$/, "正在运行命令"],
  [/^(read|view|cat|viewimage)$/, "正在读取文件"],
  [/^(edit|write|patch|applypatch|replace|strreplace|notebookedit)$/, "正在修改文件"],
  [/^(grep|glob|find|search|ripgrep|ls|list)$/, "正在查找文件"],
  [/^(webfetch|websearch|fetch|browser|curl)$/, "正在查阅资料"],
  [/^(task|agent|subagent|spawn)$/, "正在调用子代理"],
  [/^(todowrite|todo|updateplan|plan)$/, "正在整理计划"],
];

function normalize(tool: string): string {
  return tool.trim().toLowerCase().replace(/[_-]/g, "");
}

/**
 * 工具名 → 手机端一句话；空输入 → `null`（调用方落 [`MOBILE_ACTIVITY_NO_TOKEN`]）。
 * 未列出的工具返回「正在使用 <原名>」——至少把「这一串是什么」说清楚，不落回一个裸英文词。
 */
export function activityPhrase(tool: string): string | null {
  const name = tool.trim();
  if (name === "") return null;
  const key = normalize(name);
  for (const [pattern, phrase] of PHRASES) {
    if (pattern.test(key)) return phrase;
  }
  return `正在使用 ${name}`;
}

/**
 * 这段进度文本在不在「工具名」这一形态：取第一个非空行，单行无空白、像标识符（`\w . + -`）、
 * 不超过 64 字符。`progress` 在跑的行本应是工具名（活动层写的就是它），但测试夹具、旧节点或
 * 别的路径可能塞进自由文本——那种不翻译，交回调用方的原有摘要逻辑（不把一句话说成工具）。
 */
export function looksLikeToolToken(text: string): boolean {
  const line = text.split("\n").find((l) => l.trim() !== "")?.trim() ?? "";
  return line !== "" && line.length <= 64 && !/\s/.test(line) && /^[\w.+-]+$/u.test(line);
}

/**
 * 供手机端两个面（卡片的活动行、收件箱行的摘要）共用：进度文本是工具名才翻，否则 `null`。
 * 返回 `null` 表示「这不是一个工具名，按普通摘要处理」。
 */
export function activityPhraseForProgress(progress: string): string | null {
  if (!looksLikeToolToken(progress)) return null;
  const line = progress.split("\n").find((l) => l.trim() !== "")!.trim();
  return activityPhrase(line) ?? MOBILE_ACTIVITY_NO_TOKEN;
}
