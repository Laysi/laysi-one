import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import {
  attendancePunchList,
  callOp,
  leaveRequestList,
  leaveRevocation,
  money,
  OpFailed,
  overtimeRequestList,
  overtimeRevocation,
  punchCorrectionList,
  trimDecimal,
  wallSpan,
  type AttendancePunchList,
  type LeaveRequestList,
  type LeaveRequestRow,
  type OvertimeRequestList,
  type OvertimeRequestRow,
  type PunchCorrectionList,
} from '../api/client';
import { CancelledHint } from '../components/CancelledHint';
import { DateField } from '../components/DateField';
import { DayMarks } from '../components/DayMarks';
import { PeriodPicker } from '../components/PeriodPicker';
import { PunchTrail } from '../components/PunchTrail';
import { useCurrentUser } from '../auth';
import { RulesetNote } from '../components/RulesetNote';
import { TakeBackModal } from '../components/TakeBackModal';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function validTime(v: string): boolean {
  const m = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(v.trim());
  if (!m) return false;
  const [h, min, sec] = [Number(m[1]), Number(m[2]), Number(m[3] ?? 0)];
  return h < 24 && min < 60 && sec < 60;
}

type Revoking =
  | { kind: 'leave'; row: LeaveRequestRow }
  | { kind: 'overtime'; row: OvertimeRequestRow };

function statusTag(status: string): string {
  if (status === 'rejected') return 'tag tag--bad tag--static';
  if (status === 'approved') return 'tag tag--static';
  if (status === 'withdrawn' || status === 'revoked') return 'tag tag--pale tag--static';
  return 'tag tag--warn tag--static';
}

export function Attendance({ onToast, reloadKey, onChanged }: Props) {
  const [leave, setLeave] = useState<LeaveRequestList | null>(null);
  const [punches, setPunches] = useState<AttendancePunchList | null>(null);
  const [fixes, setFixes] = useState<PunchCorrectionList | null>(null);
  const [fixDone, setFixDone] = useState(false);
  const [overtime, setOvertime] = useState<OvertimeRequestList | null>(null);
  const [otDone, setOtDone] = useState(false);
  const [otNote, setOtNote] = useState<Record<string, string>>({});
  const [fixNoteById, setFixNoteById] = useState<Record<number, string>>({});
  const [trailOn, setTrailOn] = useState<string | null>(null);
  const [month, setMonth] = useState<string | undefined>(undefined);
  const [thisMonth, setThisMonth] = useState<string | undefined>(undefined);
  const [picked, setPicked] = useState<string | null>(null);
  const [fixOn, setFixOn] = useState('');
  const [fixKind, setFixKind] = useState('in');
  const [fixAt, setFixAt] = useState('');
  const [fixNote, setFixNote] = useState('');
  const [includeDone, setIncludeDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<Record<string, string>>({});
  const [revoking, setRevoking] = useState<Revoking | null>(null);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    const who = picked ? { emp_no: picked } : {};
    callOp(
      'list_attendance_punches',
      { ...(month ? { month } : {}), ...who },
      undefined,
      attendancePunchList,
    )
      .then((p) =>
        callOp(
          'list_leave_requests',
          { include_done: includeDone, month: p.month, ...who },
          undefined,
          leaveRequestList,
        ).then((l) => {
          if (!live) return;
          setLeave(l);
          setPunches(p);
          if (!month) setThisMonth(p.month);
        }),
      )
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, [month, includeDone, picked]);

  const me = useCurrentUser();
  const canSeeFixes = me.permissions.includes('list_punch_corrections');
  const canDecideFixes = me.permissions.includes('decide_punch_correction');
  const canSeeOvertime = me.permissions.includes('list_overtime_requests');
  const canDecideOvertime = me.permissions.includes('decide_overtime_request');
  const canDecideLeave = me.permissions.includes('decide_leave_request');
  const canRevokeLeave = me.permissions.includes('revoke_leave_request');
  const canRevokeOvertime = me.permissions.includes('revoke_overtime_request');

  const runningOvertime = useRef<(() => void) | null>(null);

  const loadOvertime = useCallback(() => {
    runningOvertime.current?.();
    if (!canSeeOvertime) return;
    let live = true;
    runningOvertime.current = () => {
      live = false;
    };
    callOp(
      'list_overtime_requests',
      {
        ...(month ? { month } : {}),
        include_done: otDone,
      },
      undefined,
      overtimeRequestList,
    )
      .then((d) => live && setOvertime(d))
      .catch(() => live && setOvertime(null));
    return () => runningOvertime.current?.();
  }, [canSeeOvertime, month, otDone]);

  useEffect(loadOvertime, [loadOvertime, reloadKey]);

  async function decideOvertime(requestNo: string, approve: boolean) {
    setBusy(`ot-${requestNo}`);
    try {
      await callOp<unknown>(
        'decide_overtime_request',
        {
          request_no: requestNo,
          approve,
          ...(otNote[requestNo]?.trim() ? { note: otNote[requestNo].trim() } : {}),
        },
        approve ? '核准加班申請' : '駁回加班申請',
      );
      onToast(approve ? `${requestNo} 已核准，時數列入當期薪資` : `${requestNo} 已駁回`);
      setOtNote((n) => ({ ...n, [requestNo]: '' }));
      onChanged();
      loadOvertime();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  const runningFixes = useRef<(() => void) | null>(null);

  const loadFixes = useCallback(() => {
    runningFixes.current?.();
    if (!canSeeFixes) return;
    let live = true;
    runningFixes.current = () => {
      live = false;
    };
    callOp('list_punch_corrections', { include_done: true }, undefined, punchCorrectionList)
      .then((d) => live && setFixes(d))
      .catch(() => live && setFixes(null));
    return () => runningFixes.current?.();
  }, [canSeeFixes]);

  useEffect(loadFixes, [loadFixes, reloadKey]);

  async function decideFix(id: number, approve: boolean) {
    setBusy(`fix-${id}`);
    try {
      await callOp<unknown>(
        'decide_punch_correction',
        { id, approve, ...(fixNoteById[id]?.trim() ? { note: fixNoteById[id].trim() } : {}) },
        approve ? '核准補登申請' : '駁回補登申請',
      );
      onToast(approve ? '已核准，時間已寫進打卡紀錄' : '已駁回');
      setFixNoteById((n) => ({ ...n, [id]: '' }));
      onChanged();
      loadFixes();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  useEffect(load, [load, reloadKey]);

  async function correct() {
    if (!picked) return;
    setBusy('punch');
    try {
      await callOp<unknown>(
        'correct_punch',
        {
          emp_no: picked,
          punched_on: fixOn,
          kind: fixKind,
          at: fixAt.trim(),
          ...(fixNote.trim() ? { note: fixNote.trim() } : {}),
        },
        `補登 ${picked} ${fixOn} 的${fixKind === 'in' ? '上班' : '下班'}卡`,
      );
      onToast(`${picked} ${fixOn} 的${fixKind === 'in' ? '上班' : '下班'}卡已補登為 ${fixAt}`);
      setFixOn('');
      setFixAt('');
      setFixNote('');
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function decide(r: LeaveRequestRow, approve: boolean) {
    setBusy(r.leave_no);
    try {
      await callOp<unknown>(
        'decide_leave_request',
        {
          leave_no: r.leave_no,
          approve,
          ...(note[r.leave_no]?.trim() ? { note: note[r.leave_no]?.trim() } : {}),
        },
        `${approve ? '核准' : '駁回'} ${r.name} 的${r.kind_label}`,
      );
      onToast(
        approve
          ? `${r.leave_no} 已核准：${r.name} ${r.kind_label} ${trimDecimal(r.hours)} 小時`
          : `${r.leave_no} 已駁回，說明將顯示給 ${r.name}`,
      );
      setNote((n) => ({ ...n, [r.leave_no]: '' }));
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function revoke(target: Revoking, reason: string) {
    const no = target.kind === 'leave' ? target.row.leave_no : target.row.request_no;
    setBusy(`rv-${no}`);
    try {
      if (target.kind === 'leave') {
        await callOp(
          'revoke_leave_request',
          { leave_no: no, reason },
          `撤銷 ${target.row.name} 的${target.row.kind_label}`,
          leaveRevocation,
        );
        load();
      } else {
        await callOp(
          'revoke_overtime_request',
          { request_no: no, reason },
          `撤銷 ${target.row.name} 的加班申請`,
          overtimeRevocation,
        );
        loadOvertime();
      }
      onToast(`${no} 已撤銷`);
      setRevoking(null);
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  const all = fixes?.corrections ?? [];
  const fixRows = all.filter((c) => fixDone || c.status === 'submitted');
  const dayTrail = (date: string) =>
    all.filter((c) => c.emp_no === picked && c.punched_on === date);

  return (
    <div>
      <h1 className="page-title">出勤與休假</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!punches && !error && <p className="state-note">讀取中…</p>}

      {punches && leave && (
        <>
          <div className="toolbar">
            <PeriodPicker
              periods={thisMonth ? [...punches.months, thisMonth] : punches.months}
              value={punches.month}
              onChange={setMonth}
            />
            <span className="spacer" />
            <span className="section-note">{punches.rows.length} 人</span>
          </div>

          {punches && (
            <>
              <p className="section-note">
                {!punches.schedule.judges_lateness || !punches.schedule.counts_overtime
                  ? punches.schedule.note
                  : (punches.schedule.flexible
                      ? `彈性工時 ${punches.schedule.work_start}–${punches.schedule.latest_start} 開始，下班同步順延`
                      : `上班 ${punches.schedule.work_start}、下班 ${punches.schedule.work_end}`) +
                    (punches.schedule.break_start
                      ? `，休息 ${punches.schedule.break_start}–${punches.schedule.break_end}`
                      : '') +
                    (punches.schedule.grace_minutes > 0
                      ? `，遲到寬限 ${punches.schedule.grace_minutes} 分鐘。`
                      : '。')}
              </p>

              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>員工</th>
                      <th className="align-right">打滿天數</th>
                      <th className="align-right">時數</th>
                      <th className="align-right">遲到</th>
                      <th className="align-right">未打下班</th>
                      <th className="align-right">曠職</th>
                    </tr>
                  </thead>
                  <tbody>
                    {punches.rows.map((r) => (
                      <tr
                        key={r.emp_no}
                        className={r.summary.open_days > 0 ? 'row--pending' : undefined}
                      >
                        <td>
                          <button
                            className="link-cell"
                            onClick={() => setPicked(picked === r.emp_no ? null : r.emp_no)}
                          >
                            {r.name}
                          </button>
                          <span className="cell-hint cell-hint--block num">
                            {r.emp_no} · {r.dept_label ?? '未指定'}
                          </span>
                          {r.summary.manual_days > 0 && (
                            <span className="cell-hint cell-hint--block">
                              {r.summary.manual_days} 天含補登
                            </span>
                          )}
                          {(r.summary.over_monthly_limit || r.summary.over_limit_days > 0) && (
                            <span className="cell-hint cell-hint--block cell-hint--alarm">
                              {r.summary.over_monthly_limit
                                ? `本月加班 ${r.summary.overtime_hours} 小時，超過 ${r.summary.monthly_max_hours} 小時的上限`
                                : `${r.summary.over_limit_days} 天加班超過單日上限`}
                            </span>
                          )}
                        </td>
                        <td className="num align-right">{r.summary.complete_days}</td>
                        <td className="num align-right">{r.summary.worked_hours}</td>
                        <td className="num align-right">{r.summary.late_days ?? '—'}</td>
                        <td className="num align-right">{r.summary.open_days}</td>
                        <td className="num align-right">{r.summary.absent_days}</td>
                      </tr>
                    ))}
                    {punches.rows.length === 0 && (
                      <tr>
                        <td colSpan={6} className="cell-muted">
                          {punches.month} 沒有在職員工。
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>

              {picked && punches.days && (
                <div className="split-columns split-columns--wide-left">
                  <section>
                    <h3 className="section-title">
                      {punches.employee?.name} 的逐日出勤
                    </h3>
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
                          {punches.days.map((d) => {
                            const trail = dayTrail(d.date);
                            return (
                              <Fragment key={d.date}>
                                <tr
                                  className={
                                    d.open
                                      ? 'row--pending'
                                      : !d.works && !d.in_at && !d.out_at
                                        ? 'row--rest'
                                        : undefined
                                  }
                                >
                                  <td className="num cell-nowrap">
                                    {d.date.slice(5)}
                                    <span className="cell-hint">（{d.weekday}）</span>
                                    {d.day_name && (
                                      <span className="cell-hint cell-hint--block">
                                        {d.day_name}
                                      </span>
                                    )}
                                  </td>
                                  <td className="num align-right">{d.in_at ?? '—'}</td>
                                  <td className="num align-right">{d.out_at ?? '—'}</td>
                                  <td className="num align-right">{d.worked_hours ?? '—'}</td>
                                  <td>
                                    <DayMarks day={d} />
                                    {d.open && (
                                      <span className="tag tag--warn tag--static">未打下班</span>
                                    )}
                                    {d.late === true && (
                                      <span className="tag tag--bad tag--static">遲到</span>
                                    )}
                                    {d.over_overtime_limit && (
                                      <span className="tag tag--alarm tag--static">
                                        加班超過 {d.overtime_limit_hours} 小時
                                      </span>
                                    )}
                                    {trail.length > 0 ? (
                                      <button
                                        className="link-cell"
                                        onClick={() =>
                                          setTrailOn(trailOn === d.date ? null : d.date)
                                        }
                                      >
                                        {trailOn === d.date
                                          ? '收合補登紀錄'
                                          : `補登紀錄 ${trail.length} 筆`}
                                      </button>
                                    ) : (
                                      d.manual && (
                                        <span className="tag tag--pale tag--static">補登</span>
                                      )
                                    )}
                                    {d.note && (
                                      <span className="cell-hint cell-hint--block">{d.note}</span>
                                    )}
                                  </td>
                                </tr>
                                {trailOn === d.date && (
                                  <tr>
                                    <td colSpan={5}>
                                      <PunchTrail corrections={trail} />
                                    </td>
                                  </tr>
                                )}
                              </Fragment>
                            );
                          })}
                          {punches.days.length === 0 && (
                            <tr>
                              <td colSpan={5} className="cell-muted">
                                本月無紀錄。
                              </td>
                            </tr>
                          )}
                        </tbody>
                      </table>
                    </div>
                  </section>

                  <section>
                    <h3 className="section-title">補登打卡</h3>
                    <div className="card form-card">
                      <div className="field-row">
                        <label className="field field--date">
                          日期
                          <DateField
                            className="input input--date"
                            value={fixOn}
                            onChange={(v) => setFixOn(v)}
                          />
                        </label>
                        <label className="field field--date">
                          時間（24 小時制）
                          <input
                            className="input input--time num"
                            inputMode="numeric"
                            placeholder="09:00:00"
                            value={fixAt}
                            onChange={(e) => setFixAt(e.target.value)}
                          />
                        </label>
                      </div>
                      <label className="field">
                        上班或下班
                        <select
                          className="input"
                          value={fixKind}
                          onChange={(e) => setFixKind(e.target.value)}
                        >
                          <option value="in">上班</option>
                          <option value="out">下班</option>
                        </select>
                      </label>
                      <label className="field">
                        事由
                        <input
                          className="input"
                          placeholder="選填，例如忘記打卡"
                          value={fixNote}
                          onChange={(e) => setFixNote(e.target.value)}
                        />
                      </label>
                      <button
                        className="btn"
                        disabled={busy === 'punch' || fixOn === '' || !validTime(fixAt)}
                        onClick={() => void correct()}
                      >
                        {busy === 'punch' ? '補登中…' : `補登 ${punches.employee?.name ?? ''}`}
                      </button>

                      <p className="form-note">
                        補登會覆蓋同一天同一格已有的時間，並留下更正紀錄。
                      </p>
                    </div>
                  </section>
                </div>
              )}
            </>
          )}

          {fixes && (
            <>
              <h3 className="section-title section-title--spaced">補登紀錄</h3>
              <div className="toolbar">
                <button
                  className={`chip${fixDone ? ' chip--on' : ''}`}
                  onClick={() => setFixDone((v) => !v)}
                >
                  含已決定
                </button>
                <span className="spacer" />
                <span className="section-note">{fixes.waiting_count} 張待審核</span>
              </div>
              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>員工</th>
                      <th>日期</th>
                      <th>格</th>
                      <th className="align-right">時間</th>
                      <th>事由</th>
                      <th>決定</th>
                    </tr>
                  </thead>
                  <tbody>
                    {fixRows.map((c) => (
                      <tr
                        key={c.id}
                        className={c.status === 'submitted' ? 'row--pending' : undefined}
                      >
                        <td>
                          {c.name}
                          <span className="cell-hint cell-hint--block num">{c.emp_no}</span>
                        </td>
                        <td className="num cell-nowrap">{c.punched_on}</td>
                        <td>{c.kind_label}</td>
                        <td className="num align-right">
                          {c.at}
                          {c.replaced_at && (
                            <span className="cell-hint cell-hint--block num">
                              原 {c.replaced_at}
                            </span>
                          )}
                        </td>
                        <td className="cell-muted">{c.reason || '—'}</td>
                        <td>
                          {c.status !== 'submitted' ? (
                            <>
                              <span
                                className={
                                  c.status === 'rejected'
                                    ? 'tag tag--bad tag--static'
                                    : 'tag tag--static'
                                }
                              >
                                {c.hr_entered ? '人資補登' : c.status_label}
                              </span>
                              <span className="cell-hint cell-hint--block">
                                {c.decided_by || '自動通過'}
                                {c.decision_note ? ` · ${c.decision_note}` : ''}
                              </span>
                            </>
                          ) : !canDecideFixes ? (
                            <span className="cell-hint">由人資決定</span>
                          ) : c.can_decide === false ? (
                            <span className="cell-hint">{c.decide_blocked_reason}</span>
                          ) : (
                            <>
                              <input
                                className="entry-input"
                                placeholder="說明，選填"
                                value={fixNoteById[c.id] ?? ''}
                                onChange={(e) =>
                                  setFixNoteById((n) => ({ ...n, [c.id]: e.target.value }))
                                }
                              />
                              {c.can_approve === false && (
                                <span className="cell-hint cell-hint--block">
                                  {c.approve_blocked_reason}
                                </span>
                              )}
                              <div className="cell-actions">
                                {c.can_approve !== false && (
                                  <button
                                    className="btn btn--compact"
                                    disabled={busy === `fix-${c.id}`}
                                    onClick={() => void decideFix(c.id, true)}
                                  >
                                    核准
                                  </button>
                                )}
                                <button
                                  className="btn btn--compact btn--secondary"
                                  disabled={busy === `fix-${c.id}`}
                                  onClick={() => void decideFix(c.id, false)}
                                >
                                  駁回
                                </button>
                              </div>
                            </>
                          )}
                        </td>
                      </tr>
                    ))}
                    {fixRows.length === 0 && (
                      <tr>
                        <td colSpan={6} className="cell-muted">
                          {fixDone ? '沒有補登紀錄。' : '沒有待審核的補登申請。'}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
              <p className="hint">{fixes.policy_note}</p>
            </>
          )}

          {overtime && (
            <>
              <h3 className="section-title section-title--spaced">加班申請</h3>
              <div className="toolbar">
                <button
                  className={`chip${otDone ? ' chip--on' : ''}`}
                  onClick={() => setOtDone((v) => !v)}
                >
                  含已處理
                </button>
                <span className="spacer" />
                <span className="section-note">
                  {overtime.pending_count} 張待簽核
                  {overtime.other_month_pending > 0 &&
                    ` · 其他月份還有 ${overtime.other_month_pending} 張`}
                </span>
              </div>
              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>員工</th>
                      <th>加班日</th>
                      <th>期間</th>
                      <th className="align-right">時數</th>
                      <th className="align-right">加班費</th>
                      <th>決定</th>
                    </tr>
                  </thead>
                  <tbody>
                    {overtime.requests.map((r) => (
                      <tr
                        key={r.request_no}
                        className={r.status === 'pending' ? 'row--pending' : undefined}
                      >
                        <td>
                          {r.name}
                          <span className="cell-hint cell-hint--block num">
                            {r.request_no} · {r.emp_no}
                            {r.dept_label && ` · ${r.dept_label}`}
                          </span>
                          {r.reason && (
                            <span className="cell-hint cell-hint--block">{r.reason}</span>
                          )}
                        </td>
                        <td>{r.day_type_label}</td>
                        <td className="num cell-nowrap">{wallSpan(r.from_at, r.to_at)}</td>
                        <td className="num align-right">{trimDecimal(r.hours)}</td>
                        <td className="num align-right">
                          {r.pay ? (
                            money(r.pay)
                          ) : (
                            <span className="cell-hint">{r.pay_blocked_reason}</span>
                          )}
                        </td>
                        <td>
                          {r.status !== 'pending' ? (
                            <>
                              <span className={statusTag(r.status)}>{r.status_label}</span>
                              {r.decided_by && (
                                <span className="cell-hint cell-hint--block">
                                  {r.decided_by}
                                  {r.decision_note ? ` · ${r.decision_note}` : ''}
                                </span>
                              )}
                              {r.cancelled && <CancelledHint cancelled={r.cancelled} />}
                              {r.status === 'approved' && canRevokeOvertime && (
                                r.can_revoke ? (
                                  <button
                                    className="btn btn--compact btn--secondary"
                                    onClick={() => setRevoking({ kind: 'overtime', row: r })}
                                  >
                                    撤銷
                                  </button>
                                ) : (
                                  <span className="cell-hint cell-hint--block">
                                    {r.revoke_blocked_reason}
                                  </span>
                                )
                              )}
                            </>
                          ) : !canDecideOvertime ? (
                            <span className="cell-hint">由人資決定</span>
                          ) : r.can_decide === false ? (
                            <span className="cell-hint">{r.decide_blocked_reason}</span>
                          ) : (
                            <>
                              <input
                                className="entry-input"
                                placeholder="說明，選填"
                                value={otNote[r.request_no] ?? ''}
                                onChange={(e) =>
                                  setOtNote((n) => ({ ...n, [r.request_no]: e.target.value }))
                                }
                              />
                              {r.can_approve === false && (
                                <span className="cell-hint cell-hint--block">
                                  {r.approve_blocked_reason}
                                </span>
                              )}
                              <div className="cell-actions">
                                {r.can_approve !== false && (
                                  <button
                                    className="btn btn--compact"
                                    disabled={busy === `ot-${r.request_no}`}
                                    onClick={() => void decideOvertime(r.request_no, true)}
                                  >
                                    核准
                                  </button>
                                )}
                                <button
                                  className="btn btn--compact btn--secondary"
                                  disabled={busy === `ot-${r.request_no}`}
                                  onClick={() => void decideOvertime(r.request_no, false)}
                                >
                                  駁回
                                </button>
                              </div>
                            </>
                          )}
                        </td>
                      </tr>
                    ))}
                    {overtime.requests.length === 0 && (
                      <tr>
                        <td colSpan={6} className="cell-muted">
                          {otDone ? '本月沒有加班申請。' : '本月沒有待簽核的加班申請。'}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
              {overtime.month_totals.length > 0 && (
                <div className="table-scroll">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>本月已核准</th>
                        <th className="align-right">時數</th>
                        <th className="align-right">加班費</th>
                      </tr>
                    </thead>
                    <tbody>
                      {overtime.month_totals.map((t) => (
                        <tr key={t.emp_no}>
                          <td>
                            {t.name}
                            <span className="cell-hint cell-hint--block num">
                              {t.emp_no}
                              {t.dept_label && ` · ${t.dept_label}`}
                            </span>
                          </td>
                          <td className="num align-right">
                            {trimDecimal(t.hours)}
                            {t.over_ceiling && (
                              <span className="cell-hint cell-hint--block">
                                已超過每月 {trimDecimal(overtime.monthly_max_hours)} 小時
                              </span>
                            )}
                          </td>
                          <td className="num align-right">{money(t.pay)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
              <RulesetNote
                items={[{ label: '加班費', source: overtime.ruleset.source_note }]}
              />
            </>
          )}

          <h3 className="section-title section-title--spaced">
            休假單 · {punches.month}
            {picked && punches.employee && ` · ${punches.employee.name}`}
          </h3>
          <div className="toolbar">
            <button
              className={`chip${includeDone ? ' chip--on' : ''}`}
              onClick={() => setIncludeDone((v) => !v)}
            >
              含已處理
            </button>
            <span className="spacer" />
            <span className="section-note">
              {leave.pending_count} 張待簽核
              {leave.other_pending > 0 && ` · 未列出的還有 ${leave.other_pending} 張`}
            </span>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>休假單</th>
                  <th>假別</th>
                  <th>期間</th>
                  <th className="align-right">時數</th>
                  <th>狀態</th>
                  <th className="align-right">動作</th>
                </tr>
              </thead>
              <tbody>
                {leave.requests.map((r) => (
                  <tr key={r.leave_no} className={r.status === 'pending' ? 'row--pending' : undefined}>
                    <td>
                      {r.name}
                      <span className="cell-hint cell-hint--block num">
                        {r.leave_no} · {r.emp_no}
                        {r.dept_label && ` · ${r.dept_label}`}
                      </span>
                      {r.reason && <span className="cell-hint cell-hint--block">{r.reason}</span>}
                    </td>
                    <td>
                      {r.kind_label}
                      {r.kind === 'annual' && r.annual_leave && (
                        <span className="cell-hint cell-hint--block num">
                          特休餘額 {trimDecimal(r.annual_leave.remaining)} 日
                        </span>
                      )}
                    </td>
                    <td className="num cell-nowrap">{wallSpan(r.from_at, r.to_at)}</td>
                    <td className="num align-right">
                      {trimDecimal(r.hours)}
                      <span className="cell-hint cell-hint--block num">
                        {trimDecimal(r.days)} 日
                      </span>
                    </td>
                    <td>
                      <span className={statusTag(r.status)}>{r.status_label}</span>
                      {r.status !== 'pending' && r.decided_by && (
                        <span className="cell-hint cell-hint--block">
                          {r.decided_by}
                          {r.decision_note && ` · ${r.decision_note}`}
                        </span>
                      )}
                      {r.cancelled && <CancelledHint cancelled={r.cancelled} />}
                    </td>
                    <td className="align-right">
                      {r.status === 'pending' && !canDecideLeave ? (
                        <span className="cell-muted">由人資決定</span>
                      ) : r.status === 'pending' && !r.can_decide ? (
                        <span className="cell-muted cell-reason">{r.decide_blocked_reason}</span>
                      ) : r.status === 'pending' ? (
                        <>
                          {r.can_approve === false && (
                            <div>
                              <span className="cell-muted cell-reason">
                                {r.approve_blocked_reason}
                              </span>
                            </div>
                          )}
                          <input
                            className="entry-input entry-input--code"
                            placeholder="說明"
                            value={note[r.leave_no] ?? ''}
                            onChange={(e) =>
                              setNote((n) => ({ ...n, [r.leave_no]: e.target.value }))
                            }
                          />
                          {r.can_approve !== false && (
                            <button
                              className="btn btn--compact"
                              disabled={busy === r.leave_no}
                              onClick={() => void decide(r, true)}
                            >
                              {busy === r.leave_no ? '處理中…' : '核准'}
                            </button>
                          )}
                          <button
                            className="btn btn--compact btn--secondary"
                            disabled={busy === r.leave_no}
                            onClick={() => void decide(r, false)}
                          >
                            駁回
                          </button>
                        </>
                      ) : r.status === 'approved' && canRevokeLeave && r.can_revoke ? (
                        <button
                          className="btn btn--compact btn--secondary"
                          onClick={() => setRevoking({ kind: 'leave', row: r })}
                        >
                          撤銷
                        </button>
                      ) : r.status === 'approved' && canRevokeLeave ? (
                        <span className="cell-muted cell-reason">{r.revoke_blocked_reason}</span>
                      ) : r.status === 'approved' || r.status === 'rejected' ? (
                        <span className="cell-muted">已簽核</span>
                      ) : null}
                    </td>
                  </tr>
                ))}
                {leave.requests.length === 0 && (
                  <tr>
                    <td colSpan={6} className="cell-muted">
                      {includeDone ? '本月沒有休假單。' : '本月沒有待簽核的休假單。'}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          {revoking && (
            <TakeBackModal
              title={revoking.kind === 'leave' ? '撤銷休假單' : '撤銷加班申請'}
              subtitle={
                revoking.kind === 'leave'
                  ? `${revoking.row.leave_no} · ${revoking.row.name} · ${revoking.row.kind_label} · ${wallSpan(revoking.row.from_at, revoking.row.to_at)}`
                  : `${revoking.row.request_no} · ${revoking.row.name} · ${revoking.row.day_type_label} · ${wallSpan(revoking.row.from_at, revoking.row.to_at)}`
              }
              warning={
                revoking.kind === 'leave'
                  ? '撤銷後無法復原，不再計入特休與薪資。'
                  : '撤銷後無法復原，不再計入加班費。'
              }
              reasonLabel="撤銷理由（必填，申請人看得到）"
              reasonRequired
              confirmLabel="確認撤銷"
              busy={busy !== null}
              onConfirm={(reason) => void revoke(revoking, reason)}
              onCancel={() => setRevoking(null)}
            />
          )}

          <h3 className="section-title section-title--spaced">
            {leave.annual_leave_year} 年特休餘額
          </h3>
          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>員工</th>
                  <th>部門</th>
                  <th className="align-right">額度</th>
                  <th className="align-right">已休</th>
                  <th className="align-right">剩餘</th>
                </tr>
              </thead>
              <tbody>
                {leave.annual_leave_balances.map((b) => (
                  <tr key={b.emp_no}>
                    <td>
                      {b.name}
                      <span className="cell-hint num">{b.emp_no}</span>
                    </td>
                    <td className="cell-accounts">{b.dept_label ?? '未指定'}</td>
                    <td className="num align-right">{trimDecimal(b.entitlement)}</td>
                    <td className="num align-right">{trimDecimal(b.taken)}</td>
                    <td className="num align-right">
                      {b.remaining.startsWith('-') ? (
                        <span className="tag tag--bad tag--static">
                          超休 {trimDecimal(b.remaining.slice(1))} 日
                        </span>
                      ) : (
                        `${trimDecimal(b.remaining)} 日`
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <p className="hint">{leave.annual_leave_note}</p>
          <RulesetNote
            items={[
              {
                label: '特休',
                version: leave.annual_leave_ruleset.version,
                from: leave.annual_leave_ruleset.effective_from,
                source: leave.annual_leave_ruleset.source_note,
              },
            ]}
          />
        </>
      )}
    </div>
  );
}
