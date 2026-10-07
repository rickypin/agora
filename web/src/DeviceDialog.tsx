import { useCallback, useEffect, useState } from "react";
import { apiFetch } from "./net";
import { ConfirmDialog } from "./ConfirmDialog";

/** 一台已配对设备（`GET /api/auth/devices`）。 */
interface Device {
  id: string;
  name: string;
  paired_via: string;
  created_at: string;
  last_seen_at: string;
  revoked_at: string | null;
}

/** `POST /api/auth/pair/new` 的应答：链接 + 二维码矩阵（'0' 浅 / '1' 深）。 */
export interface PairLink {
  url: string;
  qr: { size: number; rows: string[] } | null;
}

/** 把服务端给的矩阵画成 SVG：矩阵是数据，SVG 由 React 元素拼出来，不走 HTML 字符串注入。 */
export function QrSvg({ qr, module = 4 }: { qr: { size: number; rows: string[] }; module?: number }) {
  const px = qr.size * module;
  return (
    <svg
      className="pair-qr"
      data-testid="pair-qr"
      width={px}
      height={px}
      viewBox={`0 0 ${px} ${px}`}
      role="img"
      aria-label="配对二维码"
    >
      <rect width={px} height={px} fill="#fff" />
      {qr.rows.flatMap((row, y) =>
        [...row].map((cell, x) =>
          cell === "1" ? (
            <rect key={`${x}-${y}`} x={x * module} y={y * module} width={module} height={module} fill="#000" />
          ) : null,
        ),
      )}
    </svg>
  );
}

/**
 * 设备管理（agora-thc.1）：配对新设备（QR，手机扫）+ 已配对设备列表与吊销。
 *
 * 为什么 QR 由服务端给矩阵而不是前端引一个 QR 库：链接的 origin 只有服务端知道
 * （server.public_url），画法两边只差一次遍历；少一个 npm 依赖、也少一份和 Rust 侧
 * 不一致的编码。
 */
export function DeviceDialog({ onClose }: { onClose: () => void }) {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [link, setLink] = useState<PairLink | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<Device | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(async () => {
    const resp = await apiFetch("/api/auth/devices");
    if (!resp.ok) {
      setError(`读设备列表失败（HTTP ${resp.status}）`);
      return;
    }
    setDevices((await resp.json()) as Device[]);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 配对新设备：每次点都重新铸造（一次性链接，旧的没用也作废）。
  async function mint() {
    setError(null);
    setLink(null);
    setCopied(false);
    const resp = await apiFetch("/api/auth/pair/new", { method: "POST" });
    if (!resp.ok) {
      setError(`铸造配对链接失败（HTTP ${resp.status}）`);
      return;
    }
    setLink((await resp.json()) as PairLink);
  }

  async function revoke(device: Device) {
    setRevoking(null);
    const resp = await apiFetch(`/api/auth/devices/${encodeURIComponent(device.id)}`, { method: "DELETE" });
    if (!resp.ok) {
      setError(`吊销失败（HTTP ${resp.status}）`);
      return;
    }
    await load();
  }

  return (
    <div className="overlay" role="presentation" onClick={onClose}>
      <div
        className="dialog wide devices"
        role="dialog"
        aria-modal="true"
        aria-labelledby="devices-title"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id="devices-title">设备</h2>
        <section className="device-pair">
          <button data-testid="pair-new" onClick={() => void mint()}>
            配对新设备
          </button>
          {link && (
            <div className="pair-link">
              {link.qr && <QrSvg qr={link.qr} />}
              <code data-testid="pair-url">{link.url}</code>
              <button
                onClick={() => {
                  void navigator.clipboard?.writeText(link.url).then(() => setCopied(true)).catch(() => setCopied(false));
                }}
              >
                {copied ? "已复制" : "复制链接"}
              </button>
              <p className="muted">
                手机相机扫码，或在手机浏览器打开链接；打开不自动跳转时，把链接粘到配对页。
              </p>
            </div>
          )}
        </section>
        <section className="device-list">
          {devices === null ? (
            <p className="muted">读取中…</p>
          ) : devices.length === 0 ? (
            <p className="muted">还没有已配对设备。</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>名称</th>
                  <th>方式</th>
                  <th>配对时间</th>
                  <th>最近使用</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => (
                  <tr key={d.id} data-testid={`device-${d.id}`}>
                    <td>{d.name}</td>
                    <td>{d.paired_via}</td>
                    <td className="muted">{d.created_at}</td>
                    <td className="muted">{d.last_seen_at}</td>
                    <td>
                      <button className="danger" data-testid={`revoke-${d.id}`} onClick={() => setRevoking(d)}>
                        吊销
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
        {error && (
          <p className="error" data-testid="devices-error">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button onClick={onClose}>关闭</button>
        </div>
      </div>
      {revoking && (
        <ConfirmDialog
          title="吊销这台设备？"
          body={`${revoking.name} 的凭据立即失效，该设备上的页面会回到配对页；它登记的推送订阅也随之停发。`}
          confirmLabel="吊销"
          onConfirm={() => void revoke(revoking)}
          onCancel={() => setRevoking(null)}
        />
      )}
    </div>
  );
}
