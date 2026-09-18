import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessToday,
  callOp,
  myLeaveList,
  myLeaveResult,
  OpFailed,
  trimDecimal,
  wallSpan,
  type MyLeaveList,
  type MyLeaveResult,
} from '../api/client';
import type { ScreenId } from '../nav';
import { DateField } from '../components/DateField';
import { TimeField } from '../components/TimeField';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onToast: (msg: string) => void;
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
  onChanged: () => void;
}

const DAY_MS = 86400000;

function dayStart(day: string): number {
  const [y, m, d] = day.split('-').map(Number);
  return Date.UTC(y ?? 2000, (m ?? 1) - 1, d ?? 1);
}

function minutesOf(time: string): number {
  const [h, m] = time.split(':').map(Number);
  return (h ?? 0) * 60 + (m ?? 0);
}

function weekend(at: number): boolean {
  const day = new Date(at).getUTCDay();
  return day === 0 || day === 6;
}

function dayFlags(fromDay: string, toDay: string): { weekend: boolean; workday: boolean } {
  const flags = { weekend: false, workday: false };
  for (let at = dayStart(fromDay); at <= dayStart(toDay); at += DAY_MS) {
    if (weekend(at)) flags.weekend = true;
    else flags.workday = true;
  }
  return flags;
}

function nextWorkday(day: string): string {
  let at = dayStart(day);
  while (weekend(at)) at += DAY_MS;
  return new Date(at).toISOString().slice(0, 10);
}

function workingMinutes(
  fromDay: string,
  fromTime: string,
  toDay: string,
  toTime: string,
  start: string,
  end: string,
): number {
  const from = dayStart(fromDay) / 60000 + minutesOf(fromTime);
  const to = dayStart(toDay) / 60000 + minutesOf(toTime);
  let total = 0;
  for (let at = dayStart(fromDay); at <= dayStart(toDay); at += DAY_MS) {
    if (weekend(at)) continue;
    const open = Math.max(at / 60000 + minutesOf(start), from);
    const close = Math.min(at / 60000 + minutesOf(end), to);
    total += Math.max(0, close - open);
  }
  return total;
}

function round2(v: number): string {
  return trimDecimal((Math.round(v * 100) / 100).toFixed(2));
}

export function LeaveRequest({ onToast, onGo, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyLeaveList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [result, setResult] = useState<MyLeaveResult | null>(null);

  const [kind, setKind] = useState('');
  const [fromDay, setFromDay] = useState('');
  const [fromTime, setFromTime] = useState('');
  const [toDay, setToDay] = useState('');
  const [toTime, setToTime] = useState('');
  const [reason, setReason] = useState('');

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    callOp('list_my_leave_requests', {}, undefined, myLeaveList)
      .then((d) => {
        if (!live) return;
        setData(d);
        const today = nextWorkday(businessToday());
        setFromDay((v) => v || today);
        setToDay((v) => v || today);
        setFromTime((v) => v || d.schedule.work_start || '');
        setToTime((v) => v || d.schedule.work_end || '');
      })
      .catch((e) => live && setError(e instanceof OpFailed ? e.error.message : String(e)));
    return () => running.current?.();
  }, []);

  useEffect(load, [load, reloadKey]);

  if (error) {
    return (
      <div>
        <h1 className="page-title">休假申請</h1>
        <p className="state-note state-note--error">{error}</p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const al = data.annual_leave;
  const workStart = data.schedule.work_start;
  const workEnd = data.schedule.work_end;
  const filled =
    kind !== '' && fromDay !== '' && fromTime !== '' && toDay !== '' && toTime !== '';
  const ordered =
    filled &&
    dayStart(fromDay) / 60000 + minutesOf(fromTime) <
      dayStart(toDay) / 60000 + minutesOf(toTime);
  const minutes =
    ordered && workStart && workEnd
      ? workingMinutes(fromDay, fromTime, toDay, toTime, workStart, workEnd)
      : 0;
  const dailyMinutes = workStart && workEnd ? minutesOf(workEnd) - minutesOf(workStart) : 0;
  const hours = minutes / 60;
  const days = dailyMinutes > 0 ? minutes / dailyMinutes : 0;
  const span = ordered ? dayFlags(fromDay, toDay) : null;
  const ready = ordered && minutes > 0;
  const kindLabel = data.leave_kinds.find((o) => o.kind === kind)?.label ?? kind;

  async function submit() {
    setBusy(true);
    setFormError(null);
    try {
      const d = await callOp(
                        'request_leave',
                        {
            kind,
            from_at: `${fromDay}T${fromTime}:00`,
            to_at: `${toDay}T${toTime}:00`,
            ...(reason.trim() ? { reason: reason.trim() } : {}),
          },
                        `申請${kindLabel}，${fromDay} ${fromTime} 至 ${toDay} ${toTime}`,
                        myLeaveResult,
                      );
      setResult(d);
      onToast(
        `${d.request.leave_no} 已送出：${d.request.kind_label} ` +
          `${trimDecimal(d.request.hours)} 小時，待主管簽核`,
      );
      setKind('');
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
      <h1 className="page-title">休假申請</h1>

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
          {!workStart || !workEnd ? (
            <div className="card form-card">
              <p className="state-note">
                休假時數依上下班時間計算，請系統管理員在基礎設定填寫上班時間與下班時間。
              </p>
            </div>
          ) : (
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
                    value={fromDay}
                    onChange={(v) => setFromDay(v)}
                  />
                </label>
                <label className="field">
                  幾點
                  <TimeField label="開始時間" value={fromTime} onChange={setFromTime} />
                </label>
              </div>

              <div className="field-row">
                <label className="field field--date">
                  到
                  <DateField
                    className="input input--date"
                    value={toDay}
                    onChange={(v) => setToDay(v)}
                  />
                </label>
                <label className="field">
                  幾點
                  <TimeField label="結束時間" value={toTime} onChange={setToTime} />
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

              {filled && !ordered && (
                <p className="state-note state-note--error">結束時刻要晚於開始時刻。</p>
              )}
              {ordered && minutes === 0 && (
                <p className="state-note state-note--error">
                  {span && !span.workday
                    ? '所選日期都是週六或週日，請改選平日。'
                    : `所選時段落在上班時間之外，請改選 ${workStart} 至 ${workEnd} 之間的區間。`}
                </p>
              )}
              {ready && (
                <p className="form-note">
                  共 <span className="num">{round2(hours)}</span> 小時，等於{' '}
                  <span className="num">{round2(days)}</span> 日。
                  {span?.weekend && '週六與週日不計入。'}
                </p>
              )}

              <button className="btn" disabled={busy || !ready} onClick={() => void submit()}>
                {busy ? '送出中…' : '送出休假單'}
              </button>

              {formError && <p className="state-note state-note--error">{formError}</p>}
            </div>
          )}
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
                  <span className="num">
                    {trimDecimal(result.request.hours)} 小時 ·{' '}
                    {trimDecimal(result.request.days)} 日
                  </span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>期間</span>
                  <span className="num">
                    {wallSpan(result.request.from_at, result.request.to_at)}
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

      <RulesetNote items={[{ label: '特休', source: al.ruleset.source_note }]} />
    </div>
  );
}
