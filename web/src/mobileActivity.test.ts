import { describe, expect, it } from "vitest";
import { activityPhrase, activityPhraseForProgress, MOBILE_ACTIVITY_NO_TOKEN } from "./mobileActivity";

describe("手机端活动文案（agora-dflx）", () => {
  it("常见工具名归一到中文短语", () => {
    // pi（agora.ts 只发 tool_name；真录见 testdata/pi/1.0.4/hooks/parallel_tools.jsonl）
    expect(activityPhrase("bash")).toBe("正在运行命令");
    expect(activityPhrase("read")).toBe("正在读取文件");
    expect(activityPhrase("edit")).toBe("正在修改文件");
    expect(activityPhrase("write")).toBe("正在修改文件");
    expect(activityPhrase("grep")).toBe("正在查找文件");
    expect(activityPhrase("find")).toBe("正在查找文件");
    expect(activityPhrase("todo_write")).toBe("正在整理计划");
    // 其它宿主的写法：Claude 首字母大写、Codex 下划线、Grok 的全名。
    expect(activityPhrase("Bash")).toBe("正在运行命令");
    expect(activityPhrase("WebFetch")).toBe("正在查阅资料");
    expect(activityPhrase("Task")).toBe("正在调用子代理");
    expect(activityPhrase("apply_patch")).toBe("正在修改文件");
    expect(activityPhrase("update_plan")).toBe("正在整理计划");
    expect(activityPhrase("run_terminal_command")).toBe("正在运行命令");
  });

  it("未列出的工具带上原名，不落回裸英文词", () => {
    expect(activityPhrase("mcp__github__search")).toBe("正在使用 mcp__github__search");
    expect(activityPhrase("  custom-tool ")).toBe("正在使用 custom-tool");
  });

  it("空 token 没有短语，调用方落「正在处理…」", () => {
    expect(activityPhrase("")).toBeNull();
    expect(activityPhrase("   ")).toBeNull();
    expect(MOBILE_ACTIVITY_NO_TOKEN).toBe("正在处理…");
  });

  it("只有工具名形态才翻：自由文本、多行、超长都不翻（activityPhraseForProgress 返回 null）", () => {
    expect(activityPhraseForProgress("bash")).toBe("正在运行命令");
    expect(activityPhraseForProgress("\nread\n")).toBe("正在读取文件");
    expect(activityPhraseForProgress("mcp__x__y")).toBe("正在使用 mcp__x__y");
    expect(activityPhraseForProgress("")).toBeNull();
    expect(activityPhraseForProgress("第一行\n第二行")).toBeNull();
    expect(activityPhraseForProgress("把配置迁到 yaml")).toBeNull();
    expect(activityPhraseForProgress("x".repeat(65))).toBeNull();
  });
});
