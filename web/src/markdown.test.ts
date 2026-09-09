// @vitest-environment jsdom
import { createElement } from "react";
import { cleanup, render } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { MarkdownView } from "./MarkdownView";
import { renderMarkdown, type MdInline, type MdNode } from "./markdown";

afterEach(cleanup);

/** 渲染成真 DOM，返回 `.md` 容器：AST 说不清的事（标签名、属性、转义）在这里断。 */
function dom(text: string): HTMLElement {
  const { container } = render(createElement(MarkdownView, { text }));
  return container.querySelector(".md") as HTMLElement;
}

/** 把 inline 数组摊平成纯文本（不含 code / link 的结构，只看字面量）。 */
function flat(nodes: MdInline[]): string {
  return nodes
    .map((n) => {
      switch (n.kind) {
        case "text":
        case "code":
          return n.text;
        case "br":
          return "\n";
        default:
          return flat(n.children);
      }
    })
    .join("");
}

function kinds(nodes: MdNode[]): string[] {
  return nodes.map((n) => n.kind);
}

it("ATX headings drop one level: # becomes h2 and ###### bottoms out at h6", () => {
  // 页面上 h1 只有一个（Header），agent 的回复是页面里的一段内容，不跟它抢层级。
  const md = dom("# 一\n\n## 二\n\n###### 六");
  expect([...md.children].map((e) => e.tagName)).toEqual(["H2", "H3", "H6"]);
  expect(md.querySelector("h3")!.textContent).toBe("二");
  // `#结论` 没有空格，不是标题。
  expect(dom("#结论").querySelector("h2")).toBeNull();
  expect(dom("#结论").querySelector("p")!.textContent).toBe("#结论");
});

it("a single newline inside a paragraph stays a hard break", () => {
  // agent 的回复常用硬换行分点，丢了就粘成一条。
  const nodes = renderMarkdown("第一句\n第二句");
  expect(kinds(nodes)).toEqual(["paragraph"]);
  expect(flat((nodes[0] as { children: MdInline[] }).children)).toBe("第一句\n第二句");
  expect(dom("第一句\n第二句").querySelectorAll("br").length).toBe(1);
  // 空行分段。
  expect(kinds(renderMarkdown("第一段\n\n第二段"))).toEqual(["paragraph", "paragraph"]);
});

it("bullet and ordered lists nest two levels", () => {
  const md = dom("- 一\n  - 一甲\n  - 一乙\n- 二\n\n1. 甲\n2. 乙");
  const ul = md.querySelector("ul")!;
  expect(ul.children.length).toBe(2);
  const inner = ul.querySelector("ul")!;
  expect([...inner.children].map((li) => li.textContent)).toEqual(["一甲", "一乙"]);
  const ol = md.querySelector("ol")!;
  expect([...ol.children].map((li) => li.textContent)).toEqual(["甲", "乙"]);
  // 三级并进二级（两级封顶），不开第三层 ul。
  expect(dom("- 一\n  - 二\n    - 三").querySelectorAll("ul").length).toBe(2);
  // 不是列表项、但缩进够的行是上一项的续行。
  expect(dom("- 一句话\n  接着说").querySelector("li")!.textContent).toBe("一句话 接着说");
});

it("a fenced block keeps its text and indentation verbatim, with the language on data-lang", () => {
  const src = "```python\ndef f():\n    return 1\n```";
  const nodes = renderMarkdown(src);
  expect(nodes[0]).toEqual({ kind: "code", lang: "python", text: "def f():\n    return 1", closed: true });
  const pre = dom(src).querySelector("pre")!;
  expect(pre.getAttribute("data-lang")).toBe("python");
  expect(pre.querySelector("code")!.textContent).toBe("def f():\n    return 1");
  // ~~~ 同样是围栏；围栏里的 # 与 - 不解析成标题 / 列表。
  expect(renderMarkdown("~~~\n# 不是标题\n- 不是列表\n~~~")[0]).toEqual({
    kind: "code",
    lang: null,
    text: "# 不是标题\n- 不是列表",
    closed: true,
  });
});

it("inline code, bold, italic and links all render as their own elements", () => {
  const md = dom("跑 `cargo test`，**必须**绿，*别* 忘了 _这条_，见 [文档](https://example.com/a)");
  expect(md.querySelector("code")!.textContent).toBe("cargo test");
  expect(md.querySelector("strong")!.textContent).toBe("必须");
  expect([...md.querySelectorAll("em")].map((e) => e.textContent)).toEqual(["别", "这条"]);
  const a = md.querySelector("a")!;
  expect(a.getAttribute("href")).toBe("https://example.com/a");
  expect(a.textContent).toBe("文档");
});

it("snake_case and a lone asterisk are not emphasis", () => {
  // 2026-09-10：第一版没有词边界规则，agent 回复里满地的 respond_within_secs / pending_decision
  // 被 `_` 切成斜体、中间一段消失。`2 * 3 * 4` 同理（开启符后面是空白就不算）。
  const md = dom("字段 pending_decision 与 respond_within_secs，算式 2 * 3 * 4");
  expect(md.querySelectorAll("em").length).toBe(0);
  expect(md.textContent).toContain("pending_decision 与 respond_within_secs");
  expect(md.textContent).toContain("2 * 3 * 4");
});

it("a blockquote renders its blocks inside", () => {
  const md = dom("> 引用第一行\n> ## 引用里的标题");
  const q = md.querySelector("blockquote")!;
  expect(q.querySelector("p")!.textContent).toBe("引用第一行");
  expect(q.querySelector("h3")!.textContent).toBe("引用里的标题");
});

it("a table needs a separator row of the same width, and its cells are parsed inline", () => {
  const md = dom("| 文件 | 状态 |\n| --- | --- |\n| `a.rs` | **改了** |\n| b.rs | 没动 |");
  const table = md.querySelector("table")!;
  expect([...table.querySelectorAll("th")].map((e) => e.textContent)).toEqual(["文件", "状态"]);
  expect(table.querySelectorAll("tbody tr").length).toBe(2);
  expect(table.querySelector("td code")!.textContent).toBe("a.rs");
  expect(table.querySelector("td strong")!.textContent).toBe("改了");
  // 分隔线列数对不上（`a | b` 后面跟一条水平线）就不是表。
  expect(dom("a | b\n---").querySelector("table")).toBeNull();
});

it("--- is a horizontal rule", () => {
  expect(kinds(renderMarkdown("上\n\n---\n\n下"))).toEqual(["paragraph", "hr", "paragraph"]);
  expect(dom("---").querySelectorAll("hr").length).toBe(1);
});

it("raw html is shown as text, not rendered", () => {
  // 回复是不可信文本：标签必须变成屏幕上的字符，而不是 DOM 节点。
  const md = dom('<b>粗</b> <script>alert(1)</script> <img src=x onerror=alert(1)>');
  expect(md.querySelector("b")).toBeNull();
  expect(md.querySelector("script")).toBeNull();
  expect(md.querySelector("img")).toBeNull();
  expect(md.textContent).toContain("<b>粗</b>");
  expect(md.textContent).toContain("<script>alert(1)</script>");
  expect(md.textContent).toContain("<img src=x onerror=alert(1)>");
});

it("javascript: links become plain text, https links get target and rel", () => {
  const bad = dom("点 [这里](javascript:alert(1)) 或 [那里](data:text/html,<script>) 或 [本地](/etc/passwd)");
  expect(bad.querySelectorAll("a").length).toBe(0);
  // 整段原样显示：人看得见有人往回复里塞了什么。
  expect(bad.textContent).toContain("[这里](javascript:alert(1))");
  expect(bad.textContent).toContain("[本地](/etc/passwd)");
  const good = dom("[a](https://example.com) [b](http://example.com/x?y=1)");
  const links = [...good.querySelectorAll("a")];
  expect(links.map((a) => a.getAttribute("href"))).toEqual(["https://example.com", "http://example.com/x?y=1"]);
  for (const a of links) {
    expect(a.getAttribute("target")).toBe("_blank");
    expect(a.getAttribute("rel")).toBe("noopener noreferrer");
  }
});

it("empty and whitespace-only input renders nothing", () => {
  for (const src of ["", "   ", "\n\n", " \n\t\n "]) {
    expect(renderMarkdown(src)).toEqual([]);
    expect(dom(src).childNodes.length).toBe(0);
  }
});

it("a 200-line reply keeps every line", () => {
  const lines = Array.from({ length: 200 }, (_, i) => `第 ${i + 1} 行`);
  const md = dom(lines.join("\n"));
  expect(md.querySelectorAll("br").length).toBe(199);
  const text = md.textContent!;
  for (const l of lines) expect(text).toContain(l);
});

it("a fence cut in half by the fold still renders as code, not as raw text", () => {
  // 折叠按源文本的前 12 行截（agora-4yr.2），截断处落在 ``` 中间是常态。没有闭合围栏就一路
  // 渲染到结尾：退回段落会让折叠态变成一坨带 ``` 的原文，展开后又跳回代码块，视觉上闪一下。
  const src = [...Array(10)].map((_, i) => `第 ${i + 1} 行`).join("\n") + "\n```rust\nlet a = 1;";
  const lines = src.split("\n");
  expect(lines.length).toBe(12);
  const nodes = renderMarkdown(src);
  expect(nodes[nodes.length - 1]).toEqual({ kind: "code", lang: "rust", text: "let a = 1;", closed: false });
  const md = dom(src);
  expect(md.querySelector("pre code")!.textContent).toBe("let a = 1;");
  expect(md.textContent).not.toContain("```");
});

it("a table cut in half by the fold falls back to a paragraph", () => {
  // 截在表头与分隔线之间：没有分隔线就不是表，表头行原样显示成一行文字（比半张没有边框的
  // 表格好认）。截在表体中间则仍是表，只是少几行。
  const head = [...Array(11)].map((_, i) => `第 ${i + 1} 行`).join("\n");
  const cutAtHead = dom(`${head}\n| 文件 | 状态 |`);
  expect(cutAtHead.querySelector("table")).toBeNull();
  expect(cutAtHead.textContent).toContain("| 文件 | 状态 |");
  const cutInBody = dom(`${head.split("\n").slice(0, 9).join("\n")}\n| 文件 | 状态 |\n| --- | --- |\n| a.rs | 改了 |`);
  expect(cutInBody.querySelectorAll("tbody tr").length).toBe(1);
  expect(cutInBody.querySelector("th")!.textContent).toBe("文件");
});
