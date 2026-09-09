/**
 * 侧栏行上的 agent 品牌徽标（A49，agora-uvd.7）：文字 glyph + 短标签，零图片零依赖。
 * shell / custom / 未知不着色（hue 0，CSS 不设 --hue 则继承 muted）。
 */

export interface Badge {
  glyph: string;
  label: string;
  hue: number;
}

const KNOWN: Record<string, Badge> = {
  claude: { glyph: "✦", label: "Claude", hue: 30 },
  codex: { glyph: "◆", label: "Codex", hue: 200 },
  grok: { glyph: "✧", label: "Grok", hue: 280 },
  pi: { glyph: "π", label: "pi", hue: 140 },
  shell: { glyph: "$", label: "shell", hue: 0 },
};

export function agentBadge(agentType: string): Badge {
  return KNOWN[agentType] ?? { glyph: "?", label: agentType, hue: 0 };
}
