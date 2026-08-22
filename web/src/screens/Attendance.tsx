import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  trimDecimal,
  type AttendanceMonth,
  type AttendancePunchList,
  type LeaveRequestList,
  type LeaveRequestRow,
  type PunchCorrectionList,
  type PunchReconciliation,
} from '../api/client';
import { DateField } from '../components/DateField';
import { useCurrentUser } from '../auth';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function reconLabel(recon: PunchReconciliation): string {
  if (!recon.imported) return '無匯入資料';
  if (recon.matches) return '一致';
  const parts: string[] = [];
  if (recon.worked_days && recon.worked_days.difference !== '0') {
    parts.push(`工作日 ${recon.worked_days.imported} / ${recon.worked_days.from_punches}`);
  }
  if (recon.late_count?.difference != null && recon.late_count.difference !== 0) {
    parts.push(`遲到 ${recon.late_count.imported} / ${recon.late_count.from_punches}`);
  }
  return parts.length > 0 ? parts.join('、') : '一致';
}

function validTime(v: string): boolean {
  const m = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(v.trim());
  if (!m) return false;
  const [h, min, sec] = [Number(m[1]), Number(m[2]), Number(m[3] ?? 0)];
  return h < 24 && min < 60 && sec < 60;
}

function statusTag(r: LeaveRequestRow): string {
  if (r.status === 'rejected') return 'tag tag--bad tag--static';
  if (r.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function Attendance({ onToast, reloadKey, onChanged }: Props) {
  const [att, setAtt] = useState<AttendanceMonth | null>(null);
  const [leave, setLeave] = useState<LeaveRequestList | null>(null);
  const [punches, setPunches] = useState<AttendancePunchList | null>(null);
  const [fixes, setFixes] = useState<PunchCorrectionList | null>(null);
  const [fixDone, setFixDone] = useState(false);
  const [fixNoteById, setFixNoteById] = useState<Record<number, string>>({});
  const [month, setMonth] = useState<string | undefined>(undefined);
  const [picked, setPicked] = useState<string | null>(null);
  const [fixOn, setFixOn] = useState('');
  const [fixKind, setFixKind] = useState('in');
  const [fixAt, setFixAt] = useState('');
  const [fixNote, setFixNote] = useState('');
  const [includeDone, setIncludeDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<Record<string, string>>({});

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<AttendanceMonth>('get_attendance', month ? { month } : {}),
      callOp<LeaveRequestList>('list_leave_requests', { include_done: includeDone }),
      callOp<AttendancePunchList>('list_attendance_punches', {
        ...(month ? { month } : {}),
        ...(picked ? { emp_no: picked } : {}),
      }),
    ])
      .then(([a, l, p]) => {
        setAtt(a);
        setLeave(l);
        setPunches(p);
      })
      .catch((e: Error) => setError(e.message));
  }, [month, includeDone, picked]);

  const me = useCurrentUser();
  const canSeeFixes = me.permissions.includes('list_punch_corrections');
  const canDecideFixes = me.permissions.includes('decide_punch_correction');

  const loadFixes = useCallback(() => {
    if (!canSeeFixes) return;
    callOp<PunchCorrectionList>('list_punch_corrections', { include_done: fixDone })
      .then(setFixes)
      .catch(() => setFixes(null));
  }, [canSeeFixes, fixDone]);

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
          ? `${r.leave_no} 已核准：${r.name} ${r.kind_label} ${trimDecimal(r.days)} 日`
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

  return (
    <div>
      <h1 className="page-title">出勤與休假</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!att && !error && <p className="state-note">讀取中…</p>}

      {att && leave && (
        <>
          <div className="toolbar">
            {att.months.map((m) => (
              <button
                key={m}
                className={`chip${m === att.month ? ' chip--on' : ''}`}
                onClick={() => setMonth(m)}
              >
                {m}
              </button>
            ))}
            <span className="spacer" />
            <span className="section-note">{att.headcount} 人有出勤紀錄</span>
          </div>

          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">實際出勤</div>
              <div className="stat-card__value num">{trimDecimal(att.totals.worked_days)}</div>
              <div className="stat-card__note num">應出勤 {trimDecimal(att.totals.required_days)} 日</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">休假</div>
              <div className="stat-card__value num">{trimDecimal(att.totals.leave_days)}</div>
              <div className="stat-card__note num">日</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">加班時數</div>
              <div className="stat-card__value num">{trimDecimal(att.totals.overtime_hours)}</div>
              <div className="stat-card__note num">小時 · 遲到 {att.totals.late_count} 次</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">加班費</div>
              <div className="stat-card__value num">{money(att.totals.overtime_pay)}</div>
              <div className="stat-card__note num">由差勤系統帶入</div>
            </div>
          </div>

          {!att.overtime_recomputed && <p className="recon">{att.overtime_note}</p>}

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>員工</th>
                  <th className="align-right">應出勤</th>
                  <th className="align-right">實出勤</th>
                  <th className="align-right">休假</th>
                  <th className="align-right">加班時數</th>
                  <th className="align-right">加班費</th>
                  <th className="align-right">遲到</th>
                </tr>
              </thead>
              <tbody>
                {att.rows.map((r) => (
                  <tr key={r.emp_no}>
                    <td>
                      {r.name}
                      <span className="cell-hint cell-hint--block num">
                        {r.emp_no} · {r.dept_label ?? '未指定'} · {r.title}
                      </span>
                      {r.note && <span className="cell-hint cell-hint--block">{r.note}</span>}
                    </td>
                    <td className="num align-right">{trimDecimal(r.required_days)}</td>
                    <td className="num align-right">{trimDecimal(r.worked_days)}</td>
                    <td className="num align-right">{trimDecimal(r.leave_days)}</td>
                    <td className="num align-right">{trimDecimal(r.overtime_hours)}</td>
                    <td className="num align-right">
                      {money(r.overtime_pay)}
                    </td>
                    <td className="num align-right">{r.late_count}</td>
                  </tr>
                ))}
                {att.rows.length === 0 && (
                  <tr>
                    <td colSpan={7} className="cell-muted">
                      {att.month} 尚無出勤資料。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          {punches && (
            <>
              <h3 className="section-title section-title--spaced">打卡紀錄</h3>
              <p className="section-note">
                {!punches.schedule.judges_lateness || !punches.schedule.counts_overtime
                  ? punches.schedule.note
                  : (punches.schedule.flexible
                      ? `彈性工時 ${punches.schedule.work_start}–${punches.schedule.latest_start} 開始，下班同步順延`
                      : `上班 ${punches.schedule.work_start}、下班 ${punches.schedule.work_end}`) +
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
                      <th>與月統計對帳</th>
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
                        </td>
                        <td className="num align-right">{r.summary.complete_days}</td>
                        <td className="num align-right">{r.summary.worked_hours}</td>
                        <td className="num align-right">{r.summary.late_days ?? '—'}</td>
                        <td className="num align-right">{r.summary.open_days}</td>
                        <td>
                          <span
                            className={
                              r.reconciliation.matches === false
                                ? 'tag tag--warn tag--static'
                                : 'tag tag--pale tag--static'
                            }
                          >
                            {reconLabel(r.reconciliation)}
                          </span>
                        </td>
                      </tr>
                    ))}
                    {punches.rows.length === 0 && (
                      <tr>
                        <td colSpan={6} className="cell-muted">
                          {punches.month} 尚無打卡紀錄。員工可於「我的 · 打卡」自行打卡。
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>

              <p className="recon">
                打卡與匯入的月統計分開列出，兩者不同時由人資決定更正何者。
              </p>

              {picked && punches.days && (
                <div className="split-columns split-columns--wide-left">
                  <section>
                    <h3 className="section-title">
                      {punches.employee?.name} 的逐日打卡
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
                          {punches.days.map((d) => (
                            <tr key={d.date} className={d.open ? 'row--pending' : undefined}>
                              <td className="num cell-nowrap">
                                {d.date.slice(5)}
                                <span className="cell-hint">（{d.weekday}）</span>
                              </td>
                              <td className="num align-right">{d.in_at ?? '—'}</td>
                              <td className="num align-right">{d.out_at ?? '—'}</td>
                              <td className="num align-right">{d.worked_hours ?? '—'}</td>
                              <td>
                                {d.open && (
                                  <span className="tag tag--warn tag--static">未打下班</span>
                                )}
                                {d.late === true && (
                                  <span className="tag tag--bad tag--static">遲到</span>
                                )}
                                {d.manual && (
                                  <span className="tag tag--pale tag--static">補登</span>
                                )}
                                {d.note && (
                                  <span className="cell-hint cell-hint--block">{d.note}</span>
                                )}
                              </td>
                            </tr>
                          ))}
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
                        補登會覆蓋同一天同一格已有的時間，並記錄操作者。時間可填到秒。
                      </p>
                    </div>
                  </section>
                </div>
              )}
            </>
          )}

          {fixes && (
            <>
              <h3 className="section-title section-title--spaced">補登申請</h3>
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
                    {fixes.corrections.map((c) => (
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
                        <td className="num align-right">{c.at}</td>
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
                                {c.status_label}
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
                              <div className="cell-actions">
                                <button
                                  className="btn btn--compact"
                                  disabled={busy === `fix-${c.id}`}
                                  onClick={() => void decideFix(c.id, true)}
                                >
                                  核准
                                </button>
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
                    {fixes.corrections.length === 0 && (
                      <tr>
                        <td colSpan={6} className="cell-muted">
                          {fixDone ? '沒有補登申請。' : '沒有待審核的補登申請。'}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
              <p className="hint">{fixes.policy_note}</p>
            </>
          )}

          <h3 className="section-title section-title--spaced">休假單</h3>
          <div className="toolbar">
            <button
              className={`chip${includeDone ? ' chip--on' : ''}`}
              onClick={() => setIncludeDone((v) => !v)}
            >
              併同顯示已核准與已駁回
            </button>
            <span className="spacer" />
            <span className="section-note">{leave.pending_count} 張待簽核</span>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>休假單</th>
                  <th>假別</th>
                  <th>期間</th>
                  <th className="align-right">天數</th>
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
                    <td className="num cell-nowrap">
                      {r.from_on} 至 {r.to_on}
                    </td>
                    <td className="num align-right">{trimDecimal(r.days)}</td>
                    <td>
                      <span className={statusTag(r)}>{r.status_label}</span>
                      {r.status !== 'pending' && r.decided_by && (
                        <span className="cell-hint cell-hint--block">
                          {r.decided_by}
                          {r.decision_note && ` · ${r.decision_note}`}
                        </span>
                      )}
                    </td>
                    <td className="align-right">
                      {r.status === 'pending' && !r.can_decide ? (
                        <span className="cell-muted cell-reason">{r.decide_blocked_reason}</span>
                      ) : r.status === 'pending' ? (
                        <>
                          <input
                            className="entry-input entry-input--code"
                            placeholder="說明"
                            value={note[r.leave_no] ?? ''}
                            onChange={(e) =>
                              setNote((n) => ({ ...n, [r.leave_no]: e.target.value }))
                            }
                          />
                          <button
                            className="btn btn--compact"
                            disabled={busy === r.leave_no}
                            onClick={() => void decide(r, true)}
                          >
                            {busy === r.leave_no ? '處理中…' : '核准'}
                          </button>
                          <button
                            className="btn btn--compact btn--secondary"
                            disabled={busy === r.leave_no}
                            onClick={() => void decide(r, false)}
                          >
                            駁回
                          </button>
                        </>
                      ) : (
                        <span className="cell-muted">已簽核</span>
                      )}
                    </td>
                  </tr>
                ))}
                {leave.requests.length === 0 && (
                  <tr>
                    <td colSpan={6} className="cell-muted">
                      沒有待簽核休假單。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

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
          <p className="hint">
            額度依特休規則{' '}
            <span className="num">{leave.annual_leave_ruleset.version}</span>{' '}
            與年資計算（{leave.annual_leave_ruleset.effective_from} 起適用）。
            依據：{leave.annual_leave_ruleset.source_note}
          </p>
          <p className="hint">
            僅具簽核權限的角色可核准或駁回，且僅限狀態為「待簽核」的單據。
          </p>
        </>
      )}
    </div>
  );
}
