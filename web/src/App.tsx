import { useEffect, useState } from "react";
import { fetchHealth } from "./health";

type Probe = "probing" | "ok" | "down";

/** 占位页：Dashboard / Terminal Workspace 在 agora-xqa.11 落地。 */
export function App() {
  const [probe, setProbe] = useState<Probe>("probing");

  useEffect(() => {
    let cancelled = false;
    fetchHealth()
      .then((ok) => !cancelled && setProbe(ok ? "ok" : "down"))
      .catch(() => !cancelled && setProbe("down"));
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
    </main>
  );
}
