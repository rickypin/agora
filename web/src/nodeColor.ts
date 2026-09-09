/**
 * 节点 chip 的色相（A49，agora-uvd.7）：名字字符码求和 mod 360，再落到 8 档固定色板
 * （相邻 45°），两台机不容易撞色。不加配置项。本机行不走这条（muted 边框、不着色）。
 */
const STEPS = 8;
const STEP = 360 / STEPS;

export function nodeHue(name: string): number {
  let sum = 0;
  for (let i = 0; i < name.length; i++) sum += name.charCodeAt(i);
  return Math.floor((sum % 360) / STEP) * STEP;
}
