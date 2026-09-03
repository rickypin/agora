import { describe, expect, it } from "vitest";
import { fuzzyFilter, fuzzyScore } from "./fuzzy";

describe("fuzzy", () => {
  it("子序列命中、缺一个字母就不命中", () => {
    expect(fuzzyScore("New claude in agora @ mac", "ncag")).not.toBeNull();
    expect(fuzzyScore("New claude in agora @ mac", "zzz")).toBeNull();
  });

  it("大小写无关，空 query 全命中", () => {
    expect(fuzzyScore("AGORA", "ag")).not.toBeNull();
    expect(fuzzyScore("任意", "")).toBe(0);
  });

  it("连续与词首命中排在散落命中前面", () => {
    const items = ["sglog parser 重构", "s-something-g-else-log"];
    expect(fuzzyFilter(items, "sglog", (x) => x)[0]).toBe("sglog parser 重构");
  });

  it("同分保持原顺序——侧栏的第 N 条才可预测（agora-xqa.14）", () => {
    // 同一段文字命中方式一样 → 同分；贪心的最左匹配下 "a agora" 反而更差，不能拿来当同分例子。
    const items = ["agora 1", "agora 2", "agora 3"];
    expect(fuzzyFilter(items, "agora", (x) => x)).toEqual(items);
    expect(fuzzyFilter(items, "", (x) => x)).toEqual(items);
  });

  it("query 里的空格只当分隔，不参与匹配", () => {
    expect(fuzzyScore("New codex in sglog", "new codex sglog")).not.toBeNull();
  });
});
