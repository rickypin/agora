import { useEffect, useState, type FormEvent } from "react";
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
import { Workspace } from "./Workspace";

type Probe = "probing" | "ok" | "down";
type Auth = "checking" | "paired" | "unpaired" | "pair_failed" | "revoked";

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

  useEffect(() => {
    let cancelled = false;
    fetchHealth()
      .then((ok) => !cancelled && setProbe(ok ? "ok" : "down"))
      .catch(() => !cancelled && setProbe("down"));

    // 地址栏带 #pair=<token> → 兑换成 cookie，再清掉 fragment；否则看已有 session 是否有效。
    // StrictMode 会让这个 effect 同步跑两遍、两遍读到同一个 token：redeemPairOnce 按 token 只 POST 一次
    // （agora-3w8，反例见 pair.ts）；fragment 要等落定后再清，不能在这里提前清给第二遍看。
    const token = extractPairToken(window.location.hash);
    const settle = token
      ? redeemPairOnce(token).then((device) => {
          clearFragment();
          return device ? "paired" : "pair_failed";
        })
      : isPaired().then((ok) => (ok ? "paired" : "unpaired"));
    settle
      .then((state) => !cancelled && setAuth(state as Auth))
      .catch(() => !cancelled && setAuth("unpaired"));
    return () => {
      cancelled = true;
    };
  }, []);

  if (auth !== "paired") {
    return (
      <main className={`gate${mobile ? " gate-mobile" : ""}`} data-text={mobile ? textSize : undefined}>
        <h1>agora</h1>
        <p>daemon：{probe === "probing" ? "探测中…" : probe === "ok" ? "在线" : "不可达"}</p>
        <p>{authLine(auth)}</p>
        {mobile && iosWithoutStandalone() && <InstallHint />}
        <PairPaste onPaired={() => setAuth("paired")} />
      </main>
    );
  }
  if (mobile) return <MobileApp onRevoked={() => setAuth("revoked")} />;
  if (narrow) return <NarrowScreen />;
  // 事件流被服务端以 4401 关掉 = 本设备在别处被吊销（agora-0jt）：不再重连，回到配对门。
  return <Workspace onRevoked={() => setAuth("revoked")} />;
}

/**
 * 门页的粘贴配对（agora-thc.1）：在终端跑 `agora pair`（或 Dashboard 里"配对新设备"）拿到链接，
 * 粘进来兑换。iOS 主屏 PWA 与 Safari 的存储不共享时，这是主屏里补配对的唯一入口——
 * 扫码进 Safari 配好，主屏打开仍是门页，把链接粘进来即可。
 */
function PairPaste({ onPaired }: { onPaired: () => void }) {
  const [text, setText] = useState("");
  const [state, setState] = useState<"idle" | "busy" | "bad">("idle");

  async function submit(e: FormEvent) {
    e.preventDefault();
    const token = extractPairTokenFromText(text);
    if (!token) {
      setState("bad");
      return;
    }
    setState("busy");
    const device = await redeemPairOnce(token);
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
    case "revoked":
      return "本设备的配对已被吊销：要继续用，在本机终端重新运行 agora open";
  }
}
