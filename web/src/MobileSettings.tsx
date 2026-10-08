/**
 * /m 设置屏（agora-thc.7）：推送开关、不可达降级提示、加主屏引导、吊销本设备。
 *
 * 数据只有两处来源：`push.ts`（本设备的权限与订阅；经注入的 [`PushEnv`] 可单测）与
 * `/api/health` 的 `push.apple`（承载节点到不了 Apple 时显示原因；MISSION §10.3）。
 */
import { useEffect, useMemo, useState } from "react";
import { ConfirmDialog } from "./ConfirmDialog";
import { InstallHint, iosWithoutStandalone } from "./InstallHint";
import { webBuild } from "./build";
import { checkWebBuild, type WebVerdict } from "./webVersion";
import { applyOutline, clipboardReport, copyText, outlineEnabled, toggleOutline } from "./mobileDebug";
import { apiFetch } from "./net";
import { browserPushEnv, disablePush, enablePush, selfCheckPush, type PushEnv, type PushReport } from "./push";
import {
  MOBILE_TEXT_SIZES,
  loadMobileTextSize,
  storeMobileTextSize,
  type MobileTextSize,
} from "./mobileText";

async function fetchSystem(): Promise<unknown> {
  try {
    const resp = await apiFetch("/api/system");
    if (!resp.ok) return null;
    return await resp.json();
  } catch {
    return null;
  }
}

interface Props {
  onClose: () => void;
  /** 本设备凭据已被吊销 / 用户点了「吊销本设备」：App 换回配对门。 */
  onRevoked: () => void;
  /** 测试注入；默认浏览器环境。 */
  env?: PushEnv;
  /** 承载节点推送可达性；默认拉一次 `/api/health`。 */
  probe?: () => Promise<{ apple: boolean | null; reason: string | null }>;
  /** 承载节点内嵌的前端构建号比对；默认拉一次 `/api/system`（agora-xu12）。 */
  probeWeb?: () => Promise<unknown>;
  /** 当前字号档位（不给就现读 localStorage，独立渲染（测试）也成立）。 */
  textSize?: MobileTextSize;
  /** 改字号：由 MobileApp 写 state + localStorage（不传就自己写，但只本屏生效）。 */
  onTextSize?: (size: MobileTextSize) => void;
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

export function MobileSettings({ onClose, onRevoked, env, probe, probeWeb, textSize: givenText, onTextSize }: Props) {
  // 这两个必须是稳定引用：effect 依赖它们，而 `browserPushEnv()` 每次调用都返回新对象——
  // 直接写在渲染里会让 effect 每渲染一次跑一次、`setReport` 再触发渲染，设置页当场死循环
  // （2026-10-07 iPhone 实测：点「设置」像没反应；桌面复现是 renderer 卡死）。
  const pushEnv = useMemo(() => env ?? browserPushEnv(), [env]);
  const probeFn = useMemo(() => probe ?? fetchPushHealth, [probe]);
  const probeWebFn = useMemo(() => probeWeb ?? fetchSystem, [probeWeb]);
  // 页面里的构建号 vs 服务端内嵌的那份：不同就是"手机上是旧包"（agora-xu12）。
  const [webVerdict, setWebVerdict] = useState<WebVerdict | null>(null);
  const [report, setReport] = useState<PushReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [degrade, setDegrade] = useState<{ apple: boolean | null; reason: string | null } | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  // 现场取证（agora-x70t）：手机是唯一跑真 Safari 的机器，"哪块出框 / 被裁"做成能看见、能复制的。
  const [outlined, setOutlined] = useState(() => outlineEnabled());
  const [diagReport, setDiagReport] = useState<string | null>(null);
  const [diagNote, setDiagNote] = useState<string | null>(null);
  const [textSize, setTextSize] = useState<MobileTextSize>(() => givenText ?? loadMobileTextSize());

  function pickText(size: MobileTextSize) {
    setTextSize(size);
    storeMobileTextSize(size);
    onTextSize?.(size);
  }

  useEffect(() => {
    // 打开设置先自查一次（iOS 可能静默丢订阅）：自查结果就是首屏状态。
    void selfCheckPush(pushEnv).then(setReport);
    void probeFn().then(setDegrade);
  }, [pushEnv, probeFn]);
  useEffect(() => {
    // 描边状态在屏间是共享的（存 localStorage）：本屏挂载时按它标记一次。
    applyOutline();
    setOutlined(outlineEnabled());
  }, []);
  useEffect(() => {
    // 构建号比对绝不阻塞渲染：拿不到就是 unknown（不提示），别把设置页拖住。
    void probeWebFn().then((system) => setWebVerdict(checkWebBuild(system)));
  }, [probeWebFn]);

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
    <main className="mobile mobile-settings" data-testid="mobile-settings" data-text={textSize}>
      <header className="mobile-top">
        <button type="button" className="mobile-back" aria-label="返回收件箱" data-testid="mobile-settings-back" onClick={onClose}>
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

      <section className="mobile-section" aria-label="显示">
        <h2>
          <span>显示</span>
        </h2>
        {/* 四档字号（agora-x70t.xsgz）：iOS Safari 不把系统字号设置给网页，这个旋钮就得自己给。
            默认“标准”= 17px（Apple HIG body，也与 iOS“不放大输入框”的 16px 门槛同侧）；“小”档
            把阅读字号降到 15px，但触控 44px 与输入框 16px 由 CSS 用 max(…) 钉成平台下限，不跟着缩。 */}
        <div className="mobile-text-sizes" role="group" aria-label="字号">
          {MOBILE_TEXT_SIZES.map((s) => (
            <button
              key={s.key}
              type="button"
              data-testid={`mobile-text-${s.key}`}
              aria-pressed={textSize === s.key}
              onClick={() => pickText(s.key)}
            >
              {s.label}
            </button>
          ))}
        </div>
        <p className="muted">字号只影响这台设备上的 /m；这一档存本机，换一台设备要重选。</p>
      </section>

      <section className="mobile-section" aria-label="诊断">
        <h2>
          <span>诊断</span>
        </h2>
        {/* 排障用（agora-x70t）：布局出问题时不靠描述——描边标出出框（红）与被裁（黄）的元素，
            或者把整个视口的盒子数字复制出来。 */}
        <div className="mobile-diag-actions">
          <button
            type="button"
            data-testid="mobile-diag-outline"
            aria-pressed={outlined}
            onClick={() => {
              const on = !outlined;
              setOutlined(on);
              toggleOutline();
            }}
          >
            {outlined ? "隐藏边框" : "显示边框"}
          </button>
          <button
            type="button"
            data-testid="mobile-diag-report"
            onClick={() => {
              const text = clipboardReport();
              setDiagReport(text);
              void copyText(text).then((ok) => setDiagNote(ok ? "已复制，粘给我就行" : "复制被拒——长按下面的文字全选复制"));
            }}
          >
            复制布局报告
          </button>
        </div>
        {/* 前端构建号摆在设置页底：升级后一眼能看出手机跑的是不是新包（iOS 主屏 PWA 会把
            start_url 钉在缓存里，光看行为看不出来）。 */}
        <p className="muted" data-testid="mobile-build">
          前端 {webBuild()}
          {webVerdict?.kind === "stale" ? ` · 服务端 ${webVerdict.served}` : ""} ·{" "}
          <button
            type="button"
            className="link"
            data-testid="mobile-reload"
            onClick={() => {
              // 硬刷新：iOS 主屏 PWA 从任务切换器回来常常是"恢复旧页面"（不重新导航），光
              // location.reload() 也可能落在同一份缓存条目上——所以先清 Cache Storage，再用
              // 带随机查询串的地址重新导航（同 scope，仍留在 PWA 里；下次冷启又回到 start_url）。
              void (async () => {
                try {
                  for (const key of await caches.keys()) await caches.delete(key);
                } catch {
                  /* 没有 caches（非安全上下文 / 单测）就算了 */
                }
                try {
                  location.replace(`/m?r=${Date.now()}`);
                } catch {
                  /* 单测里没有 location.replace */
                }
              })();
            }}
          >
            重新加载
          </button>
        </p>
        {webVerdict?.kind === "stale" && (
          // 只在服务端明确报了另一份构建号时才提示——宁可少提示，也不要让用户天天点"重新加载"。
          <p className="mobile-note warning" data-testid="mobile-stale-web">
            有新版本（服务端已是 {webVerdict.served}）：点上面的「重新加载」拿到新界面
          </p>
        )}
        {diagNote && <p className="muted" data-testid="mobile-diag-note">{diagNote}</p>}
        {diagReport && (
          <textarea className="mobile-diag-report" data-testid="mobile-diag-text" readOnly rows={8} value={diagReport} />
        )}
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
