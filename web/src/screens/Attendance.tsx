import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  trimDecimal,
  type AttendanceMonth,
  type LeaveRequestList,
  type LeaveRequestRow,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

/** Status label for a leave request. Pending ones stand out; decided ones
 *  stay quiet. */
function statusTag(r: LeaveRequestRow): string {
  if (r.status === 'rejected') return 'tag tag--bad tag--static';
  if (r.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function Attendance({ onToast, reloadKey, onChanged }: Props) {
  const [att, setAtt] = useState<AttendanceMonth | null>(null);
  const [leave, setLeave] = useState<LeaveRequestList | null>(null);
  const [month, setMonth] = useState<string | undefined>(undefined);
  const [includeDone, setIncludeDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  /** Note for the requester, keyed by request number: several may await a
   *  decision at once. */
  const [note, setNote] = useState<Record<string, string>>({});

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<AttendanceMonth>('get_attendance', month ? { month } : {}),
      callOp<LeaveRequestList>('list_leave_requests', { include_done: includeDone }),
    ])
      .then(([a, l]) => {
        setAtt(a);
        setLeave(l);
      })
      .catch((e: Error) => setError(e.message));
  }, [month, includeDone]);

  useEffect(load, [load, reloadKey]);

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
      <h1 className="page-title">出勤與請假</h1>
      <p className="page-lede">
        本月出勤與加班統計，以及待簽核的請假單。
      </p>

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
              <div className="stat-card__note">應出勤 {trimDecimal(att.totals.required_days)} 日</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">請假</div>
              <div className="stat-card__value num">{trimDecimal(att.totals.leave_days)}</div>
              <div className="stat-card__note">日</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">加班時數</div>
              <div className="stat-card__value num">{trimDecimal(att.totals.overtime_hours)}</div>
              <div className="stat-card__note">小時 · 遲到 {att.totals.late_count} 次</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">加班費</div>
              <div className="stat-card__value num">{money(att.totals.overtime_pay)}</div>
              <div className="stat-card__note">由差勤系統帶入，非本系統計算</div>
            </div>
          </div>

          {/* Having hours is not the same as having rules to multiply them
              by. This paragraph must stay. */}
          {!att.overtime_recomputed && <p className="recon">{att.overtime_note}</p>}

          <table className="table">
            <thead>
              <tr>
                <th>員工</th>
                <th className="align-right">應出勤</th>
                <th className="align-right">實出勤</th>
                <th className="align-right">請假</th>
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
                    <span className="cell-hint cell-hint--block">來源系統帶入</span>
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

          <h3 className="section-title section-title--spaced">請假單</h3>
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

          <table className="table">
            <thead>
              <tr>
                <th>請假單</th>
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
                    {r.status === 'pending' ? (
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
                    沒有待簽核請假單。
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          <h3 className="section-title section-title--spaced">
            {leave.annual_leave_year} 年特休餘額
          </h3>
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

          <p className="hint">{leave.annual_leave_note}</p>
          <p className="hint">
            額度依{' '}
            <span className="num">annual_leave {leave.annual_leave_ruleset.version}</span>{' '}
            即時計算（{leave.annual_leave_ruleset.effective_from} 起適用），資料庫未設「特休額度」
            欄位。依據：{leave.annual_leave_ruleset.source_note}
          </p>
          <p className="hint">
            核准與駁回是否可執行，取決於角色是否具備該項權限，
            <strong>權限不足時將被拒絕，並寫入一筆被擋下的稽核紀錄</strong>。
            另外，狀態非「待簽核」的單據在資料層即被擋下。
          </p>
        </>
      )}
    </div>
  );
}
