#!/usr/bin/env bun
// The notice renderer's subset, checked without a browser.
//
// Markdown.tsx builds React elements rather than HTML, so this walks the
// element tree and flattens it to a tag outline plus its text. Nothing here
// needs react-dom: an element is a plain object.
//
//   bun scripts/markdown-test.ts

import { Markdown } from "../web/src/components/Markdown";

type Node = { type?: unknown; props?: { children?: unknown } } | string | number | null;

function outline(node: Node): string {
  if (node === null || node === undefined || node === false) return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(outline).join("");
  const el = node as { type?: unknown; props?: { children?: unknown; href?: string } };
  const inner = outline((el.props?.children ?? null) as Node);
  if (typeof el.type !== "string") return inner;
  if (el.type === "br") return "\n";
  const href = el.props?.href ? ` href=${el.props.href}` : "";
  return `<${el.type}${href}>${inner}</${el.type}>`;
}

function render(text: string): string {
  return outline(Markdown({ text }) as Node).replace(/^<div>|<\/div>$/g, "");
}

let failed = 0;
function is(name: string, got: string, want: string) {
  if (got === want) {
    console.log(`  ok   ${name}`);
    return;
  }
  failed += 1;
  console.log(`  FAIL ${name}\n         得到 ${got}\n         預期 ${want}`);
}

console.log("Markdown 子集");

is("段落", render("一句話。"), "<p>一句話。</p>");
is("段內換行保留", render("第一行\n第二行"), "<p>第一行\n第二行</p>");
is("空行分段", render("上段\n\n下段"), "<p>上段</p><p>下段</p>");
is("標題三層", render("# 一\n## 二\n### 三"), "<h3>一</h3><h4>二</h4><h5>三</h5>");
is("項目清單", render("- 甲\n- 乙"), "<ul><li>甲</li><li>乙</li></ul>");
is("編號清單", render("1. 甲\n2. 乙"), "<ol><li>甲</li><li>乙</li></ol>");
is("引述", render("> 條文"), "<blockquote>條文</blockquote>");
is("分隔線", render("---"), "<hr></hr>");
is("粗體與斜體", render("**濃**與*淡*"), "<p><strong>濃</strong>與<em>淡</em></p>");
is("行內程式碼", render("用 `E001` 編號"), "<p>用 <code>E001</code> 編號</p>");
is(
  "連結",
  render("見 [勞動部](https://www.mol.gov.tw/)"),
  "<p>見 <a href=https://www.mol.gov.tw/>勞動部</a></p>",
);

// The renderer emits elements, never HTML, so a tag in the source is text and
// javascript: never becomes an href. Both are the reason there is no
// sanitiser in front of it.
is(
  "原始 HTML 當成文字",
  render("<script>alert(1)</script>"),
  "<p><script>alert(1)</script></p>",
);
is(
  "javascript 連結不成為連結",
  render("[點我](javascript:alert(1))"),
  "<p>[點我](javascript:alert(1))</p>",
);

// A rule is a heading, a paragraph and a numbered list in one block; the
// parser has to switch between them without a blank line in between.
is(
  "混合區塊不必空行分隔",
  render("## 第一章\n本章規定如下。\n1. 甲\n2. 乙\n---"),
  "<h4>第一章</h4><p>本章規定如下。</p><ol><li>甲</li><li>乙</li></ol><hr></hr>",
);

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
