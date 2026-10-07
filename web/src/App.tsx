import { useEffect, useState } from "react";
import { fetchHealth } from "./health";
import { MobileApp } from "./MobileApp";
import { isMobilePath, useNarrow } from "./mobileRoute";
import { clearFragment, extractPairToken, isPaired, redeemPairOnce } from "./pair";
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
      <main className={`gate${mobile ? " gate-mobile" : ""}`}>
        <h1>agora</h1>
        <p>daemon：{probe === "probing" ? "探测中…" : probe === "ok" ? "在线" : "不可达"}</p>
        <p>{authLine(auth)}</p>
      </main>
    );
  }
  if (mobile) return <MobileApp onRevoked={() => setAuth("revoked")} />;
  if (narrow) return <NarrowScreen />;
  // 事件流被服务端以 4401 关掉 = 本设备在别处被吊销（agora-0jt）：不再重连，回到配对门。
  return <Workspace onRevoked={() => setAuth("revoked")} />;
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
