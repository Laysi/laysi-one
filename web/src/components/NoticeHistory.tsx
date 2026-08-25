import { useEffect, useState } from 'react';
import {
  businessDate,
  callOp,
  OpFailed,
  type NoticeHistory as History,
  type NoticeRow,
} from '../api/client';
import { Markdown } from './Markdown';
import type { OpName } from '../api/generated';

interface Props {
  notice: NoticeRow;
  op: OpName;
}

export function NoticeHistory({ notice, op }: Props) {
  const [open, setOpen] = useState(false);
  const [data, setData] = useState<History | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let live = true;
    setError(null);
    callOp<History>(op, { id: notice.id })
      .then((d) => live && setData(d))
      .catch((e) => live && setError(e instanceof OpFailed ? e.error.message : String(e)));
    return () => {
      live = false;
    };
  }, [open, op, notice.id]);

  if (notice.versions < 2) return null;

  return (
    <>
      <button className="btn btn--ghost btn--compact" onClick={() => setOpen((v) => !v)}>
        {open ? '收合修訂紀錄' : `修訂紀錄 ${notice.versions} 版`}
      </button>
      {open && (
        <div className="notice-history">
          {error && <p className="state-note state-note--error">{error}</p>}
          {!data && !error && <p className="state-note">讀取中…</p>}
          {data?.versions.map((v) => (
            <div className="notice-history__item" key={v.version_no}>
              <p className="notice-history__head num">
                第 {v.version_no} 版 · {v.effective_on} 起 · {v.published_by} 發布於{' '}
                {businessDate(v.published_at)}
                {v.version_no === notice.version_no && ' · 現行'}
              </p>
              {v.title !== notice.title && (
                <p className="notice-history__title">標題：{v.title}</p>
              )}
              <div className="notice-card__body">
                <Markdown text={v.body} />
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
