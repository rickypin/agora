import { apiFetch } from "./net";

/** `GET /api/health` 的公开子集（docs/spec/api.md）。 */
export interface PublicHealth {
  status: string;
}

export function isHealthy(h: unknown): h is PublicHealth {
  return typeof h === "object" && h !== null && (h as PublicHealth).status === "ok";
}

export async function fetchHealth(): Promise<boolean> {
  const resp = await apiFetch("/api/health");
  if (!resp.ok) return false;
  return isHealthy(await resp.json());
}
