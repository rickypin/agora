/**
 * 键位层（MISSION §6.5；键位表 docs/spec/ux.md）。
 *
 * 一层两面，所以放在同一个文件里：
 * - agora-xqa.3：把**终端要的**键从浏览器手里抢回来（Shift+Enter、Cmd+←/→）；
 * - agora-xqa.14：全局快捷键**不许吞掉**终端要的 Ctrl 组合；
 * - agora-82g：终端聚焦时全局快捷键也得按得动——xterm 处理完 Ctrl+字母、Alt+字母（Linux /
 *   Windows 发 `ESC x`）会 preventDefault + stopPropagation，window 上的全局层永远收不到。
 *   所以终端层把 matchShortcut 认的、**不带 Ctrl** 的键（Cmd 系、Alt/Option 系）让给全局层；
 *   Ctrl 组合一律留给 pane（Ctrl+K 是 kill-line、Ctrl+F 是 vim / less 的翻页），Linux / Windows
 *   在终端里开面板 / 过滤用 Alt/Option+K / F。
 *
 * 与 DOM 无关：只看事件的形状（`KeyLike`），所以能在 node 环境里直接测；接线在
 * TerminalView.tsx（终端侧）与 Workspace.tsx（全局侧）。
 */

/** 只用得上的那几个字段——测试里直接造对象，不必伪造整个 KeyboardEvent。 */
export interface KeyLike {
  key: string;
  /** 物理键位。macOS 上 Option+1 的 `key` 是 `¡`，只有 `code` 还认得出 Digit1。 */
  code?: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  type?: string;
}

/**
 * 终端聚焦时必须原样到达 pane 的 Ctrl 组合（MISSION §6.5 硬约束）。
 * 全局快捷键永远不碰这几个键——真要加新快捷键，先在这里撞一下。
 */
export const TERMINAL_CTRL_KEYS = ["a", "c", "d", "e", "r", "z"] as const;

export type ShortcutAction =
  | { action: "palette" }
  | { action: "filter" }
  | { action: "new" }
  | { action: "next" }
  | { action: "prev" }
  /** 侧栏第 index+1 条（按当前过滤后的显示顺序）。 */
  | { action: "jump"; index: number };

/**
 * 全局快捷键（Cmd/Ctrl+K/F、Alt/Option+K/F、Alt/Option+1…9、Alt/Option+]/[、Alt/Option+N）。
 *
 * Alt/Option+K / F 是面板与过滤在终端聚焦时的入口（agora-82g）：Ctrl+K / F 那时归 pane，macOS 还有
 * Cmd 可用，Linux / Windows 只剩 Alt。Alt+F 在 Linux 的 readline 里是 forward-word（Alt+→ 同义，
 * xterm 照发），macOS 上 Option+F 本来只会打出 `ƒ`，拿走不亏。
 *
 * 除了面板与过滤这两个 Cmd/Ctrl 组合，其余一律走 Alt/Option：Cmd/Ctrl+数字、
 * Cmd/Ctrl+Shift+]/[、Cmd/Ctrl+N 都是浏览器自己的加速键（标签页切换、新窗口），
 * 在浏览器 UI 层就被吃掉，页面连 keydown 都收不到，`preventDefault` 救不回来
 * （macOS Chrome 人眼实测 2026-09-04，agora-rzn；jsdom 与 CDP 注入的按键都测不出
 * 这件事——它们绕过了浏览器的加速键处理，只会给出假阳性）。
 * 返回 null = 这个键与 agora 无关，让它去它本来该去的地方。
 */
export function matchShortcut(ev: KeyLike): ShortcutAction | null {
  if (ev.type && ev.type !== "keydown") return null;
  const mod = ev.metaKey || ev.ctrlKey;

  // Alt/Option 系：数字跳转、上下一个、新建。都不带 Cmd/Ctrl，靠 code 认——macOS 上
  // Option+] 的 key 是 `‘`、Option+N 是死键 `Dead`，只有 code 还认得出（见 KeyLike.code）。
  if (ev.altKey && !mod) {
    const d = digit(ev);
    if (d !== null) return { action: "jump", index: d - 1 };
    switch (ev.code ?? "") {
      case "BracketRight":
        return { action: "next" };
      case "BracketLeft":
        return { action: "prev" };
      case "KeyN":
        return { action: "new" };
      case "KeyK":
        return { action: "palette" };
      case "KeyF":
        return { action: "filter" };
      default:
        return null;
    }
  }
  if (!mod || ev.altKey) return null;

  // 硬约束：Ctrl+C/D/Z/R/A/E 这些是终端的键，全局层一律不认（哪怕将来手滑给它们绑了动作）。
  if (ev.ctrlKey && !ev.metaKey && (TERMINAL_CTRL_KEYS as readonly string[]).includes(ev.key.toLowerCase())) {
    return null;
  }

  // Cmd/Ctrl 系只剩这两个——它们浏览器不保留，页面按得住。
  if (ev.shiftKey) return null;
  switch (ev.key.toLowerCase()) {
    case "k":
      return { action: "palette" };
    case "f":
      return { action: "filter" };
    default:
      return null;
  }
}

function digit(ev: KeyLike): number | null {
  const m = /^Digit([1-9])$/.exec(ev.code ?? "");
  if (m) return Number(m[1]);
  return /^[1-9]$/.test(ev.key) ? Number(ev.key) : null;
}

/**
 * 终端里要替浏览器代劳的键（agora-xqa.3）。返回要发给 pane 的字节，null = 交回 xterm。
 *
 * Shift+Enter 发 `ESC CR`（= Option/Alt+Enter）而不是裸 CR：xterm.js 默认把 Shift+Enter
 * 也发成 CR，TUI 分不出"换行"和"发送"，于是一按就提交（2026-09-02 Chrome + Claude Code 实测）。
 * 选 `ESC CR` 而不是 kitty/CSI-u 的 `ESC[13;2u`：后者要 TUI 先打开 kitty keyboard protocol，
 * 没打开就会被当成乱码打进输入框；`ESC CR` 是 Claude Code / Codex 今天就认的"换行不发送"，
 * 也正是各家 terminal-setup 给 iTerm2 装的那条约定。
 *
 * Cmd+←/→ 映射成 Home/End：浏览器把它们留给了历史前进后退，不拦就会真的退出页面、
 * 顺手丢掉整个终端视图；xterm.js 又不转发它们，所以只能这里代发。
 */
export function terminalKey(ev: KeyLike): string | null {
  if (ev.type && ev.type !== "keydown") return null;
  if (ev.key === "Enter" && ev.shiftKey && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
    return "\x1b\r";
  }
  if (ev.metaKey && !ev.ctrlKey && !ev.altKey && !ev.shiftKey) {
    if (ev.key === "ArrowLeft") return "\x1b[H";
    if (ev.key === "ArrowRight") return "\x1b[F";
  }
  return null;
}

/** `attachCustomKeyEventHandler` 的形状：false = 这个键我处理了，xterm 别再管。 */
export interface Preventable extends KeyLike {
  preventDefault(): void;
  stopPropagation?(): void;
}

/**
 * 终端的自定义 key handler：认识的键自己发字节并 `preventDefault`（否则 Cmd+← 照样让
 * 浏览器后退）；全局快捷键里不带 Ctrl 的那些返回 false 让 xterm 别碰、事件照常冒泡到 window
 * 由全局层处理（agora-82g）；其余一律返回 true 交回 xterm——Ctrl+C/D/Z/R/A/E、Ctrl+K/F、
 * Option+←/→ 按词跳、粘贴都走 xterm 原路，这一层不碰。
 */
export function handleTerminalKey(ev: Preventable, send: (data: string) => void): boolean {
  const bytes = terminalKey(ev);
  if (bytes !== null) {
    ev.preventDefault();
    send(bytes);
    return false;
  }
  // 2026-09-05 agent-browser 实测：焦点在 xterm 时 Ctrl+K 进了 pane 成 ^K、面板没开；xterm 对
  // Ctrl+字母与（非 mac 的）Alt+字母都会 cancel 事件。这里不 preventDefault：由全局层自己做，
  // 它没装（手机断点）时键就按浏览器缺省走。Ctrl 组合永远不让——MISSION §6.5 的硬约束。
  if (!ev.ctrlKey && matchShortcut(ev) !== null) return false;
  return true;
}

/** 桌面断点（index.css 的侧栏宽度也是照这个来的）。 */
export const DESKTOP_MIN_WIDTH = 700;

/**
 * 桌面断点才装全局快捷键与命令面板（MISSION §6.5：手机端没有键盘）。V1 只做桌面断点。
 * 看 innerWidth 而不是 matchMedia：jsdom 的 matchMedia 恒为 false，用它测不出"装上了"。
 */
export function isDesktop(): boolean {
  if (typeof window === "undefined") return true;
  return window.innerWidth >= DESKTOP_MIN_WIDTH;
}
