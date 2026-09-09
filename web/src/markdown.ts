/**
 * agent 回复的 markdown 子集渲染器（A50，agora-4yr.2；兑现 agora-03k）。
 *
 * 纯函数、零依赖，只负责"源文本 → 一棵小 AST"；React 元素由 MarkdownView.tsx 生成。
 * 分成两个文件是为了让解析能在 node 环境里直接测（markdown.test.ts 不用 jsdom）。
 *
 * **安全边界在这里**：AST 里只有纯文本与已经过白名单的 href，没有任何 HTML 片段——原始
 * HTML 标签（`<b>`、`<script>`）一律留在 text 节点里，由 React 的 children 自然转义、显示成
 * 字面量。链接只放行 `http://` / `https://` 前缀，其余（`javascript:`、`data:`、相对路径）把
 * 整段 `[text](url)` 原样当文本，人能看见自己点不了的是什么。前端一行 HTML 注入口都没有，
 * 由 tests/arch_boundary.rs::frontend_never_uses_innerhtml 守着（那条守卫扫的是 web/src 的
 * 全文、注释也算，所以这里连那个 React prop 的名字都不能写出来，只能提测试名）。
 *
 * 子集固定：标题、段落（单换行 = 硬折）、两级列表、围栏代码、引用、表格、水平线；行内
 * `code` / **bold** / *italic* / [link](url)。**不做**语法高亮、任务列表 checkbox、脚注、
 * 引用式链接、setext 标题、反斜杠转义。子集不够用时另立 issue，别让这里长成半个 CommonMark。
 */

export type MdInline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  /** 段落里的单个换行（agent 的回复常用硬换行，丢了就粘成一坨）。 */
  | { kind: "br" }
  | { kind: "strong"; children: MdInline[] }
  | { kind: "em"; children: MdInline[] }
  | { kind: "link"; href: string; children: MdInline[] };

export interface MdListItem {
  children: MdInline[];
  /** 第二级列表挂在第一级的项上；更深的缩进并进第二级（见 parseList）。 */
  nested: MdList | null;
}

export interface MdList {
  kind: "list";
  ordered: boolean;
  items: MdListItem[];
}

export type MdNode =
  /** level 是源里 `#` 的个数（1–6）；渲染成 h2–h6 由 MarkdownView 递降一级。 */
  | { kind: "heading"; level: number; children: MdInline[] }
  | { kind: "paragraph"; children: MdInline[] }
  /** closed = 源里有闭合围栏。截断的长回复常把围栏切成两半，见下面 parseFence 的注释。 */
  | { kind: "code"; lang: string | null; text: string; closed: boolean }
  | MdList
  | { kind: "quote"; children: MdNode[] }
  | { kind: "table"; head: MdInline[][]; rows: MdInline[][][] }
  | { kind: "hr" };

const FENCE_OPEN = /^ {0,3}(`{3,}|~{3,})[ \t]*([^\s`]*)[ \t]*$/;
const HEADING = /^ {0,3}(#{1,6})[ \t]+(.*)$/;
const HR = /^ {0,3}(-{3,}|\*{3,}|_{3,})[ \t]*$/;
const QUOTE = /^ {0,3}> ?(.*)$/;
const BULLET = /^([ \t]*)([-*+])[ \t]+(.*)$/;
const ORDERED = /^([ \t]*)(\d{1,9})[.)][ \t]+(.*)$/;
/** 表格分隔行的一格：`---`、`:--`、`--:`、`:-:`。 */
const SEP_CELL = /^:?-+:?$/;
/** `data-lang` 只放行标识符样的语言名——属性值经 React 设置本来就安全，限一层是为了别把
    信息串里的怪东西带进 DOM（``` bash; rm -rf / 这种）。 */
const LANG = /^[\w.+#-]{1,20}$/;
/** 链接白名单：只认 http / https 两个协议的绝对 URL（前缀白名单，不做 URL 解析）。 */
const SAFE_HREF = /^https?:\/\/\S/i;

function isWordChar(c: string | undefined): boolean {
  return c !== undefined && /\w/.test(c);
}

function isSpace(c: string | undefined): boolean {
  return c === undefined || /\s/.test(c);
}

/**
 * 找 `marker` 的闭合位置。规则跟 CommonMark 借了两条，都是为了别把普通文本切碎：
 * 闭合符前面不能是空白（`2 * 3 * 4` 不算斜体），`_` 的闭合符后面不能是词字符、开启符
 * 前面也不能是（`pending_decision` 不算斜体——agent 的回复里 snake_case 满地都是，
 * 2026-09-10 设计时的第一版没有这条，`respond_within_secs` 直接被吃掉一段）。
 */
function emphasisEnd(src: string, from: number, marker: string): number {
  let j = from;
  while (j < src.length) {
    const k = src.indexOf(marker, j);
    if (k < 0) return -1;
    if (k > from && !isSpace(src[k - 1]) && (marker !== "_" || !isWordChar(src[k + marker.length]))) {
      return k;
    }
    j = k + marker.length;
  }
  return -1;
}

/** 行内解析：`code` / **bold** / *italic* / _italic_ / [text](url)，其余原样进 text。 */
export function parseInline(src: string): MdInline[] {
  const out: MdInline[] = [];
  let buf = "";
  const flush = () => {
    if (buf) out.push({ kind: "text", text: buf });
    buf = "";
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === "`") {
      // 反引号按"同长度的串配对"找闭合（``a `b` c`` 这种嵌套写法在 agent 的回复里出现过）。
      let n = 0;
      while (src[i + n] === "`") n++;
      const fence = "`".repeat(n);
      const end = src.indexOf(fence, i + n);
      if (end > i + n && src[end + n] !== "`") {
        flush();
        let text = src.slice(i + n, end);
        if (text.length > 2 && text.startsWith(" ") && text.endsWith(" ")) text = text.slice(1, -1);
        out.push({ kind: "code", text });
        i = end + n;
        continue;
      }
    } else if (c === "[") {
      const link = matchLink(src, i);
      if (link) {
        flush();
        const href = link.url.trim();
        if (SAFE_HREF.test(href)) {
          out.push({ kind: "link", href, children: parseInline(link.text) });
        } else {
          // 协议不在白名单：整段原样当文本。不是"去掉链接留文字"——把 javascript: 那串
          // 也显示出来，人才看得见有人往回复里塞了什么。
          out.push({ kind: "text", text: src.slice(i, link.end) });
        }
        i = link.end;
        continue;
      }
    } else if (c === "*" || c === "_") {
      const strong = c === "*" && src.startsWith("**", i);
      const marker = strong ? "**" : c;
      const opens = !isSpace(src[i + marker.length]) && (c !== "_" || !isWordChar(src[i - 1]));
      const end = opens ? emphasisEnd(src, i + marker.length, marker) : -1;
      if (end > 0) {
        flush();
        const children = parseInline(src.slice(i + marker.length, end));
        out.push(strong ? { kind: "strong", children } : { kind: "em", children });
        i = end + marker.length;
        continue;
      }
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}

/** `[text](url)`，text 里允许一层方括号嵌套，url 里不允许空格与括号。 */
function matchLink(src: string, at: number): { text: string; url: string; end: number } | null {
  let depth = 0;
  let close = -1;
  for (let i = at; i < src.length; i++) {
    if (src[i] === "[") depth++;
    else if (src[i] === "]") {
      depth--;
      if (depth === 0) {
        close = i;
        break;
      }
    }
  }
  if (close < 0 || src[close + 1] !== "(") return null;
  const end = src.indexOf(")", close + 2);
  if (end < 0) return null;
  const url = src.slice(close + 2, end);
  if (/[\s()]/.test(url)) return null;
  return { text: src.slice(at + 1, close), url, end: end + 1 };
}

interface Item {
  indent: number;
  ordered: boolean;
  text: string;
}

function itemAt(line: string): Item | null {
  const m = BULLET.exec(line) ?? ORDERED.exec(line);
  if (!m) return null;
  return { indent: m[1].replace(/\t/g, "    ").length, ordered: /\d/.test(m[2]), text: m[3] };
}

function leadingSpaces(line: string): number {
  return (/^[ \t]*/.exec(line)?.[0] ?? "").replace(/\t/g, "    ").length;
}

/** `| a | b |` → ["a", "b"]；首尾的 `|` 可有可无。转义的 `\|` 不在子集里。 */
function cells(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) s = s.slice(0, -1);
  return s.split("|").map((c) => c.trim());
}

/**
 * 表格只在"表头行 + 下一行是同列数的分隔线"时成立。列数必须相等是防 `a | b` 后面跟一条
 * `---` 被当成单列表（那是水平线），也顺手兜住了截断：源文本被切在表头与分隔线之间时，
 * 这里不成立，表头行退回成一个普通段落、原样显示 `| a | b |`（见 markdown.test.ts
 * 「截断处落在表格中间」）。
 */
function isTableStart(lines: string[], i: number): boolean {
  const head = lines[i];
  const sep = lines[i + 1];
  if (head === undefined || sep === undefined || !head.includes("|")) return false;
  const sepCells = cells(sep);
  return (
    sepCells.length > 0 &&
    sepCells.every((c) => SEP_CELL.test(c)) &&
    sepCells.length === cells(head).length
  );
}

function isBlockStart(lines: string[], i: number): boolean {
  const l = lines[i];
  if (l === undefined) return true;
  if (l.trim() === "") return true;
  return (
    FENCE_OPEN.test(l) ||
    HEADING.test(l) ||
    HR.test(l) ||
    QUOTE.test(l) ||
    itemAt(l) !== null ||
    isTableStart(lines, i)
  );
}

/**
 * 围栏代码块。**没有闭合围栏就一路渲染到结尾**，而不是退回成普通段落——本任务的折叠按
 * 源文本的前 12 行截（agora-4yr.2 notes 的编排者定夺），截断处落在 ``` 中间是常态；退回段落
 * 会让折叠态突然变成一坨带 ``` 的原文，比"代码块少了下半截"难看得多，展开后又跳回代码块，
 * 视觉上还闪一下。closed 留在 AST 里，是给测试钉这个决定用的
 * （markdown.test.ts「截断处落在围栏中间」，2026-09-10）。
 */
function parseFence(lines: string[], start: number): { node: MdNode; next: number } {
  const m = FENCE_OPEN.exec(lines[start])!;
  const marker = m[1];
  const lang = LANG.test(m[2]) ? m[2] : null;
  const body: string[] = [];
  let i = start + 1;
  let closed = false;
  for (; i < lines.length; i++) {
    const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(lines[i]);
    if (close && close[1][0] === marker[0] && close[1].length >= marker.length) {
      closed = true;
      i++;
      break;
    }
    body.push(lines[i]);
  }
  return { node: { kind: "code", lang, text: body.join("\n"), closed }, next: i };
}

interface RawItem {
  text: string;
  nested: RawItem[] | null;
  nestedOrdered: boolean;
}

/**
 * 列表，**两级封顶**（描述里的子集）：缩进 ≥ 首项 + 2 空格算第二级，再深的并进第二级而不是
 * 开第三级。不是列表项、但缩进够的行算上一项的续行（markdown 的 lazy continuation），拼进
 * 同一项的文本里。
 */
function parseList(lines: string[], start: number): { node: MdList; next: number } {
  const first = itemAt(lines[start])!;
  const base = first.indent;
  const raw: RawItem[] = [];
  let cur: RawItem | null = null;
  let nested: RawItem | null = null;
  let i = start;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      const next = lines[i + 1];
      // 空行之后还是本列表的项就继续（松散列表），否则列表到此为止。
      if (next !== undefined && itemAt(next) !== null && leadingSpaces(next) >= base) {
        i++;
        continue;
      }
      break;
    }
    const it = itemAt(line);
    const indent = leadingSpaces(line);
    if (it !== null && indent >= base + 2 && cur !== null) {
      if (cur.nested === null) {
        cur.nested = [];
        cur.nestedOrdered = it.ordered;
      }
      nested = { text: it.text, nested: null, nestedOrdered: false };
      cur.nested.push(nested);
      i++;
      continue;
    }
    if (it !== null) {
      // 缩进比本列表还浅、或者标记从 `-` 换成了 `1.`（反过来同理）：那是**另一个**列表，
      // 交回上层重新开一个。少这一条，`- 一\n\n1. 甲` 会把有序项接在无序列表尾巴上
      // （2026-09-10 实测，markdown.test.ts「bullet and ordered lists nest two levels」）。
      if (indent < base || it.ordered !== first.ordered) break;
      cur = { text: it.text, nested: null, nestedOrdered: false };
      nested = null;
      raw.push(cur);
      i++;
      continue;
    }
    const target = nested ?? cur;
    if (indent >= base + 2 && target !== null) {
      target.text += " " + line.trim();
      i++;
      continue;
    }
    break;
  }
  const node: MdList = {
    kind: "list",
    ordered: first.ordered,
    items: raw.map((r) => ({
      children: parseInline(r.text),
      nested:
        r.nested === null
          ? null
          : {
              kind: "list",
              ordered: r.nestedOrdered,
              items: r.nested.map((n) => ({ children: parseInline(n.text), nested: null })),
            },
    })),
  };
  return { node, next: i };
}

function parseTable(lines: string[], start: number): { node: MdNode; next: number } {
  const head = cells(lines[start]).map(parseInline);
  const width = head.length;
  const rows: MdInline[][][] = [];
  let i = start + 2;
  for (; i < lines.length; i++) {
    const line = lines[i];
    if (line.trim() === "" || !line.includes("|")) break;
    const row = cells(line).map(parseInline);
    // 补齐 / 截齐到表头列数：渲染成 <table> 之后列数不齐会错位，源里少写一格是常事。
    while (row.length < width) row.push([]);
    rows.push(row.slice(0, width));
  }
  return { node: { kind: "table", head, rows }, next: i };
}

/** 源文本 → AST。空串与纯空白得到空数组（调用方据此决定占不占位）。 */
export function renderMarkdown(src: string): MdNode[] {
  return parseBlocks(src.replace(/\r\n?/g, "\n").split("\n"));
}

function parseBlocks(lines: string[]): MdNode[] {
  const out: MdNode[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      i++;
      continue;
    }
    if (FENCE_OPEN.test(line)) {
      const r = parseFence(lines, i);
      out.push(r.node);
      i = r.next;
      continue;
    }
    const h = HEADING.exec(line);
    if (h) {
      out.push({
        kind: "heading",
        level: h[1].length,
        children: parseInline(h[2].replace(/[ \t]+#+[ \t]*$/, "").trim()),
      });
      i++;
      continue;
    }
    if (HR.test(line)) {
      out.push({ kind: "hr" });
      i++;
      continue;
    }
    if (isTableStart(lines, i)) {
      const r = parseTable(lines, i);
      out.push(r.node);
      i = r.next;
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length) {
        const q = QUOTE.exec(lines[i]);
        if (!q) break;
        body.push(q[1]);
        i++;
      }
      out.push({ kind: "quote", children: parseBlocks(body) });
      continue;
    }
    if (itemAt(line) !== null) {
      const r = parseList(lines, i);
      out.push(r.node);
      i = r.next;
      continue;
    }
    // 段落：一直吃到下一个块开头；单换行在段落内保留成硬折（MdInline 的 br）。
    const children: MdInline[] = [];
    let firstLine = true;
    while (i < lines.length && (firstLine || !isBlockStart(lines, i))) {
      if (!firstLine) children.push({ kind: "br" });
      children.push(...parseInline(lines[i]));
      firstLine = false;
      i++;
    }
    out.push({ kind: "paragraph", children });
  }
  return out;
}
