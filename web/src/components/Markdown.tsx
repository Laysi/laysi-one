import { Fragment, createElement, type ReactNode } from 'react';

export const MARKDOWN_HINT =
  '支援 Markdown：# 標題、- 項目、1. 編號、> 引述、**粗體**、*斜體*、`程式碼`、--- 分隔線、[文字](網址)。空一行分段。';

const INLINE = /(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`|\[[^\]]+\]\(https?:\/\/[^)\s]+\))/g;
const LINK = /^\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)$/;
const HEADING = /^(#{1,3})\s+(.*)$/;
const BULLET = /^[-*]\s+/;
const NUMBER = /^\d+\.\s+/;
const QUOTE = /^>\s?/;
const RULE = /^---+\s*$/;
const BLOCK_START = /^(#{1,3}\s|[-*]\s|\d+\.\s|>|---+\s*$)/;
const HEADINGS = ['h3', 'h4', 'h5'] as const;

function inline(text: string, key: string): ReactNode[] {
  return text.split(INLINE).map((part, i) => {
    const at = `${key}-${i}`;
    if (part.startsWith('**') && part.endsWith('**') && part.length > 4) {
      return <strong key={at}>{part.slice(2, -2)}</strong>;
    }
    if (part.startsWith('`') && part.endsWith('`') && part.length > 2) {
      return <code key={at}>{part.slice(1, -1)}</code>;
    }
    const link = LINK.exec(part);
    if (link) {
      return (
        <a key={at} href={link[2]} target="_blank" rel="noreferrer noopener">
          {link[1]}
        </a>
      );
    }
    if (part.startsWith('*') && part.endsWith('*') && part.length > 2) {
      return <em key={at}>{part.slice(1, -1)}</em>;
    }
    return <Fragment key={at}>{part}</Fragment>;
  });
}

function lines(text: string, key: string): ReactNode[] {
  return text
    .split('\n')
    .flatMap((line, i) =>
      i === 0
        ? inline(line, `${key}-${i}`)
        : [<br key={`${key}-br-${i}`} />, ...inline(line, `${key}-${i}`)],
    );
}

export function Markdown({ text }: { text: string }) {
  const rows = text.replace(/\r\n/g, '\n').split('\n');
  const blocks: ReactNode[] = [];
  let i = 0;
  let n = 0;

  const row = () => rows[i] ?? '';

  while (i < rows.length) {
    const line = row();
    const at = `b${n++}`;

    if (line.trim() === '') {
      i += 1;
    } else if (RULE.test(line)) {
      blocks.push(<hr key={at} />);
      i += 1;
    } else if (HEADING.test(line)) {
      const parts = HEADING.exec(line);
      const level = (parts?.[1] ?? '#').length - 1;
      blocks.push(
        createElement(HEADINGS[level] ?? 'h5', { key: at }, inline(parts?.[2] ?? '', at)),
      );
      i += 1;
    } else if (BULLET.test(line) || NUMBER.test(line)) {
      const ordered = NUMBER.test(line);
      const mark = ordered ? NUMBER : BULLET;
      const items: string[] = [];
      while (i < rows.length && mark.test(row())) {
        items.push(row().replace(mark, ''));
        i += 1;
      }
      const children = items.map((item, k) => (
        <li key={`${at}-${k}`}>{inline(item, `${at}-${k}`)}</li>
      ));
      blocks.push(createElement(ordered ? 'ol' : 'ul', { key: at }, children));
    } else if (QUOTE.test(line)) {
      const quoted: string[] = [];
      while (i < rows.length && QUOTE.test(row())) {
        quoted.push(row().replace(QUOTE, ''));
        i += 1;
      }
      blocks.push(<blockquote key={at}>{lines(quoted.join('\n'), at)}</blockquote>);
    } else {
      const paragraph: string[] = [];
      while (i < rows.length && row().trim() !== '' && !BLOCK_START.test(row())) {
        paragraph.push(row());
        i += 1;
      }
      blocks.push(<p key={at}>{lines(paragraph.join('\n'), at)}</p>);
    }
  }

  return <div className="prose">{blocks}</div>;
}
