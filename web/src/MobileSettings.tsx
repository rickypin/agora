/**
 * /m 设置屏（agora-thc.7）：推送开关、不可达降级提示、加主屏引导、吊销本设备。
 *
 * 数据只有两处来源：`push.ts`（本设备的权限与订阅；经注入的 [`PushEnv`] 可单测）与
 * `/api/health` 的 `push.apple`（承载节点到不了 Apple 时显示原因；MISSION §10.3）。
 */
import { useEffect, useMemo, useState } from "react";
import { ConfirmDialog } from "./ConfirmDialog";
import { InstallHint, iosWithoutStandalone } from "./InstallHint";
import { apiFetch } from "./net";
import { browserPushEnv, disablePush, enablePush, selfCheckPush, type PushEnv, type PushReport } from "./push";

interface Props {
  onClose: () => void;
  /** 本设备凭据已被吊销 / 用户点了「吊销本设备」：App 换回配对门。 */
  onRevoked: () => void;
  /** 测试注入；默认浏览器环境。 */
  env?: PushEnv;
  /** 承载节点推送可达性；默认拉一次 `/api/health`。 */
  probe?: () => Promise<{ apple: boolean | null; reason: string | null }>;
}

async function fetchPushHealth(): Promise<{ apple: boolean | null; reason: string | null }> {
  try {
    const resp = await apiFetch("/api/health");
    if (!resp.ok) return { apple: null, reason: null };
    const body = (await resp.json()) as { push?: { apple?: boolean | null; reason?: string | null } };
    return { apple: body.push?.apple ?? null, reason: body.push?.reason ?? null };
  } catch {
    return { apple: null, reason: null };
  }
}

const STATE_TEXT: Record<PushReport["state"], string> = {
  on: "已开启",
  off: "未开启",
  denied: "权限被拒绝",
  unsupported: "这个环境不支持",
  insecure: "需要 HTTPS",
  "no-key": "承载节点未启用推送",
  failed: "订阅失败",
};

export function MobileSettings({ onClose, onRevoked, env, probe }: Props) {
  // 这两个必须是稳定引用：effect 依赖它们，而 `browserPushEnv()` 每次调用都返回新对象——
  // 直接写在渲染里会让 effect 每渲染一次跑一次、`setReport` 再触发渲染，设置页当场死循环
  // （2026-10-07 iPhone 实测：点「设置」像没反应；桌面复现是 renderer 卡死）。
  const pushEnv = useMemo(() => env ?? browserPushEnv(), [env]);
  const probeFn = useMemo(() => probe ?? fetchPushHealth, [probe]);
  const [report, setReport] = useState<PushReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [degrade, setDegrade] = useState<{ apple: boolean | null; reason: string | null } | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);

  useEffect(() => {
    // 打开设置先自查一次（iOS 可能静默丢订阅）：自查结果就是首屏状态。
    void selfCheckPush(pushEnv).then(setReport);
    void probeFn().then(setDegrade);
  }, [pushEnv, probeFn]);

  async function toggle() {
    setBusy(true);
    try {
      // 开：必须在用户手势里请求权限（iOS 的规矩），所以不在 useEffect 里自动开。
      const next = report?.state === "on" ? await disablePush(pushEnv) : await enablePush(pushEnv);
      setReport(next);
    } finally {
      setBusy(false);
    }
  }

  async function revoke() {
    setConfirmRevoke(false);
    try {
      await apiFetch("/api/auth/logout", { method: "POST" });
    } finally {
      onRevoked();
    }
  }

  const state = report?.state ?? "off";
  const canToggle = !["unsupported", "insecure"].includes(state);
  return (
    <main className="mobile mobile-settings" data-testid="mobile-settings">
      <header className="mobile-top">
        <button type="button" className="mobile-back" data-testid="mobile-settings-back" onClick={onClose}>
          ←
        </button>
        <h1>设置</h1>
      </header>

      <section className="mobile-section" aria-label="推送">
        <h2>
          <span>推送</span>
          <span data-testid="mobile-push-state">{STATE_TEXT[state]}</span>
        </h2>
        <button
          type="button"
          className="mobile-push-toggle"
          data-testid="mobile-push-toggle"
          disabled={busy || !canToggle}
          aria-pressed={state === "on"}
          onClick={() => void toggle()}
        >
          {state === "on" ? "关" : "开"}
        </button>
        {report?.detail && (
          <p className="muted" data-testid="mobile-push-detail">
            {report.detail}
          </p>
        )}
        {state === "denied" && (
          <p className="muted">去 iPhone 的设置 → 通知 → agora 里打开「允许通知」，再回来点一次。</p>
        )}
        {state === "unsupported" && iosWithoutStandalone() && <InstallHint />}
        {degrade?.apple === false && (
          <p className="mobile-note warning" data-testid="mobile-push-unreachable">
            推送不可达：{degrade.reason ?? "未知原因"}；PWA 打开期间仍实时更新。
          </p>
        )}
        <p className="muted">推送到的是标题（哪个会话在等你），不含命令 / 回复原文；点开落到会话卡。</p>
      </section>

      <section className="mobile-section" aria-label="本设备">
        <h2>
          <span>本设备</span>
        </h2>
        <button type="button" className="danger" data-testid="mobile-revoke-self" onClick={() => setConfirmRevoke(true)}>
          吊销本设备
        </button>
        <p className="muted">吊销后这台设备要重新配对：终端跑 agora pair 扫码，或在门页粘贴配对链接。</p>
      </section>

      {confirmRevoke && (
        <ConfirmDialog
          title="吊销本设备？"
          body="这台设备上的凭据马上失效，页面会回到配对页；它登记的推送订阅也随之停发。"
          confirmLabel="吊销"
          onConfirm={() => void revoke()}
          onCancel={() => setConfirmRevoke(false)}
        />
      )}
    </main>
  );
}
