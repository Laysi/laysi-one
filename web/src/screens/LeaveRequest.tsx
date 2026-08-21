import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  trimDecimal,
  type MyLeaveList,
  type MyLeaveResult,
} from '../api/client';
import type { ScreenId } from '../nav';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
  onChanged: () => void;
}

export function LeaveRequest({ onToast, onGo, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyLeaveList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [result, setResult] = useState<MyLeaveResult | null>(null);

  const [kind, setKind] = useState('');
  const [fromOn, setFromOn] = useState('');
  const [toOn, setToOn] = useState('');
  const [days, setDays] = useState('');
  const [reason, setReason] = useState('');

  const load = useCallback(() => {
    setError(null);
    callOp<MyLeaveList>('list_my_leave_requests')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  const ready = kind !== '' && fromOn !== '' && toOn !== '' && days.trim() !== '';

  async function submit() {
    setBusy(true);
    setFormError(null);
    try {
      const d = await callOp<MyLeaveResult>(
        'request_leave',
        {
          kind,
          from_on: fromOn,
          to_on: toOn,

          days: days.trim(),
          ...(reason.trim() ? { reason: reason.trim() } : {}),
        },
        `申請${kind}，${fromOn} 至 ${toOn} 共 ${days.trim()} 日`,
      );
      setResult(d);
      onToast(
        `${d.request.leave_no} 已送出：${d.request.kind_label} ` +
          `${trimDecimal(d.request.days)} 日，待主管簽核`,
      );
      setKind('');
      setFromOn('');
      setToOn('');
      setDays('');
      setReason('');
      onChanged();
      load();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setFormError(msg);
      onToast(msg);
    } finally {
      setBusy(false);
    }
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">休假申請</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          你的帳號尚未對應員工編號，請管理員完成連結後再送出休假單。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const al = data.annual_leave;

  return (
    <div>
      <h1 className="page-title">休假申請</h1>
      <p className="page-lede">送出後為待簽核，由主管決定是否核准。</p>

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">{al.year} 年特休餘額</div>
          <div className="stat-card__value num">{trimDecimal(al.remaining)}</div>
          <div className="stat-card__note num">
            日 · 額度 {trimDecimal(al.entitlement)}、已休 {trimDecimal(al.taken)}
          </div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">待簽核</div>
          <div className="stat-card__value num">{data.pending_count}</div>
          <div className="stat-card__note num">張</div>
        </div>
      </div>

      <div className={result ? 'split-columns' : undefined}>
        <section>
          <h3 className="section-title">申請內容</h3>
          <div className="card form-card">
            <label className="field">
              假別
              <select className="input" value={kind} onChange={(e) => setKind(e.target.value)}>
                <option value="">請選擇</option>
                {data.leave_kinds.map((o) => (
                  <option key={o.kind} value={o.kind}>
                    {o.label}
                  </option>
                ))}
              </select>
            </label>

            <div className="field-row">
              <label className="field field--date">
                從
                <DateField
                  className="input input--date"
                  value={fromOn}
                  onChange={(v) => setFromOn(v)}
                />
              </label>
              <label className="field field--date">
                到
                <DateField
                  className="input input--date"
                  value={toOn}
                  onChange={(v) => setToOn(v)}
                />
              </label>
              <label className="field field--grow">
                天數
                <input
                  className="input"
                  placeholder="1 或 0.5"
                  value={days}
                  onChange={(e) => setDays(e.target.value)}
                />
              </label>
            </div>

            <label className="field">
              事由
              <input
                className="input"
                placeholder="選填"
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
            </label>

            <button className="btn" disabled={busy || !ready} onClick={() => void submit()}>
              {busy ? '送出中…' : '送出休假單'}
            </button>

            {formError && <p className="state-note state-note--error">{formError}</p>}

            <p className="form-note">
              請填寫天數，半日填 <span className="num">0.5</span>。
            </p>
          </div>
        </section>

        {result && (
          <section>
            <h3 className="section-title">送出結果</h3>
            <>
              <div className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">{result.request.leave_no}</h3>
                  <span className="doc-card__badge">{result.request.status_label}</span>
                </div>
                <div className="doc-row">
                  <span>{result.request.kind_label}</span>
                  <span className="num">{trimDecimal(result.request.days)} 日</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>期間</span>
                  <span className="num">
                    {result.request.from_on} 至 {result.request.to_on}
                  </span>
                </div>
                {result.request.reason && (
                  <div className="doc-row doc-row--sub">
                    <span>事由</span>
                    <span>{result.request.reason}</span>
                  </div>
                )}
              </div>
              <button
                className="btn btn--compact btn--secondary btn--spaced"
                onClick={() => onGo('meleave')}
              >
                查看我的休假 →
              </button>
            </>
          </section>
        )}
      </div>

      <p className="hint">
        特休額度依年資與 {al.ruleset.source_note} 即時計算，已休天數只計本年度已核准的特休。
      </p>
    </div>
  );
}
