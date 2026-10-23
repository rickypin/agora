import { lazy, Suspense, useEffect, useState, type FormEvent } from "react";
import { fetchHealth } from "./health";
import { MobileApp } from "./MobileApp";
import { loadMobileTextSize } from "./mobileText";
import { isMobilePath, useNarrow } from "./mobileRoute";
import {
  clearFragment,
  extractPairToken,
  extractPairTokenFromText,
  isPaired,
  redeemPairOnce,
} from "./pair";
import { InstallHint, iosWithoutStandalone } from "./InstallHint";

// 桌面工作台按需加载（agora-up0p）：它的依赖链带着 xterm（约 333 KB，占原 entry 一半），
// /m 永远不渲染它就不该为它付首包。Suspense 的 fallback 只在桌面首屏出现一下。
const Workspace = lazy(() => import("./Workspace").then((module) => ({ default: module.Workspace })));

type Probe = "probing" | "ok" | "down";
type Auth = "checking" | "paired" | "unpaired" | "pair_failed" | "probe_failed" | "revoked";

/** 入口：配对门 → Terminal Workspace（agora-xqa.11）。Dashboard 的 attention 视图随 M1b 落地（agora-dvh.6）。 */
export function App() {
  const [probe, setProbe] = useState<Probe>("probing");
  const [auth, setAuth] = useState<Auth>("checking");
  // 手机入口按地址栏判（`/m`）：同一份 bundle、同一套 api / events / attention，与桌面只差一个壳。
  const mobile = isMobilePath(window.location.pathname);
  // 桌面窗口窄于 700 px：不做抽屉式全功能布局，引导去 /m（MISSION §6.6；agora-thc.5）。
  const narrow = useNarrow();
  // 手机门页也跟 /m 用同一个字号档位（agora-x70t.xsgz）：重配对时要读的那几行正是最需要看清的。
  const [textSize] = useState(() => loadMobileTextSize());
  // 门页的「重试」：重跑健康探测与凭据探测（agora-axz0）。不重载页面，避免丢掉粘贴框里的链接。
  const [authAttempt, setAuthAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setProbe("probing");
    fetchHealth()
      .then((ok) => !cancelled && setProbe(ok ? "ok" : "down"))
      .catch(() => !cancelled && setProbe("down"));

    // 2026-10-11：同标签页打开配对链接只触发 hashchange，不重挂 App（agora-auuw）。
    // generation 让较早的凭据探测或兑换不能覆盖新链接；StrictMode 的重复兑换仍按 token 去重。
    let generation = 0;
    function checkAuth() {
      const token = extractPairToken(window.location.hash);
      const current = ++generation;
      setAuth("checking");
      const settle: Promise<Auth> = token
        ? redeemPairOnce(token).then((device) => device ? "paired" : "pair_failed")
        : isPaired().then((ok) => ok ? "paired" : "unpaired");
      // 网络失败既不是「已配对」也不是「未配对 / 链接无效」：单独一档，别撒谎，也别把
      // 一次性 token 当成用掉了（agora-axz0）。
      void settle.catch((): Auth => "probe_failed").then((state) => {
        if (cancelled || current !== generation) return;
        // 地址栏可能已改变，但对应事件尚未送达：不要清掉新 token 或应用旧结果。
        if (token && extractPairToken(window.location.hash) !== token) return;
        // 只有服务端给了确定答复才抹掉 token：probe_failed 时留着，重试 / 刷新还能再兑换。
        if (token && state !== "probe_failed") clearFragment();
        setAuth(state);
      });
    }
    function onHashChange() {
      if (extractPairToken(window.location.hash)) checkAuth();
    }
    window.addEventListener("hashchange", onHashChange);
    checkAuth();
    return () => {
      cancelled = true;
      window.removeEventListener("hashchange", onHashChange);
    };
  }, [authAttempt]);

  if (auth !== "paired") {
    return (
      <main className={`gate${mobile ? " gate-mobile" : ""}`} data-text={mobile ? textSize : undefined}>
        <h1>agora</h1>
        <p>daemon：{probe === "probing" ? "探测中…" : probe === "ok" ? "在线" : "不可达"}</p>
        <p>{authLine(auth)}</p>
        {auth === "probe_failed" && (
          <button type="button" data-testid="auth-retry" onClick={() => setAuthAttempt((n) => n + 1)}>
            重试
          </button>
        )}
        {mobile && iosWithoutStandalone() && <InstallHint />}
        <PairPaste onPaired={() => setAuth("paired")} />
      </main>
    );
  }
  if (mobile) return <MobileApp onRevoked={() => setAuth("revoked")} />;
  if (narrow) return <NarrowScreen />;
  // 事件流被服务端以 4401 关掉 = 本设备在别处被吊销（agora-0jt）：不再重连，回到配对门。
  return (
    <Suspense
      fallback={
        <main className="gate" data-testid="workspace-loading">
          <p>正在加载工作台…</p>
        </main>
      }
    >
      <Workspace onRevoked={() => setAuth("revoked")} />
    </Suspense>
  );
}

/**
 * 门页的粘贴配对（agora-thc.1）：在终端跑 `agora pair`（或 Dashboard 里"配对新设备"）拿到链接，
 * 粘进来兑换。iOS 主屏 PWA 与 Safari 的存储不共享时，这是主屏里补配对的唯一入口——
 * 扫码进 Safari 配好，主屏打开仍是门页，把链接粘进来即可。
 */
function PairPaste({ onPaired }: { onPaired: () => void }) {
  const [text, setText] = useState("");
  const [state, setState] = useState<"idle" | "busy" | "bad" | "offline">("idle");

  async function submit(e: FormEvent) {
    e.preventDefault();
    const token = extractPairTokenFromText(text);
    if (!token) {
      setState("bad");
      return;
    }
    setState("busy");
    // 网络失败不是「链接无效」：不把一次性的 token 当成用掉了，输入框里的链接也留着，
    // 连上之后点一次「配对」就能重试（agora-axz0；redeemPairOnce 不缓存失败）。
    let device: Awaited<ReturnType<typeof redeemPairOnce>>;
    try {
      device = await redeemPairOnce(token);
    } catch {
      setState("offline");
      return;
    }
    if (device) {
      clearFragment();
      onPaired();
    } else {
      setState("bad");
    }
  }

  return (
    <form className="pair-paste" data-testid="pair-paste" onSubmit={(e) => void submit(e)}>
      <input
        value={text}
        placeholder="粘贴配对链接或 token"
        aria-label="粘贴配对链接"
        data-testid="pair-paste-input"
        onChange={(e) => {
          setText(e.target.value);
          setState("idle");
        }}
      />
      <button type="submit" disabled={state === "busy"} data-testid="pair-paste-submit">
        {state === "busy" ? "配对中…" : "配对"}
      </button>
      {state === "bad" && (
        <p className="error" data-testid="pair-paste-error">
          链接无效或已用过；重新生成一条再试。
        </p>
      )}
      {state === "offline" && (
        <p className="error" data-testid="pair-paste-offline">
          连不上节点：检查网络 / Tailscale 后再点一次「配对」。
        </p>
      )}
    </form>
  );
}

/** 桌面窗口窄于 700 px 的引导页：桌面控制台放不下，手机形态在 /m（MISSION §6.6；agora-thc.5）。 */
function NarrowScreen() {
  return (
    <main className="gate narrow-guide">
      <h1>agora</h1>
      <p>窗口太窄，桌面控制台放不下。</p>
      <p>
        <a href="/m">打开手机收件箱 /m</a>，或把窗口拉宽（≥ 700px）。
      </p>
    </main>
  );
}

function authLine(auth: Auth): string {
  switch (auth) {
    case "checking":
      return "凭据：检查中…";
    case "paired":
      return "凭据：本设备已配对";
    case "pair_failed":
      return "配对链接无效、已用或已过期：在本机终端重新运行 agora open";
    case "unpaired":
      return "本设备未配对：在本机终端运行 agora open";
    case "probe_failed":
      return "凭据：暂时无法确认（连不上节点）：检查网络 / Tailscale 后重试";
    case "revoked":
      return "本设备的配对已被吊销：要继续用，在本机终端重新运行 agora open";
  }
}
