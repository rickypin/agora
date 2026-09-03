/**
 * 命令面板的 fuzzy 匹配（docs/spec/ux.md：sessions / projects / nodes / actions）。
 *
 * 子序列匹配 + 打分：连续命中加分、词首命中加分、越靠前越好。目标是 20–50 个 agent 时
 * 打三四个字母就能选中，不追求 fzf 的算法保真。
 */

export interface Scored<T> {
  item: T;
  score: number;
}

/** 匹配则返回分数（越大越好），不匹配返回 null。空 query 视为全部命中、同分。 */
export function fuzzyScore(text: string, query: string): number | null {
  const q = query.trim().toLowerCase();
  if (q === "") return 0;
  const t = text.toLowerCase();
  let score = 0;
  let from = 0;
  let prev = -2;
  for (const ch of q) {
    if (ch === " ") continue; // 空格只当分隔，不参与匹配
    const at = t.indexOf(ch, from);
    if (at < 0) return null;
    if (at === prev + 1) score += 8; // 连续
    if (at === 0 || /[\s/@._-]/.test(t[at - 1] ?? "")) score += 4; // 词首
    score -= Math.min(at - from, 8); // 跳过的字符越多越差
    prev = at;
    from = at + 1;
  }
  return score;
}

/** 过滤 + 按分数降序；同分保持原顺序（稳定），好让"第 N 条"可预测。 */
export function fuzzyFilter<T>(items: T[], query: string, text: (x: T) => string): T[] {
  const scored: Scored<T>[] = [];
  items.forEach((item) => {
    const score = fuzzyScore(text(item), query);
    if (score !== null) scored.push({ item, score });
  });
  return scored
    .map((s, i) => ({ ...s, i }))
    .sort((a, b) => b.score - a.score || a.i - b.i)
    .map((s) => s.item);
}
