import { expect, it } from "vitest";
import { agentBadge } from "./agentBadge";

it("known adapters get distinct glyphs and hues; shell and unknown are uncolored (A49)", () => {
  const claude = agentBadge("claude");
  const codex = agentBadge("codex");
  const grok = agentBadge("grok");
  const pi = agentBadge("pi");
  expect(claude).toEqual({ glyph: "✦", label: "Claude", hue: 30 });
  expect(codex).toEqual({ glyph: "◆", label: "Codex", hue: 200 });
  expect(grok).toEqual({ glyph: "✧", label: "Grok", hue: 280 });
  expect(pi).toEqual({ glyph: "π", label: "pi", hue: 140 });
  const glyphs = [claude.glyph, codex.glyph, grok.glyph, pi.glyph];
  const hues = [claude.hue, codex.hue, grok.hue, pi.hue];
  expect(new Set(glyphs).size).toBe(4);
  expect(new Set(hues).size).toBe(4);
  expect(hues.every((h) => h !== 0)).toBe(true);

  const shell = agentBadge("shell");
  expect(shell).toEqual({ glyph: "$", label: "shell", hue: 0 });
  const unknown = agentBadge("my-agent");
  expect(unknown).toEqual({ glyph: "?", label: "my-agent", hue: 0 });
  const custom = agentBadge("custom");
  expect(custom).toEqual({ glyph: "?", label: "custom", hue: 0 });
});
