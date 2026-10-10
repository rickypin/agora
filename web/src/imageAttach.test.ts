// @vitest-environment jsdom
/** composer 附图的纯规则（agora-lmz2）：引用写法、回显、粘贴板取图、缩不缩。 */
import { describe, expect, it } from "vitest";
import { composeWire, displayPrompt, fitWithin, imageRef, imagesFromClipboard, needsReencode, PASSTHROUGH_BYTES, prepareImage } from "./imageAttach";

describe("imageAttach", () => {
  it("图的引用接在文字同一行末尾；只发图时只有引用；没图就是原文", () => {
    expect(imageRef("/w/a.png")).toBe("[image: /w/a.png]");
    expect(composeWire("看看", ["/w/a.png", "/w/b.jpg"])).toBe("看看 [image: /w/a.png] [image: /w/b.jpg]");
    expect(composeWire("", ["/w/a.png"])).toBe("[image: /w/a.png]");
    expect(composeWire("只有字", [])).toBe("只有字");
  });

  it("回显把引用说成［图片］，带空格的路径也认；别的方括号不动", () => {
    expect(displayPrompt("看 [image: /Users/me/My Repo/.agora-uploads/1-abcdef.png] 这里")).toBe("看 ［图片］ 这里");
    expect(displayPrompt("[x] 待办 [image] 不是引用")).toBe("[x] 待办 [image] 不是引用");
  });

  it("粘贴板：files 里的图优先，没有再看 items；非图片不要", () => {
    const shot = new File(["x"], "s.png", { type: "image/png" });
    const text = new File(["x"], "a.txt", { type: "text/plain" });
    const fromFiles = imagesFromClipboard({ files: [shot, text], items: [] } as unknown as DataTransfer);
    expect(fromFiles).toEqual([shot]);
    const item = { kind: "file", type: "image/jpeg", getAsFile: () => shot };
    const str = { kind: "string", type: "text/plain", getAsFile: () => null };
    expect(imagesFromClipboard({ files: [], items: [str, item] } as unknown as DataTransfer)).toEqual([shot]);
    expect(imagesFromClipboard(null)).toEqual([]);
  });

  it("四种常见格式且不大才原样发；HEIC / 大图要缩", () => {
    expect(needsReencode("image/png", 500_000)).toBe(false);
    expect(needsReencode("image/jpeg", PASSTHROUGH_BYTES)).toBe(false);
    expect(needsReencode("image/png", PASSTHROUGH_BYTES + 1)).toBe(true);
    expect(needsReencode("image/heic", 10)).toBe(true);
    expect(needsReencode("", 10)).toBe(true);
  });

  it("等比缩到长边 2000，小图不放大", () => {
    expect(fitWithin(1179, 2556)).toEqual({ width: 923, height: 2000 });
    expect(fitWithin(800, 600)).toEqual({ width: 800, height: 600 });
  });

  it("原样发的图：base64 就是文件字节", async () => {
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0, 255]);
    const b64 = await prepareImage(new File([bytes], "s.png", { type: "image/png" }));
    expect(b64).toBe(btoa(String.fromCharCode(...bytes)));
  });
});
