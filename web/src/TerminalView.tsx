import { useEffect, useRef, useState } from "react";
// @xterm/xterm 6.0.0 + @xterm/addon-fit 0.11.0（同一上游 commit 配套发布；agora-hhu，2026-09-06）。
// 为什么是 6.0：5.5.0 的 Viewport 构造函数里有一个未登记的 setTimeout(syncScrollArea)，dispose 之后仍跑一次
// （xtermjs/xterm.js#4983，报告方就是 React strict mode），PR #4984 修在 6.0.0 里；6.0.0 还把 Viewport
// 换成了 VS Code 的 scrollable element（#5096，滚动条变 overlay）。代价是 #5346 删掉了 Alt+←/→ 的词跳改写，
// 那条由 keys.ts 接管——不要因为"6.0 少了词跳"退回 5.5。
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { handleTerminalKey, isImePunctuation } from "./keys";
import { TerminalClient, type ExitInfo, type TerminalClientOptions } from "./terminal";

/** scrollback 与运行时的 history-limit 对齐（ADR-001 D6）。 */
export const SCROLLBACK = 10000;

type Link = "connecting" | "attached" | "read_only" | "detached" | "exited";

interface Props {
  sessionId: string;
  /** 测试注入：建 WS 的方式；默认同源 `/api/sessions/<id>/terminal`。 */
  connect?: TerminalClientOptions["connect"];
  /** 挂着的终端的 focus()：Workspace 在点已激活的行 / 标签页时把焦点交回来（agora-vcc）。
   * 挂载时填、卸载时清空。 */
  focusRef?: { current: (() => void) | null };
  /** 只读终端（agora-h1k.5 的 diff）：xterm 不收键入、不向 WS 发 input；服务端那头也丢 input，两边各守一半。
   * 退出后的按钮是"重新运行"（再跑一次 git diff）而不是"重新连接"。挂载时定死，与 connect 同理不进依赖。 */
  readOnly?: boolean;
}

/**
 * 一个会话的终端：xterm.js + FitAddon ↔ TerminalClient。
 * 组件卸载 = detach（MISSION §4.6）：只关 WS，agent 不受影响。
 */
export function TerminalView({ sessionId, connect, focusRef, readOnly = false }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const [link, setLink] = useState<Link>("connecting");
  const [exit, setExit] = useState<ExitInfo | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    setLink("connecting");
    setExit(null);
    const term = new Terminal({
      scrollback: SCROLLBACK,
      cursorBlink: !readOnly,
      disableStdin: readOnly,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
      fontSize: 13,
      theme: { background: "#0e1116", foreground: "#d6dbe3" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();

    const client = new TerminalClient({
      connect,
      onOutput: (data) => term.write(data),
      onStatus: (s) => {
        // read_only 是只读终端的"已连上"（docs/spec/api.md「只读产出」）：同样交焦点，滚动 / 选区要它。
        if (s === "attached" || s === "read_only") {
          setLink(s === "read_only" ? "read_only" : "attached");
          // 挂载时那一次 focus 在新开的浏览器标签页里偶尔不生效（agora-p29，2026-09-03 目检：
          // attach 成功但键入不进 pane，点一下终端才好）；attached 到达时再交一次焦点。
          // 用户这会儿已经在别的输入框里打字的话不抢。
          if (!typingElsewhere(el)) term.focus();
        }
      },
      onExit: (e) => {
        setExit(e);
        setLink("exited");
      },
      onClose: (exited) => setLink(exited ? "exited" : "detached"),
    });
    client.connect(sessionId, term.cols, term.rows);
    // 只读时这一头就不发 input：服务端也丢，但少发一帧就少一分歧义。
    const send = readOnly ? () => {} : (d: string) => client.sendInput(d);
    const input = term.onData(send);
    // 浏览器抢走的那几个键由这一层代发（agora-xqa.3）；Option/Alt+←/→ 的词跳字节也在这里发
    // （agora-hhu：xterm 6.0 不再替 embedder 做这件事），按浏览器所在平台选、挂载时算一次；
    // 其余一律交回 xterm，终端里的 Ctrl+C/D/Z/R/A/E 不经过任何 agora 的判断（MISSION §6.5）。
    const keyOpts = { mac: isMacLike() };
    term.attachCustomKeyEventHandler((ev) => handleTerminalKey(ev, send, keyOpts));
    // 标点键的 keydown 让给了浏览器 / 输入法（keys.ts isImePunctuationKey，agora-n0l）：
    // 字符真正落进 xterm 的 textarea 时从 input 事件里发出去。xterm 自己的 input 处理对"见过
    // keydown"的插入不发字节，也不会重复；输入法组合中的 insertCompositionText 仍归 CompositionHelper。
    const onInput = (ev: Event) => {
      const { data, inputType, isComposing } = ev as InputEvent;
      if (inputType === "insertText" && data && !isComposing && isImePunctuation(data)) send(data);
    };
    term.textarea?.addEventListener("input", onInput);
    const resize = term.onResize(({ cols, rows }) => client.sendResize(cols, rows));
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(el);
    // 点到终端区域任何地方都把焦点交回 xterm 的 helper textarea（agora-p29）。不在 pointerdown 上
    // preventDefault：取消 pointerdown 会连带取消后面的 mousedown / click，xterm 的选区靠它们。
    const onPointerDown = () => term.focus();
    // xterm 自己的 mousedown（preventDefault + focus）只覆盖 .xterm 内部；点在 .term-host 的
    // padding 上时浏览器缺省会把焦点挪到 body，这里把那一圈也拦住。
    const onMouseDown = (ev: MouseEvent) => {
      if (ev.target === el) ev.preventDefault();
    };
    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("mousedown", onMouseDown);
    if (focusRef) focusRef.current = () => term.focus();
    term.focus();

    return () => {
      if (focusRef) focusRef.current = null;
      term.textarea?.removeEventListener("input", onInput);
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("mousedown", onMouseDown);
      ro.disconnect();
      input.dispose();
      resize.dispose();
      client.close();
      // React 19 <StrictMode> 开发模式对每个 effect 同步地挂载 → 清理 → 再挂载，所以 dev 里这行每次挂载都会
      // 跑一遍。xterm 5.5.0 时这里紧跟着一条 "Uncaught TypeError: Cannot read properties of undefined (reading
      // 'dimensions')"（Viewport 构造函数里未登记的 setTimeout(syncScrollArea) 在 dispose 后仍跑一次；
      // xtermjs/xterm.js#4983，agora-oir 代检 dev+StrictMode 每挂一次 +1 条），6.0.0 的 PR #4984 修掉了它，
      // 2026-09-06 agent-browser 代检（agora-hhu）同一路径 0 条。6.0.0 仍留有一个同类残余：dispose 时鼠标按着
      // 不放，下一次 mouseup 走 RenderService.dimensions 抛同一条（#6070，修复 PR #6019 在 7.0.0 milestone）——
      // 与 StrictMode 双挂载无关，看到了别当成 agora 的焦点 / 挂载故障去查，也不要为它延迟 dispose 或吞 window.onerror。
      term.dispose();
    };
    // connect 是挂载时定死的测试注入，不进依赖：内联闭包每次渲染都是新引用，进了就会
    // 每 setLink 一次重建终端与 WS。focusRef 是 Workspace 的 useRef，引用不变，同理不进。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, attempt]);

  return (
    <div className="term">
      <div className="term-bar">
        <span className={`link link-${link}`}>{linkLabel(link, exit)}</span>
        {link !== "attached" && link !== "read_only" && link !== "connecting" && (
          <button onClick={() => setAttempt((n) => n + 1)}>{readOnly ? "重新运行" : "重新连接"}</button>
        )}
      </div>
      <div className="term-host" ref={host} />
    </div>
  );
}

/**
 * 浏览器跑在 macOS / iOS 上：Option+←/→ 发 `ESC b` / `ESC f`，否则发 `ESC[1;5D/C`（keys.ts terminalKey）。
 * 判法照 xterm 5.5 自己的 src/common/Platform.ts（isMac 认 Macintosh / MacIntel / MacPPC / Mac68K，
 * 另有 iPad / iPhone）——升级到 6.0 后 xterm 不再替我们判，这里得自己判，且判的是**浏览器**的平台，
 * 不是 pane 所在机器的：5.5 也是这么做的，键位随用户手里的键盘走。
 */
function isMacLike(): boolean {
  if (typeof navigator === "undefined") return false;
  return /^(Mac|iPhone|iPad)/.test(navigator.platform ?? "");
}

/** 焦点在终端之外的某个文本输入上（侧栏过滤、Rename、New Agent 表单…）。 */
function typingElsewhere(host: HTMLElement): boolean {
  const a = document.activeElement;
  if (!a || a === document.body || host.contains(a)) return false;
  return a instanceof HTMLInputElement || a instanceof HTMLTextAreaElement || a instanceof HTMLSelectElement || (a as HTMLElement).isContentEditable === true;
}

function linkLabel(link: Link, exit: ExitInfo | null): string {
  switch (link) {
    case "connecting":
      return "连接中…";
    case "attached":
      return "已连接";
    case "read_only":
      return "只读";
    case "detached":
      return "已断开（agent 仍在运行）";
    case "exited":
      return exit
        ? `终端流结束：${exit.kind === "code" ? `退出码 ${exit.value}` : `信号 ${exit.value}`}`
        : "终端流结束";
  }
}
