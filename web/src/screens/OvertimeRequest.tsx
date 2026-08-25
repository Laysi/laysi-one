import { useCallback, useEffect, useState } from 'react';
import {
  businessToday,
  callOp,
  money,
  OpFailed,
  trimDecimal,
  wallSpan,
  type MyOvertimeList,
  type MyOvertimeResult,
} from '../api/client';
import type { ScreenId } from '../nav';
import { DateField } from '../components/DateField';
import { TimeField } from '../components/TimeField';

interface Props {
  onToast: (msg: string) => void;
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
  onChanged: () => void;
}

function stamp(day: string, time: string): number {
  const [y, m, d] = day.split('-').map(Number);
  const [hh, mm] = time.split(':').map(Number);
  return Date.UTC(y ?? 2000, (m ?? 1) - 1, d ?? 1) / 60000 + (hh ?? 0) * 60 + (mm ?? 0);
}

function restDay(day: string): boolean {
  const [y, m, d] = day.split('-').map(Number);
  const weekday = new Date(Date.UTC(y ?? 2000, (m ?? 1) - 1, d ?? 1)).getUTCDay();
  return weekday === 0 || weekday === 6;
}

function round2(v: number): string {
  return trimDecimal((Math.round(v * 100) / 100).toFixed(2));
}

export function OvertimeRequest({ onToast, onGo, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyOvertimeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [result, setResult] = useState<MyOvertimeResult | null>(null);

  const [fromDay, setFromDay] = useState('');
  const [fromTime, setFromTime] = useState('');
  const [toDay, setToDay] = useState('');
  const [toTime, setToTime] = useState('');
  const [dayType, setDayType] = useState('');
  const [reason, setReason] = useState('');

  const load = useCallback(() => {
    setError(null);
    callOp<MyOvertimeList>('list_my_overtime_requests')
      .then((d) => {
        setData(d);
        const today = businessToday();
        setFromDay((v) => v || today);
        setToDay((v) => v || today);
        setFromTime((v) => v || d.schedule.work_end || '18:00');
        setDayType((v) => v || (restDay(today) ? 'rest_day' : 'weekday'));
      })
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  if (error) {
    return (
      <div>
        <h1 className="page-title">加班申請</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">本帳號尚未對應員工編號，請管理員完成連結後再送出加班單。</p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const filled = fromDay !== '' && fromTime !== '' && toDay !== '' && toTime !== '';
  const ordered = filled && stamp(fromDay, fromTime) < stamp(toDay, toTime);
  const hours = ordered ? (stamp(toDay, toTime) - stamp(fromDay, fromTime)) / 60 : 0;
  const picked = data.day_types.find((t) => t.code === dayType);
  const dailyMax = picked ? Number(picked.max_hours) : 0;
  const overDaily = ordered && dailyMax > 0 && hours > dailyMax;
  const monthLeft = Number(data.month_hours_left);
  const overMonthly = ordered && fromDay.slice(0, 7) === data.month && hours > monthLeft;
  const ready = ordered && !overDaily && !overMonthly;

  async function submit() {
    setBusy(true);
    setFormError(null);
    try {
      const d = await callOp<MyOvertimeResult>(
        'submit_my_overtime_request',
        {
          from_at: `${fromDay}T${fromTime}:00`,
          to_at: `${toDay}T${toTime}:00`,
          day_type: dayType,
          ...(reason.trim() ? { reason: reason.trim() } : {}),
        },
        `申請加班，${fromDay} ${fromTime} 至 ${toDay} ${toTime}`,
      );
      setResult(d);
      onToast(
        `${d.request.request_no} 已送出：${d.request.day_type_label} ` +
          `${trimDecimal(d.request.hours)} 小時，待主管簽核`,
      );
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

  return (
    <div>
      <h1 className="page-title">加班申請</h1>
      <p className="page-lede">核准後才列入當期薪資的加班費。</p>

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">{data.month} 已核准加班</div>
          <div className="stat-card__value num">{trimDecimal(data.month_approved_hours)}</div>
          <div className="stat-card__note num">
            小時 · 本月上限 {trimDecimal(data.monthly_max_hours)}、尚可申請{' '}
            {trimDecimal(data.month_hours_left)}
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
            <div className="field-row field-row--when">
              <label className="field field--date">
                從
                <DateField
                  className="input input--date"
                  value={fromDay}
                  onChange={(v) => {
                    setFromDay(v);
                    if (toDay === '' || toDay < v) setToDay(v);
                    setDayType(restDay(v) ? 'rest_day' : 'weekday');
                  }}
                />
              </label>
              <TimeField label="開始時間" value={fromTime} onChange={setFromTime} />
            </div>

            <div className="field-row field-row--when">
              <label className="field field--date">
                到
                <DateField
                  className="input input--date"
                  value={toDay}
                  onChange={(v) => setToDay(v)}
                />
              </label>
              <TimeField label="結束時間" value={toTime} onChange={setToTime} />
            </div>

            <label className="field">
              加班日
              <select
                className="input"
                value={dayType}
                onChange={(e) => setDayType(e.target.value)}
              >
                {data.day_types.map((t) => (
                  <option key={t.code} value={t.code}>
                    {t.label}
                  </option>
                ))}
              </select>
            </label>

            <label className="field">
              事由
              <input
                className="input"
                placeholder="選填"
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
            </label>

            {filled && !ordered && (
              <p className="state-note state-note--error">結束時刻要晚於開始時刻。</p>
            )}
            {overDaily && picked && (
              <p className="state-note state-note--error">
                {picked.label}加班一天最多 {trimDecimal(picked.max_hours)} 小時，本次為{' '}
                {round2(hours)} 小時。
              </p>
            )}
            {overMonthly && (
              <p className="state-note state-note--error">
                {data.month} 尚可申請 {trimDecimal(data.month_hours_left)} 小時（含等簽核的單），
                本次為 {round2(hours)} 小時，請縮短時段。
              </p>
            )}
            {ready && (
              <p className="form-note">
                共 <span className="num">{round2(hours)}</span> 小時。
              </p>
            )}

            <button className="btn" disabled={busy || !ready} onClick={() => void submit()}>
              {busy ? '送出中…' : '送出加班單'}
            </button>

            {formError && <p className="state-note state-note--error">{formError}</p>}
          </div>
        </section>

        {result && (
          <section>
            <h3 className="section-title">送出結果</h3>
            <>
              <div className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">{result.request.request_no}</h3>
                  <span className="doc-card__badge">{result.request.status_label}</span>
                </div>
                <div className="doc-row">
                  <span>{result.request.day_type_label}</span>
                  <span className="num">{trimDecimal(result.request.hours)} 小時</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>期間</span>
                  <span className="num">
                    {wallSpan(result.request.from_at, result.request.to_at)}
                  </span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>加班費</span>
                  <span className="num">
                    {result.request.pay
                      ? money(result.request.pay)
                      : result.request.pay_blocked_reason}
                  </span>
                </div>
              </div>
              <button
                className="btn btn--compact btn--secondary btn--spaced"
                onClick={() => onGo('meot')}
              >
                查看我的加班 →
              </button>
            </>
          </section>
        )}
      </div>

      <p className="hint">
        加班費依 {data.ruleset.source_note} 與加班日適用的規則計算，核准後隨當期薪資發放。
      </p>
    </div>
  );
}
