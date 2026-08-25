import { useCallback, useEffect, useState } from 'react';
import { DateField } from '../components/DateField';
import { Markdown, MARKDOWN_HINT } from '../components/Markdown';
import { NoticeHistory } from '../components/NoticeHistory';
import {
  businessDate,
  businessToday,
  callOp,
  OpFailed,
  type NoticeList,
  type NoticeRow,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  canManage: boolean;
}

interface Form {
  kind: string;
  title: string;
  body: string;
  effective_on: string;
  expires_on: string;
}

const BLANK: Form = {
  kind: 'announcement',
  title: '',
  body: '',
  effective_on: '',
  expires_on: '',
};

function statusTag(status: string): string {
  if (status === 'ended') return 'tag tag--pale tag--static';
  if (status === 'scheduled') return 'tag tag--warn tag--static';
  return 'tag tag--static';
}

function endLabel(kind: string): string {
  return kind === 'policy' ? '廢止' : '下架';
}

export function Notices({ onToast, reloadKey, canManage }: Props) {
  const [data, setData] = useState<NoticeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [includeEnded, setIncludeEnded] = useState(false);
  const [form, setForm] = useState<Form | null>(null);
  const [editing, setEditing] = useState<NoticeRow | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<Set<number>>(new Set());

  const load = useCallback(() => {
    setError(null);
    callOp<NoticeList>('list_notices', { include_ended: includeEnded })
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, [includeEnded]);

  useEffect(load, [load, reloadKey]);

  function openNew(kind: string) {
    setEditing(null);
    setForm({ ...BLANK, kind, effective_on: businessToday() });
  }

  function toggle(id: number) {
    setOpen((s) => {
      const next = new Set(s);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }

  function openEdit(n: NoticeRow) {
    setEditing(n);
    setForm({
      kind: n.kind,
      title: n.title,
      body: n.body,
      effective_on: n.effective_on,
      expires_on: n.expires_on ?? '',
    });
  }

  function setField(key: keyof Form, value: string) {
    setForm((f) => (f ? { ...f, [key]: value } : f));
  }

  async function save() {
    if (!form) return;
    setBusy(true);
    try {
      if (editing) {
        const input: Record<string, unknown> = { id: editing.id };
        if (form.title !== editing.title) input.title = form.title;
        if (form.body !== editing.body) input.body = form.body;
        if (form.effective_on !== editing.effective_on) input.effective_on = form.effective_on;
        if (form.expires_on !== (editing.expires_on ?? '')) input.expires_on = form.expires_on;
        const d = await callOp<{ revised: boolean; notice: NoticeRow }>(
          'update_notice',
          input,
          `更新${editing.kind_label} ${editing.title}`,
        );
        onToast(d.revised ? `已發布第 ${d.notice.version_no} 版` : '已更新');
      } else {
        await callOp(
          'create_notice',
          {
            kind: form.kind,
            title: form.title,
            body: form.body,
            effective_on: form.effective_on,
            ...(form.expires_on ? { expires_on: form.expires_on } : {}),
          },
          `發布 ${form.title}`,
        );
        onToast('已發布');
      }
      setForm(null);
      setEditing(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function takeDown(n: NoticeRow) {
    try {
      await callOp(
        'update_notice',
        { id: n.id, expires_on: businessToday() },
        `${endLabel(n.kind)} ${n.title}`,
      );
      onToast(`已${endLabel(n.kind)}`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  const kindLabel = (kind: string) => data?.kinds.find((k) => k.kind === kind)?.label ?? kind;
  const revising =
    editing !== null &&
    form !== null &&
    (form.title !== editing.title ||
      form.body !== editing.body ||
      form.effective_on !== editing.effective_on);

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
              {n.expires_on ? ` – ${n.expires_on}` : ' 起'} · 第 {n.version_no} 版 ·{' '}
              {n.published_by} 發布於 {businessDate(n.published_at)}
            </p>
          </span>
          <span className={statusTag(n.status)}>{n.status_label}</span>
        </button>
        {shown && (
          <div className="notice-card__panel">
            <div className="notice-card__body">
              <Markdown text={n.body} />
            </div>
            <div className="notice-card__foot">
              <span>
                <NoticeHistory notice={n} op="list_notices" />
              </span>
              {canManage && (
                <span className="notice-card__actions">
                  <button className="btn btn--compact btn--secondary" onClick={() => openEdit(n)}>
                    編輯
                  </button>
                  {n.status !== 'ended' && (
                    <button
                      className="btn btn--compact btn--secondary"
                      onClick={() => void takeDown(n)}
                    >
                      {endLabel(n.kind)}
                    </button>
                  )}
                </span>
              )}
            </div>
          </div>
        )}
      </div>
    );
  }

  const announcements = (data?.notices ?? []).filter((n) => n.kind === 'announcement');
  const policies = (data?.notices ?? []).filter((n) => n.kind === 'policy');

  return (
    <div>
      <h1 className="page-title">公告與規章</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="stat-grid stat-grid--two">
            <div className="card stat-card">
              <div className="stat-card__kicker">公告</div>
              <div className="stat-card__value num">{data.announcement_count}</div>
              <div className="stat-card__note num">則</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">規章</div>
              <div className="stat-card__value num">{data.policy_count}</div>
              <div className="stat-card__note num">則 · 每一版都留著供查閱</div>
            </div>
          </div>

          <div className="toolbar">
            <button
              className={`chip${includeEnded ? ' chip--on' : ''}`}
              onClick={() => setIncludeEnded((v) => !v)}
            >
              一併列出已下架與已廢止
            </button>
            <span className="spacer" />
            {canManage && !form && (
              <>
                <button className="btn btn--compact" onClick={() => openNew('announcement')}>
                  ＋ 發布公告
                </button>
                <button className="btn btn--compact" onClick={() => openNew('policy')}>
                  ＋ 發布規章
                </button>
              </>
            )}
          </div>

          {form && (
            <div className="card form-card">
              <h3 className="section-title">
                {editing ? `編輯${editing.kind_label}` : `發布${kindLabel(form.kind)}`}
              </h3>
              <div className="field-grid">
                <label className="field field--full">
                  標題
                  <input
                    className="input"
                    value={form.title}
                    onChange={(e) => setField('title', e.target.value)}
                  />
                </label>
                <label className="field field--full">
                  內容
                  <textarea
                    className="input"
                    rows={8}
                    value={form.body}
                    onChange={(e) => setField('body', e.target.value)}
                  />
                  <span className="field__hint">{MARKDOWN_HINT}</span>
                </label>
                <label className="field field--date">
                  {form.kind === 'policy' ? '生效日' : '發布日'}
                  <DateField
                    value={form.effective_on}
                    onChange={(v) => setField('effective_on', v)}
                  />
                </label>
                <label className="field field--date">
                  {endLabel(form.kind)}日
                  <DateField
                    value={form.expires_on}
                    onChange={(v) => setField('expires_on', v)}
                  />
                  <span className="field__hint">留空表示持續有效</span>
                </label>
              </div>

              {revising && editing && (
                <p className="form-note">
                  儲存後成為第 <span className="num">{editing.version_no + 1}</span> 版，
                  第 <span className="num">{editing.version_no}</span> 版留在修訂紀錄裡供查閱。
                </p>
              )}

              <div className="toolbar toolbar--spaced">
                <button
                  className="btn btn--compact btn--secondary"
                  onClick={() => {
                    setForm(null);
                    setEditing(null);
                  }}
                >
                  取消
                </button>
                <span className="spacer" />
                <button className="btn btn--compact" disabled={busy} onClick={() => void save()}>
                  {busy ? '儲存中…' : '儲存'}
                </button>
              </div>
            </div>
          )}

          <section>
            <h3 className="section-title section-title--spaced">公告</h3>
            {announcements.map(card)}
            {announcements.length === 0 && <p className="state-note">目前沒有公告。</p>}
          </section>

          <section>
            <h3 className="section-title section-title--spaced">規章制度</h3>
            {policies.map(card)}
            {policies.length === 0 && <p className="state-note">目前沒有規章。</p>}
          </section>
        </>
      )}
    </div>
  );
}
