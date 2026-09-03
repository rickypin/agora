import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { TerminalClient, type ExitInfo } from "./terminal";

/** scrollback 与运行时的 history-limit 对齐（ADR-001 D6）。 */
export const SCROLLBACK = 10000;

type Link = "connecting" | "attached" | "detached" | "exited";

/**
 * 一个会话的终端：xterm.js + FitAddon ↔ TerminalClient。
 * 组件卸载 = detach（MISSION §4.6）：只关 WS，agent 不受影响。
 */
export function TerminalView({ sessionId }: { sessionId: string }) {
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
      onOutput: (data) => term.write(data),
      onStatus: (s) => {
        if (s === "attached") setLink("attached");
      },
      onExit: (e) => {
        setExit(e);
        setLink("exited");
      },
      onClose: (exited) => setLink(exited ? "exited" : "detached"),
    });
    client.connect(sessionId, term.cols, term.rows);
    const input = term.onData((d) => client.sendInput(d));
    const resize = term.onResize(({ cols, rows }) => client.sendResize(cols, rows));
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(el);
    term.focus();

    return () => {
      ro.disconnect();
      input.dispose();
      resize.dispose();
      client.close();
      term.dispose();
    };
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
