import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import {
  businessClock,
  callOp,
  OpFailed,
  serverClockGapMs,
  type MyPunchCorrectionList,
  type MyPunchList,
  type PunchCorrection,
  type PunchCorrectionResult,
  type PunchDay,
  type PunchResult,
  type WorkSchedule,
} from '../api/client';
import { PunchTrail } from '../components/PunchTrail';
import { TimeField } from '../components/TimeField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function dayTag(day: PunchDay): { className: string; label: string } | null {
  if (day.open) return { className: 'tag tag--warn tag--static', label: '未打下班' };
  if (day.late === true) return { className: 'tag tag--bad tag--static', label: '遲到' };
  return null;
}

function ScheduleNote({ schedule }: { schedule: WorkSchedule }) {
  if (!schedule.judges_lateness || !schedule.counts_overtime) {
    return <p className="hint">{schedule.note}請系統管理員於「基礎設定」填寫上下班時間。</p>;
  }
  return (
    <p className="hint">
      {schedule.flexible ? (
        <>
          彈性工時：<span className="num">{schedule.work_start}</span> 到{' '}
          <span className="num">{schedule.latest_start}</span> 之間開始上班都算準時，
          下班時間同步順延，工作總時數不變。
        </>
      ) : (
        <>
          上班 <span className="num">{schedule.work_start}</span>、下班{' '}
          <span className="num">{schedule.work_end}</span>。
        </>
      )}
      {schedule.break_start && (
        <>
          休息 <span className="num">{schedule.break_start}</span> 到{' '}
          <span className="num">{schedule.break_end}</span>，不計工作時數。
        </>
      )}
      {schedule.grace_minutes > 0 && (
        <>
          遲到寬限 <span className="num">{schedule.grace_minutes}</span> 分鐘。
        </>
      )}
    </p>
  );
}

const CLOCK_GAP_WARN_MS = 5 * 60_000;

function gapLabel(ms: number): string {
  const minutes = Math.round(Math.abs(ms) / 60_000);
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours === 0) return `${minutes} 分`;
  return rest === 0 ? `${hours} 小時` : `${hours} 小時 ${rest} 分`;
}

function shiftMonth(month: string, by: number): string {
  const parts = month.split('-');
  const d = new Date(Number(parts[0]), Number(parts[1]) - 1 + by, 1);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
}

export function Clock({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyPunchList | null>(null);
  const [month, setMonth] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [note, setNote] = useState('');
  const [noteOpen, setNoteOpen] = useState(false);
  const [clock, setClock] = useState(businessClock);
  const clockGap = serverClockGapMs();

  useEffect(() => {
    const id = window.setInterval(() => setClock(businessClock()), 1000);
    return () => window.clearInterval(id);
  }, []);

  const load = useCallback(() => {
    setError(null);
    callOp<MyPunchList>('list_my_punches', month ? { month } : {})
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, [month]);

  const [fixes, setFixes] = useState<MyPunchCorrectionList | null>(null);
  const [editing, setEditing] = useState<{ date: string; kind: 'in' | 'out' } | null>(null);
  const [fixAt, setFixAt] = useState('');
  const [fixReason, setFixReason] = useState('');
  const [fixBusy, setFixBusy] = useState(false);
  const [trailOn, setTrailOn] = useState<string | null>(null);

  const loadFixes = useCallback(() => {
    callOp<MyPunchCorrectionList>('list_my_punch_corrections')
      .then(setFixes)
      .catch(() => setFixes(null));
  }, []);

  useEffect(loadFixes, [loadFixes, reloadKey]);
  useEffect(load, [load, reloadKey]);

  const bySlot = useMemo(() => {
    const map = new Map<string, PunchCorrection>();
    for (const c of fixes?.corrections ?? []) {
      const key = `${c.punched_on}|${c.kind}`;
      if (!map.has(key)) map.set(key, c);
    }
    return map;
  }, [fixes]);

  const byDay = useMemo(() => {
    const map = new Map<string, PunchCorrection[]>();
    for (const c of fixes?.corrections ?? []) {
      const day = map.get(c.punched_on);
      if (day) day.push(c);
      else map.set(c.punched_on, [c]);
    }
    return map;
  }, [fixes]);

  function startFix(date: string, kind: 'in' | 'out', at: string | null) {
    setEditing({ date, kind });
    setFixAt(at ? at.slice(0, 5) : '');
    setFixReason('');
    setFormError(null);
  }

  async function submitFix() {
    if (!editing) return;
    setFixBusy(true);
    setFormError(null);
    try {
      const d = await callOp<PunchCorrectionResult>(
        'submit_my_punch_correction',
        {
          punched_on: editing.date,
          kind: editing.kind,
          at: fixAt,
          ...(fixReason.trim() ? { reason: fixReason.trim() } : {}),
        },
        '送出補登申請',
      );
      onToast(
        d.punch_written
          ? `${d.correction.punched_on} 的${d.correction.kind_label}卡已補上 ${d.correction.at}`
          : `${d.correction.punched_on} 的${d.correction.kind_label}卡補登申請已送出，等人資決定`,
      );
      setEditing(null);
      setFixAt('');
      setFixReason('');
      onChanged();
      load();
      loadFixes();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setFormError(msg);
      onToast(msg);
    } finally {
      setFixBusy(false);
    }
  }

  async function punch(kind: 'in' | 'out') {
    setBusy(true);
    setFormError(null);
    try {
      const d = await callOp<PunchResult>(
        'punch_clock',
        { kind, ...(note.trim() ? { note: note.trim() } : {}) },
        kind === 'in' ? '打上班卡' : '打下班卡',
      );
      onToast(`${d.punch.kind_label}卡 ${d.punch.at} 已記錄`);
      setNote('');
      setNoteOpen(false);
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
        <h1 className="page-title">打卡</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          本帳號尚未對應員工編號，請管理員完成連結後再打卡。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const today = data.today_punches;
  const s = data.summary;
  const thisMonth = data.today.slice(0, 7);
  const monthLabel = data.month === thisMonth ? '本月' : data.month;
  const otherPending = (fixes?.corrections ?? []).filter(
    (c) => c.status === 'submitted' && !c.punched_on.startsWith(data.month),
  );
  const earliestPending = otherPending
    .map((c) => c.punched_on.slice(0, 7))
    .sort()
    .at(0);

  function slotCell(d: PunchDay, kind: 'in' | 'out') {
    const at = kind === 'in' ? d.in_at : d.out_at;
    const asked = bySlot.get(`${d.date}|${kind}`);
    if (asked?.status === 'submitted') {
      return (
        <>
          <span className="num">{at ?? '—'}</span>
          <span className="tag tag--warn tag--static">審核中 {asked.at.slice(0, 5)}</span>
        </>
      );
    }
    return (
      <>
        <button className="link-cell num" onClick={() => startFix(d.date, kind, at)}>
          {at ?? '—'}
        </button>
        {asked?.status === 'rejected' && (
          <span className="cell-hint cell-hint--block">
            補登已駁回{asked.decision_note ? ` · ${asked.decision_note}` : ''}
          </span>
        )}
      </>
    );
  }

  return (
    <div>
      <h1 className="page-title">打卡</h1>

      <div className="card punch-card">
        <div className="punch-now">
          <div className="punch-now__label">現在</div>
          <div className="punch-now__clock num">{clock}</div>
          <div className="punch-now__date num">{data.today}</div>
        </div>

        {Math.abs(clockGap) >= CLOCK_GAP_WARN_MS && (
          <p className="form-warn">
            伺服器時間與本機時間相差 <span className="num">{gapLabel(clockGap)}</span>
            。打卡記下的是伺服器時間，請系統管理員校正伺服器時鐘。
          </p>
        )}

        <div className="punch-actions">
          {data.next_kind ? (
            <button
              className="btn btn--punch"
              disabled={busy}
              onClick={() => void punch(data.next_kind as 'in' | 'out')}
            >
              {busy ? '記錄中…' : `${data.next_kind_label}打卡`}
            </button>
          ) : (
            <div className="punch-done">今日已完成</div>
          )}
        </div>

        {data.next_kind && (
          <div className="punch-note">
            {noteOpen ? (
              <>
                <input
                  className="input"
                  autoFocus
                  placeholder="例如外出洽公"
                  value={note}
                  onChange={(e) => setNote(e.target.value)}
                />
                <button
                  className="link-cell"
                  onClick={() => {
                    setNote('');
                    setNoteOpen(false);
                  }}
                >
                  不加說明
                </button>
              </>
            ) : (
              <button className="link-cell" onClick={() => setNoteOpen(true)}>
                加上說明
              </button>
            )}
          </div>
        )}

        <div className="punch-state">
          <div className="punch-state__item">
            <span className="punch-now__label">上班</span>
            <span className="punch-state__value num">{today?.in_at ?? '—'}</span>
            <span className="cell-hint">
              {today?.late === true ? '遲到' : today?.in_at ? '已打卡' : '尚未打卡'}
            </span>
          </div>
          <div className="punch-state__item">
            <span className="punch-now__label">下班</span>
            <span className="punch-state__value num">{today?.out_at ?? '—'}</span>
            <span className="cell-hint">
              {today?.worked_hours
                ? `本日 ${today.worked_hours} 小時`
                : today?.expected_end
                  ? `預計 ${today.expected_end}`
                  : '尚未打卡'}
            </span>
          </div>
        </div>
      </div>

      {formError && <p className="state-note state-note--error">{formError}</p>}

      {!data.next_kind && <p className="hint">今天的上下班都已記錄。</p>}

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">{monthLabel} 出勤</div>
          <div className="stat-card__value num">{s.complete_days}</div>
          <div className="stat-card__note num">日 · 共 {s.worked_hours} 小時</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">{monthLabel} 遲到</div>
          <div className="stat-card__value num">{s.late_days ?? '—'}</div>
          <div className="stat-card__note num">
            {s.late_days == null ? '未設定上班時間' : `日 · 未打下班 ${s.open_days} 日`}
          </div>
        </div>
      </div>

      <ScheduleNote schedule={data.schedule} />

      <section>
        <div className="toolbar">
          <h3 className="section-title section-title--spaced">{data.month} 逐日明細</h3>
          <span className="spacer" />
          <button
            className="btn btn--compact btn--secondary"
            onClick={() => {
              setEditing(null);
              setMonth(shiftMonth(data.month, -1));
            }}
          >
            上個月
          </button>
          <button
            className="btn btn--compact btn--secondary"
            disabled={data.month >= thisMonth}
            onClick={() => {
              setEditing(null);
              setMonth(shiftMonth(data.month, 1));
            }}
          >
            下個月
          </button>
        </div>
        <p className="section-note">
          點時間或空格可以補登或調整。
          {fixes?.auto_approve ? '自動通過，直接寫進紀錄。' : '送出後由人資審核。'}
        </p>
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th>日期</th>
                <th className="align-right">上班</th>
                <th className="align-right">下班</th>
                <th className="align-right">時數</th>
                <th>狀態</th>
              </tr>
            </thead>
            <tbody>
              {data.days.map((d) => {
                const tag = dayTag(d);
                const trail = byDay.get(d.date) ?? [];
                const rest = d.weekday === '六' || d.weekday === '日';
                const blank = !d.in_at && !d.out_at;
                return (
                  <Fragment key={d.date}>
                    <tr
                      className={
                        d.open ? 'row--pending' : rest && blank ? 'row--rest' : undefined
                      }
                    >
                      <td className="num cell-nowrap">
                        {d.date.slice(5)}
                        <span className="cell-hint">（{d.weekday}）</span>
                      </td>
                      <td className="num align-right">{slotCell(d, 'in')}</td>
                      <td className="num align-right">{slotCell(d, 'out')}</td>
                      <td className="num align-right">{d.worked_hours ?? '—'}</td>
                      <td>
                        {tag && <span className={tag.className}>{tag.label}</span>}
                        {d.over_overtime_limit && (
                          <span className="tag tag--bad tag--static">
                            加班超過 {d.overtime_limit_hours} 小時
                          </span>
                        )}

                        {trail.length > 0 ? (
                          <button
                            className="link-cell"
                            onClick={() => setTrailOn(trailOn === d.date ? null : d.date)}
                          >
                            {trailOn === d.date
                              ? '收合補登紀錄'
                              : `補登紀錄 ${trail.length} 筆`}
                          </button>
                        ) : (
                          d.manual && <span className="tag tag--pale tag--static">補登</span>
                        )}
                        {d.note && <span className="cell-hint cell-hint--block">{d.note}</span>}
                      </td>
                    </tr>
                    {trailOn === d.date && (
                      <tr>
                        <td colSpan={5}>
                          <PunchTrail corrections={trail} />
                        </td>
                      </tr>
                    )}
                    {editing?.date === d.date && (
                      <tr className="row--pending">
                        <td colSpan={5}>
                          <div className="punch-fix">
                            <span className="punch-fix__slot num">
                              {d.date.slice(5)} {editing.kind === 'in' ? '上班' : '下班'}
                            </span>
                            <TimeField label="補登時間" value={fixAt} onChange={setFixAt} />
                            <input
                              className="entry-input"
                              placeholder="事由，選填"
                              value={fixReason}
                              onChange={(e) => setFixReason(e.target.value)}
                            />
                            <button
                              className="btn btn--compact"
                              disabled={fixBusy || fixAt === ''}
                              onClick={() => void submitFix()}
                            >
                              {fixBusy ? '送出中…' : '送出'}
                            </button>
                            <button
                              className="btn btn--compact btn--secondary"
                              onClick={() => setEditing(null)}
                            >
                              取消
                            </button>
                          </div>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
              {data.days.length === 0 && (
                <tr>
                  <td colSpan={5} className="cell-muted">
                    {data.month} 沒有可補登的日子。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>

        {earliestPending && (
          <p className="section-note">
            其他月份還有 {otherPending.length} 張待審核的補登申請。
            <button
              className="link-cell"
              onClick={() => {
                setEditing(null);
                setMonth(earliestPending);
              }}
            >
              看 {earliestPending}
            </button>
          </p>
        )}

        {s.overtime_hours != null && (
          <p className="recon recon--off">
            本月加班 {s.overtime_hours} 小時。
            {s.over_monthly_limit && `已超過每月 ${s.monthly_max_hours} 小時的上限。`}
            {s.over_limit_days > 0 && `其中 ${s.over_limit_days} 天超過單日上限。`}
            {s.overtime_note}
          </p>
        )}
      </section>
    </div>
  );
}
