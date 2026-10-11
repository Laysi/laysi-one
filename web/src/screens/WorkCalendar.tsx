import { useCallback, useEffect, useRef, useState } from 'react';
import { useCurrentUser } from '../auth';
import {
  businessToday,
  callOp,
  OpFailed,
  trimDecimal,
  workCalendarRefresh,
  workCalendarSet,
  workCalendarYear,
  type LeaveOverlap,
  type WorkCalendarDay,
  type WorkCalendarYear,
} from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  onToast: (msg: string) => void;
  reloadKey: number;
}

const MONTH_LABELS = [
  '一月', '二月', '三月', '四月', '五月', '六月',
  '七月', '八月', '九月', '十月', '十一月', '十二月',
];

const WEEK_HEAD = ['一', '二', '三', '四', '五', '六', '日'];

function offsetOf(day: WorkCalendarDay) {
  return WEEK_HEAD.indexOf(day.weekday);
}

export function WorkCalendar({ onGo, onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const canEdit = me.permissions.includes('set_work_calendar_day');
  const canRefresh = me.permissions.includes('refresh_work_calendar');
  const canRevoke = me.permissions.includes('revoke_leave_request');

  const thisYear = Number(businessToday().slice(0, 4));
  const [year, setYear] = useState(thisYear);
  const [data, setData] = useState<WorkCalendarYear | null>(null);
  const [picked, setPicked] = useState<WorkCalendarDay | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [overlaps, setOverlaps] = useState<LeaveOverlap[]>([]);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    callOp('list_work_calendar', { year }, undefined, workCalendarYear)
      .then((d) => live && setData(d))
      .catch(() => live && setData(null));
    return () => running.current?.();
  }, [year]);

  useEffect(load, [load, reloadKey]);

  const choose = (day: WorkCalendarDay) => {
    setPicked(day);
    setName(day.name ?? '');
  };

  const apply = async (kind: string) => {
    if (!picked) return;
    setBusy(true);
    try {
      const r = await callOp(
        'set_work_calendar_day',
        { on_date: picked.on_date, kind, name: kind ? name : null },
        undefined,
        workCalendarSet,
      );
      setOverlaps(r.leave_overlaps ?? []);
      onToast(`${picked.on_date} 已更新`);
      setPicked(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const refresh = async () => {
    setBusy(true);
    try {
      const r = await callOp('refresh_work_calendar', { year }, undefined, workCalendarRefresh);
      setOverlaps(r.leave_overlaps ?? []);
      onToast(`${r.source}：寫入 ${r.written} 天，保留自訂 ${r.kept} 天`);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <h1 className="page-title">工作日曆</h1>
      <div className="toolbar toolbar--spaced">
        <button className="btn btn--ghost btn--compact" onClick={() => setYear(year - 1)}>
          ← {year - 1}
        </button>
        <span className="section-title num">{year} 年</span>
        <button className="btn btn--ghost btn--compact" onClick={() => setYear(year + 1)}>
          {year + 1} →
        </button>
        {data && (
          <span className="hint num">
            工作日 {data.workdays} 天 · 國定假日 {data.holidays} 天
          </span>
        )}
        <span className="spacer" />
        {data && (
          <span className="cal-legend">
            {data.kinds.map((k) => (
              <span className="cal-legend__item" key={k.code}>
                <span className={`cal-legend__swatch cal-day--${k.code}`} />
                {k.label}
              </span>
            ))}
            <span className="cal-legend__item">
              <span className="cal-legend__swatch cal-day--stated" />
              已登記
            </span>
          </span>
        )}
        <span className="spacer" />
        {canRefresh && (
          <button className="btn btn--secondary" disabled={busy} onClick={() => void refresh()}>
            從政府網站更新
          </button>
        )}
      </div>

      {overlaps.length > 0 && (
        <section className="cal-overlaps">
          <div className="toolbar">
            <span className="section-title">與日曆不一致的休假單</span>
            <span className="spacer" />
            {canRevoke && (
              <button className="btn btn--ghost btn--compact" onClick={() => onGo('att')}>
                前往出勤與休假
              </button>
            )}
            <button className="btn btn--close" onClick={() => setOverlaps([])} aria-label="關閉">
              ×
            </button>
          </div>
          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>日期</th>
                  <th>員工</th>
                  <th>休假單</th>
                  <th>狀態</th>
                  <th className="align-right">本單計入</th>
                  <th className="align-right">依日曆</th>
                </tr>
              </thead>
              <tbody>
                {overlaps.map((o) => (
                  <tr key={`${o.on_date}${o.leave_no}`}>
                    <td className="num">{o.on_date}</td>
                    <td>
                      {o.name}
                      <span className="cell-hint cell-hint--block num">{o.emp_no}</span>
                    </td>
                    <td className="cell-nowrap">
                      <span className="tag tag--static tag--pale">{o.kind_label}</span>
                      <span className="cell-hint num">{o.leave_no}</span>
                    </td>
                    <td>{o.status_label}</td>
                    <td className="num align-right">{trimDecimal(o.hours)} 小時</td>
                    <td className="num align-right">{trimDecimal(o.calendar_hours)} 小時</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      {!data && <p className="state-note">讀取中…</p>}

      {data && (
        <div className="cal-year">
          {data.months.map((m) => (
            <section className="cal-month" key={m.month}>
              <h3 className="cal-month__title">{MONTH_LABELS[m.month - 1]}</h3>
              <div className="cal-grid">
                {WEEK_HEAD.map((w) => (
                  <span className="cal-head" key={w}>
                    {w}
                  </span>
                ))}
                {m.days[0] &&
                  Array.from({ length: offsetOf(m.days[0]) }, (_, i) => (
                    <span key={`pad${i}`} />
                  ))}
                {m.days.map((d) => (
                  <button
                    key={d.on_date}
                    className={`cal-day cal-day--${d.kind}${
                      picked?.on_date === d.on_date ? ' cal-day--on' : ''
                    }${d.source ? ' cal-day--stated' : ''}`}
                    title={d.name ? `${d.kind_label}：${d.name}` : d.kind_label}
                    onClick={() => choose(d)}
                  >
                    <span className="num">{Number(d.on_date.slice(8))}</span>
                  </button>
                ))}
              </div>
            </section>
          ))}
        </div>
      )}

      {picked && (
        <div className="overlay" onClick={() => setPicked(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title num">
                  {picked.on_date}（{picked.weekday}）
                </div>
                <div className="drawer__subtitle">
                  {picked.kind_label}
                  {picked.name ? `：${picked.name}` : ''}
                  {picked.source === 'government' && (
                    <span className="tag tag--pale tag--spaced">政府行事曆</span>
                  )}
                </div>
              </div>
              <button className="btn btn--close" onClick={() => setPicked(null)} aria-label="關閉">
                ×
              </button>
            </div>

            <div className="drawer__body">
              {canEdit ? (
                <>
                  <div className="field-grid">
                    <label className="field field--full">
                      名稱
                      <input
                        className="input"
                        value={name}
                        placeholder="中秋節、補行上班"
                        onChange={(e) => setName(e.target.value)}
                      />
                    </label>
                  </div>
                  <div className="choice-list">
                    {data?.kinds.map((k) => (
                      <button
                        key={k.code}
                        className={`choice${picked.kind === k.code ? ' choice--on' : ''}`}
                        disabled={busy}
                        onClick={() => void apply(k.code)}
                      >
                        <span className="choice__title">{k.label}</span>
                      </button>
                    ))}
                  </div>
                </>
              ) : (
                <p className="section-note">工作日曆由人資維護。</p>
              )}
            </div>

            <div className="drawer__foot">
              <button className="btn btn--ghost" onClick={() => setPicked(null)}>
                關閉
              </button>
              <span className="spacer" />
              {canEdit && !picked.default && (
                <button className="btn btn--ghost" disabled={busy} onClick={() => void apply('')}>
                  回到星期決定的日別
                </button>
              )}
            </div>
          </aside>
        </div>
      )}

    </div>
  );
}
