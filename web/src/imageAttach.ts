/**
 * 手机 composer 的附图（agora-lmz2；MISSION §6.9；docs/spec/api.md「附图」）。
 *
 * 图不进 `input`：先逐张 `POST /api/sessions/:id/images`，节点把它落进会话工作目录、交回绝对路径，
 * 再把 `[image: <路径>]` 接在文字后面走原来的 text。agent 用自己的读文件工具看图——这是所有宿主
 * 都认的形态（2026-10-10 zuan 实测：消息里带工作目录内的路径，宿主直接读到并答对图里的颜色）。
 *
 * 大图在这里先缩：长边 ≤ 2000、转 JPEG。iPhone 截图原图 1–3 MB，经 peer 一跳转发有 5 s 总时限
 * （docs/spec/api.md「一跳转发」），缩完通常几百 KB；小于 1.5 MB 的常见格式原样发，不白白损画质。
 */

/** 一条消息最多带几张。 */
export const MAX_IMAGES = 4;
/** 原样发的上限：超过就缩。 */
export const PASSTHROUGH_BYTES = 1.5 * 1024 * 1024;
/** 缩图时的长边上限。 */
export const MAX_EDGE = 2000;

const PASSTHROUGH_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

/** 消息里引用一张图的写法；节点不解析它，只有 agent 读。 */
export function imageRef(path: string): string {
  return `[image: ${path}]`;
}

/** 交给 `input` 的那一串：文字在前，图的引用接在同一行末尾（只发图时就只有引用）。 */
export function composeWire(text: string, paths: string[]): string {
  const refs = paths.map(imageRef).join(" ");
  if (!refs) return text;
  return text ? `${text} ${refs}` : refs;
}

/**
 * 卡片上回显 `row.prompt` 时把图的引用换成一个短记号：路径对人没用，还会把气泡撑成一长串。
 * 只认 [`imageRef`] 写出的形态。
 */
export function displayPrompt(prompt: string): string {
  return prompt.replace(/\[image: [^\]\n]+\]/g, "［图片］");
}

/** 粘贴板里的图片文件（`files` 与 `items` 都看：iOS Safari 只在其中一处给）。 */
export function imagesFromClipboard(data: DataTransfer | null): File[] {
  if (!data) return [];
  const out: File[] = [];
  for (const file of Array.from(data.files ?? [])) {
    if (file.type.startsWith("image/")) out.push(file);
  }
  if (out.length === 0) {
    for (const item of Array.from(data.items ?? [])) {
      if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
      const file = item.getAsFile();
      if (file) out.push(file);
    }
  }
  return out;
}

/** 这张要不要先缩：格式不在四种常见格式里（HEIC 等），或者太大。 */
export function needsReencode(type: string, size: number): boolean {
  return !PASSTHROUGH_TYPES.has(type) || size > PASSTHROUGH_BYTES;
}

/** 等比缩到长边不超过 `max`；本来就小的不放大。 */
export function fitWithin(width: number, height: number, max = MAX_EDGE): { width: number; height: number } {
  const edge = Math.max(width, height);
  if (edge <= max) return { width, height };
  const scale = max / edge;
  return { width: Math.round(width * scale), height: Math.round(height * scale) };
}

function base64Of(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result ?? "");
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("读图失败"));
    reader.readAsDataURL(blob);
  });
}

function decode(blob: Blob): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("这张图解不开"));
    };
    img.src = url;
  });
}

async function reencode(blob: Blob): Promise<Blob> {
  const img = await decode(blob);
  const { width, height } = fitWithin(img.naturalWidth, img.naturalHeight);
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("浏览器不给画布");
  // JPEG 没有透明：先铺白底，透明 PNG 不会变成黑底。
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, width, height);
  ctx.drawImage(img, 0, 0, width, height);
  return new Promise((resolve, reject) =>
    canvas.toBlob((out) => (out ? resolve(out) : reject(new Error("转码失败"))), "image/jpeg", 0.85),
  );
}

/** 上传前的那一步：需要就缩，再转成 `images` 端点要的 base64。 */
export async function prepareImage(blob: Blob): Promise<string> {
  return base64Of(needsReencode(blob.type, blob.size) ? await reencode(blob) : blob);
}
