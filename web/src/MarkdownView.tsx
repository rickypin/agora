import { Fragment, useMemo, type ReactNode } from "react";
import { renderMarkdown, type MdInline, type MdList, type MdNode } from "./markdown";

/**
 * markdown.ts 的 AST → React 元素（A50，agora-4yr.2）。
 *
 * 这里**只**用 JSX 的 children 放文本：所有文本都经 React 转义，源里的 `<script>` 就是屏幕上
 * 的六个字符。整个前端一个 HTML 注入口都没有，由 tests/arch_boundary.rs::frontend_never_uses_innerhtml
 * 扫 web/src 全文（含注释）守着——那条守卫连关键词本身都不许出现，所以这段注释只能提测试名。
 *
 * 标题递降一级（源里的 `#` 渲染成 h2）：页面上 h1 只有一个（Header 的标题），agent 的回复
 * 是页面的一段内容，不该跟它抢层级；`######` 到底，压在 h6。
 */

const HEADING_TAG = ["h2", "h2", "h3", "h4", "h5", "h6", "h6"] as const;

function inlines(nodes: MdInline[]): ReactNode {
  return nodes.map((n, i) => {
    switch (n.kind) {
      case "text":
        return <Fragment key={i}>{n.text}</Fragment>;
      case "br":
        return <br key={i} />;
      case "code":
        return <code key={i}>{n.text}</code>;
      case "strong":
        return <strong key={i}>{inlines(n.children)}</strong>;
      case "em":
        return <em key={i}>{inlines(n.children)}</em>;
      case "link":
        // 协议已在 markdown.ts 白名单过（只有 http / https 走到这里）。新窗口打开 + noopener：
        // 回复里的链接是不可信文本，绝不能让它拿到 window.opener。
        return (
          <a key={i} href={n.href} target="_blank" rel="noopener noreferrer">
            {inlines(n.children)}
          </a>
        );
    }
  });
}

function list(node: MdList, key: number): ReactNode {
  const Tag = node.ordered ? "ol" : "ul";
  return (
    <Tag key={key}>
      {node.items.map((item, i) => (
        <li key={i}>
          {inlines(item.children)}
          {item.nested && list(item.nested, 0)}
        </li>
      ))}
    </Tag>
  );
}

function block(node: MdNode, key: number): ReactNode {
  switch (node.kind) {
    case "heading": {
      const Tag = HEADING_TAG[node.level];
      return <Tag key={key}>{inlines(node.children)}</Tag>;
    }
    case "paragraph":
      return <p key={key}>{inlines(node.children)}</p>;
    case "code":
      return (
        <pre key={key} data-lang={node.lang ?? undefined}>
          <code>{node.text}</code>
        </pre>
      );
    case "hr":
      return <hr key={key} />;
    case "quote":
      return <blockquote key={key}>{node.children.map(block)}</blockquote>;
    case "list":
      return list(node, key);
    case "table":
      return (
        <table key={key}>
          <thead>
            <tr>
              {node.head.map((cell, i) => (
                <th key={i}>{inlines(cell)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {node.rows.map((row, r) => (
              <tr key={r}>
                {row.map((cell, c) => (
                  <td key={c}>{inlines(cell)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      );
  }
}

/** 排版样式全部限定在 `.md` 作用域下（index.css），别处的 h2 / pre / table 不受影响。 */
export function MarkdownView({ text, className }: { text: string; className?: string }) {
  const nodes = useMemo(() => renderMarkdown(text), [text]);
  return <div className={className ? `md ${className}` : "md"}>{nodes.map(block)}</div>;
}
