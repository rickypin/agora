import { useEffect, useState } from "react";
import { fetchHealth } from "./health";
import { clearFragment, extractPairToken, isPaired, redeemPair } from "./pair";

type Probe = "probing" | "ok" | "down";
type Auth = "checking" | "paired" | "unpaired" | "pair_failed";

/** 占位页：Dashboard / Terminal Workspace 在 agora-xqa.11 落地。 */
export function App() {
  const [probe, setProbe] = useState<Probe>("probing");
  const [auth, setAuth] = useState<Auth>("checking");

  useEffect(() => {
    let cancelled = false;
    fetchHealth()
      .then((ok) => !cancelled && setProbe(ok ? "ok" : "down"))
      .catch(() => !cancelled && setProbe("down"));

    // 地址栏带 #pair=<token> → 兑换成 cookie，再清掉 fragment；否则看已有 session 是否有效。
    const token = extractPairToken(window.location.hash);
    const settle = token
      ? redeemPair(token).then((device) => {
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

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: "2rem" }}>
      <h1>agora</h1>
      <p>
        daemon：{probe === "probing" ? "探测中…" : probe === "ok" ? "在线" : "不可达"}
      </p>
      <p>{authLine(auth)}</p>
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
  }
}
