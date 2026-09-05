import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { handleTerminalKey } from "./keys";
import { TerminalClient, type ExitInfo, type TerminalClientOptions } from "./terminal";

/** scrollback 与运行时的 history-limit 对齐（ADR-001 D6）。 */
export const SCROLLBACK = 10000;

type Link = "connecting" | "attached" | "detached" | "exited";

interface Props {
  sessionId: string;
  /** 测试注入：建 WS 的方式；默认同源 `/api/sessions/<id>/terminal`。 */
  connect?: TerminalClientOptions["connect"];
  /** 挂着的终端的 focus()：Workspace 在点已激活的行 / 标签页时把焦点交回来（agora-vcc）。
   * 挂载时填、卸载时清空。 */
  focusRef?: { current: (() => void) | null };
}

/**
 * 一个会话的终端：xterm.js + FitAddon ↔ TerminalClient。
 * 组件卸载 = detach（MISSION §4.6）：只关 WS，agent 不受影响。
 */
export function TerminalView({ sessionId, connect, focusRef }: Props) {
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
      cursorBlink: true,
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
        if (s === "attached") {
          setLink("attached");
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
    const input = term.onData((d) => client.sendInput(d));
    // 浏览器抢走的那几个键由这一层代发（agora-xqa.3）；其余一律交回 xterm，
    // 终端里的 Ctrl+C/D/Z/R/A/E 不经过任何 agora 的判断（MISSION §6.5）。
    term.attachCustomKeyEventHandler((ev) => handleTerminalKey(ev, (d) => client.sendInput(d)));
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
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("mousedown", onMouseDown);
      ro.disconnect();
      input.dispose();
      resize.dispose();
      client.close();
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
        {link !== "attached" && link !== "connecting" && (
          <button onClick={() => setAttempt((n) => n + 1)}>重新连接</button>
        )}
      </div>
      <div className="term-host" ref={host} />
    </div>
  );
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
    case "detached":
      return "已断开（agent 仍在运行）";
    case "exited":
      return exit
        ? `终端流结束：${exit.kind === "code" ? `退出码 ${exit.value}` : `信号 ${exit.value}`}`
        : "终端流结束";
  }
}
