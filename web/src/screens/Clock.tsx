import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  type MyPunchList,
  type PunchDay,
  type PunchReconciliation,
  type PunchResult,
  type WorkSchedule,
} from '../api/client';

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

function Reconciliation({ recon }: { recon: PunchReconciliation }) {
  if (!recon.imported) return <p className="recon">{recon.note}</p>;
  if (recon.matches) return <p className="recon">打卡推導的天數與匯入的出勤月統計一致。</p>;
  return (
    <p className="recon recon--off">
      與匯入的出勤月統計不一致：工作日 {recon.worked_days?.imported} /{' '}
      {recon.worked_days?.from_punches}（匯入 / 打卡）
      {recon.late_count?.from_punches != null && (
        <>
          ，遲到 {recon.late_count.imported} / {recon.late_count.from_punches}
        </>
      )}
      。{recon.note}
    </p>
  );
}

function ScheduleNote({ schedule }: { schedule: WorkSchedule }) {
  if (!schedule.judges_lateness || !schedule.counts_overtime) {
    return <p className="hint">{schedule.note}請負責人於「基礎設定」填寫上下班時間。</p>;
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
      {schedule.grace_minutes > 0 && (
        <>
          遲到寬限 <span className="num">{schedule.grace_minutes}</span> 分鐘。
        </>
      )}
    </p>
  );
}

function nowClock(): string {
  const d = new Date();
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}

export function Clock({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyPunchList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [note, setNote] = useState('');
  const [noteOpen, setNoteOpen] = useState(false);
  const [clock, setClock] = useState(nowClock);

  useEffect(() => {
    const id = window.setInterval(() => setClock(nowClock()), 1000);
    return () => window.clearInterval(id);
  }, []);

  const load = useCallback(() => {
    setError(null);
    callOp<MyPunchList>('list_my_punches')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

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
          你的帳號尚未對應員工編號，請管理員完成連結後再打卡。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const today = data.today_punches;
  const s = data.summary;

  return (
    <div>
      <h1 className="page-title">打卡</h1>

      <div className="card punch-card">
        <div className="punch-now">
          <div className="punch-now__label">現在</div>
          <div className="punch-now__clock num">{clock}</div>
          <div className="punch-now__date num">{data.today}</div>
        </div>

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

      {!data.next_kind && (
        <p className="hint">今天的上下班都已記錄；如需更正請洽人資補登。</p>
      )}

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">本月出勤</div>
          <div className="stat-card__value num">{s.complete_days}</div>
          <div className="stat-card__note num">日 · 共 {s.worked_hours} 小時</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">遲到</div>
          <div className="stat-card__value num">{s.late_days ?? '—'}</div>
          <div className="stat-card__note">
            {s.late_days == null ? '未設定上班時間' : `日 · 未打下班 ${s.open_days} 日`}
          </div>
        </div>
      </div>

      <ScheduleNote schedule={data.schedule} />

      <section>
        <h3 className="section-title section-title--spaced">{data.month} 逐日明細</h3>
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
                return (
                  <tr key={d.date} className={d.open ? 'row--pending' : undefined}>
                    <td className="num cell-nowrap">
                      {d.date.slice(5)}
                      <span className="cell-hint">（{d.weekday}）</span>
                    </td>
                    <td className="num align-right">{d.in_at ?? '—'}</td>
                    <td className="num align-right">{d.out_at ?? '—'}</td>
                    <td className="num align-right">{d.worked_hours ?? '—'}</td>
                    <td>
                      {tag && <span className={tag.className}>{tag.label}</span>}

                      {d.manual && <span className="tag tag--pale tag--static">補登</span>}
                      {d.note && <span className="cell-hint cell-hint--block">{d.note}</span>}
                    </td>
                  </tr>
                );
              })}
              {data.days.length === 0 && (
                <tr>
                  <td colSpan={5} className="cell-muted">
                    本月尚無打卡紀錄。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>

        <Reconciliation recon={data.reconciliation} />
        {s.overtime_hours != null && (
          <p className="recon recon--off">
            本月加班 {s.overtime_hours} 小時。{s.overtime_note}
          </p>
        )}
      </section>
    </div>
  );
}
