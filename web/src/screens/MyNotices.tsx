import { useCallback, useEffect, useState } from 'react';
import { Markdown } from '../components/Markdown';
import { NoticeHistory } from '../components/NoticeHistory';
import {
  businessDate,
  callOp,
  myNoticeList,
  OpFailed,
  type MyNoticeList,
  type NoticeRow,
} from '../api/client';

interface Props {
  reloadKey: number;
  onRead: () => void;
}

export function MyNotices({ reloadKey, onRead }: Props) {
  const [data, setData] = useState<MyNoticeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Set<number>>(new Set());

  const load = useCallback(() => {
    setError(null);
    callOp('list_my_notices', {}, undefined, myNoticeList)
      .then((d) => {
        setData(d);
        setOpen(new Set(d.notices.filter((n) => n.unread).map((n) => n.id)));
      })
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function markRead() {
    try {
      await callOp('mark_my_notices_read', {}, '標記公告與規章為已看過');
      setData((d) => (d ? { ...d, unread_count: 0, notices: d.notices.map((n) => ({ ...n, unread: false })) } : d));
      onRead();
    } catch (e) {
      setError(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  function toggle(id: number) {
    setOpen((s) => {
      const next = new Set(s);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">公告與規章</h1>
        <p className="state-note state-note--error">{error}</p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const announcements = data.notices.filter((n) => n.kind === 'announcement');
  const policies = data.notices.filter((n) => n.kind === 'policy');

  function card(n: NoticeRow) {
    const shown = open.has(n.id);
    return (
      <div className="card notice-card" key={n.id}>
        <button className="notice-card__head" onClick={() => toggle(n.id)}>
          <span className="notice-card__caret">{shown ? '▾' : '▸'}</span>
          <span className="notice-card__heading">
            <h3 className="notice-card__title">{n.title}</h3>
            <p className="notice-card__meta num">
              {n.effective_on}
              {n.expires_on ? ` – ${n.expires_on}` : ' 起'}
              {n.version_no > 1 &&
                ` · 第 ${n.version_no} 版 · ${businessDate(n.published_at)} 修訂`}
            </p>
          </span>
          {n.unread && (
            <span className="tag tag--warn tag--static">
              {n.version_no > 1 ? '已更新' : '新'}
            </span>
          )}
        </button>
        {shown && (
          <div className="notice-card__panel">
            <div className="notice-card__body">
              <Markdown text={n.body} />
            </div>
            {n.versions > 1 && (
              <div className="notice-card__foot">
                <NoticeHistory notice={n} op="list_my_notices" />
              </div>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div>
      <h1 className="page-title">公告與規章</h1>

      {data.unread_count > 0 && (
        <div className="toolbar">
          <span className="state-note">
            有 <span className="num">{data.unread_count}</span> 則還沒看過，已展開。
          </span>
          <span className="spacer" />
          <button className="btn btn--compact" onClick={() => void markRead()}>
            全部標為已看過
          </button>
        </div>
      )}

      <section>
        <h3 className="section-title">公告</h3>
        {announcements.map(card)}
        {announcements.length === 0 && <p className="state-note">目前沒有公告。</p>}
      </section>

      <section>
        <h3 className="section-title section-title--spaced">規章制度</h3>
        {policies.map(card)}
        {policies.length === 0 && <p className="state-note">目前沒有規章。</p>}
      </section>
    </div>
  );
}
