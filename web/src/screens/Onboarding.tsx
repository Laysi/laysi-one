import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  type ClosedAccess,
  type EmployeeList,
  type MovementList,
  type MovementRow,
} from '../api/client';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function statusTag(m: MovementRow): string {
  return m.status === 'done' ? 'tag tag--static' : 'tag tag--warn tag--static';
}

function closedNote(closed: ClosedAccess | undefined): string {
  if (!closed) return '';
  const parts: string[] = [];
  if (closed.logins.length > 0) parts.push(`停用登入帳號 ${closed.logins.join('、')}`);
  if (closed.tokens > 0) parts.push(`撤銷 ${closed.tokens} 組 MCP Token`);
  return parts.length > 0 ? `，並${parts.join('、')}` : '';
}

export function Onboarding({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MovementList | null>(null);
  const [staff, setStaff] = useState<EmployeeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const [empNo, setEmpNo] = useState('');
  const [kind, setKind] = useState('');
  const [effectiveOn, setEffectiveOn] = useState('');
  const [fromValue, setFromValue] = useState('');
  const [toValue, setToValue] = useState('');
  const [reason, setReason] = useState('');

  const load = useCallback(() => {
    setError(null);
    callOp<MovementList>('list_employee_movements')
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    callOp<EmployeeList>('list_employees')
      .then(setStaff)
      .catch(() => setStaff(null));
  }, [reloadKey]);

  async function completeStep(m: MovementRow) {
    setBusy(m.movement_no);
    try {
      const d = await callOp<{
        movement: MovementRow;
        finished: boolean;
        closed_access: ClosedAccess;
      }>(
        'complete_movement_step',
        { movement_no: m.movement_no },
        `${m.name}的${m.kind_label}本關卡已完成`,
      );
      onToast(
        d.finished
          ? `${m.movement_no} 全部關卡已完成，單據結案${closedNote(d.closed_access)}`
          : `${m.movement_no} 本關已完成，尚待「${d.movement.next_step?.label ?? '結案'}」`,
      );
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function record() {
    setBusy('new');
    try {
      const d = await callOp<{ movement: MovementRow }>(
        'record_employee_movement',
        {
          emp_no: empNo.trim(),
          kind,
          ...(effectiveOn ? { effective_on: effectiveOn } : {}),
          ...(fromValue.trim() ? { from_value: fromValue.trim() } : {}),
          ...(toValue.trim() ? { to_value: toValue.trim() } : {}),
          ...(reason.trim() ? { reason: reason.trim() } : {}),
        },
        `為 ${empNo.trim()} 開立異動單`,
      );
      onToast(
        `${d.movement.movement_no} 已開立：${d.movement.name}的${d.movement.kind_label}，` +
          `共 ${d.movement.step_count} 關`,
      );
      setEmpNo('');
      setFromValue('');
      setToValue('');
      setReason('');
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
      <h1 className="page-title">到離職與異動</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <div className="split-columns split-columns--wide-left">
          <section>
            <div className="toolbar">
              <span className="section-note">
                {data.movements.length} 張單據，其中 {data.open_count} 張進行中
              </span>
            </div>

            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>異動單</th>
                    <th>生效日</th>
                    <th className="align-right">進度</th>
                    <th>目前關卡</th>
                    <th className="align-right">動作</th>
                  </tr>
                </thead>
                <tbody>
                  {data.movements.map((m) => (
                    <tr key={m.movement_no} className={m.status === 'open' ? 'row--pending' : undefined}>
                      <td>
                        <button
                          className="link-cell"
                          onClick={() => setOpen(open === m.movement_no ? null : m.movement_no)}
                        >
                          {m.name} · {m.kind_label}
                        </button>
                        <span className="cell-hint cell-hint--block num">
                          {m.movement_no} · {m.emp_no}
                          {m.dept_label && ` · ${m.dept_label}`}
                        </span>
                        {(m.from_value || m.to_value) && (
                          <span className="cell-hint cell-hint--block">
                            {m.from_value || '—'} → {m.to_value || '—'}
                          </span>
                        )}
                        {open === m.movement_no && (
                          <span className="cell-hint cell-hint--block">
                            {m.steps.map((s) => (
                              <span key={s.step_no} className="step-chip">
                                {s.done ? '✓' : '○'} {s.label}
                                <span className="step-chip__why">
                                  （{s.done ? `${s.done_by ?? '—'} 已完成` : s.note}）
                                </span>
                              </span>
                            ))}
                            {m.reason && <span className="step-chip">事由：{m.reason}</span>}
                          </span>
                        )}
                      </td>
                      <td className="num cell-nowrap">{m.effective_on}</td>
                      <td className="num align-right">
                        {m.done_count} / {m.step_count}
                      </td>
                      <td>
                        <span className={statusTag(m)}>{m.step_label}</span>
                      </td>
                      <td className="align-right">
                        {m.next_step ? (
                          <button
                            className="btn btn--compact"
                            disabled={busy === m.movement_no}
                            title={`將「${m.next_step.label}」標記為完成`}
                            onClick={() => void completeStep(m)}
                          >
                            {busy === m.movement_no ? '處理中…' : '本關卡已完成'}
                          </button>
                        ) : (
                          <span className="cell-muted">
                            {m.status === 'done' ? '已結案' : '待結案'}
                          </span>
                        )}
                      </td>
                    </tr>
                  ))}
                  {data.movements.length === 0 && (
                    <tr>
                      <td colSpan={5} className="cell-muted">
                        目前沒有任何異動單。
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>

            <p className="hint">
              開立離職單時，該員工在名冊與薪資作業就標示為「離職中」並記錄預定離職日；
              完成最後一關後變成「已離職」，登入帳號同時停用、MCP Token 一併撤銷，
              要再開由系統管理員在「帳號管理」啟用。
            </p>
            <p className="hint">
              關卡所列名稱為<strong>承辦人</strong>；能否操作由角色權限決定。
            </p>
          </section>

          <section>
            <h3 className="section-title">開一張異動單</h3>
            <div className="card form-card">
              <div className="field-row">
                <label className="field field--grow">
                  員工
                  <select
                    className="input"
                    value={empNo}
                    onChange={(e) => setEmpNo(e.target.value)}
                  >
                    <option value="">請選擇</option>
                    {(staff?.employees ?? []).map((e) => (
                      <option key={e.emp_no} value={e.emp_no}>
                        {e.emp_no} {e.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field field--grow">
                  異動種類
                  <select
                    className="input"
                    value={kind}
                    onChange={(e) => setKind(e.target.value)}
                  >
                    <option value="">請選擇</option>
                    {data.flows.map((f) => (
                      <option key={f.kind} value={f.kind}>
                        {f.label}（{f.steps.length} 關）
                      </option>
                    ))}
                  </select>
                </label>
              </div>

              <div className="field-row">
                <label className="field field--date">
                  生效日
                  <DateField
                    className="input input--date"
                    value={effectiveOn}
                    onChange={(v) => setEffectiveOn(v)}
                  />
                </label>
                <label className="field field--grow">
                  從
                  <input
                    className="input"
                    placeholder="調薪、調部門需填寫"
                    value={fromValue}
                    onChange={(e) => setFromValue(e.target.value)}
                  />
                </label>
                <label className="field field--grow">
                  變成
                  <input
                    className="input"
                    value={toValue}
                    onChange={(e) => setToValue(e.target.value)}
                  />
                </label>
              </div>

              <label className="field">
                事由
                <input
                  className="input"
                  placeholder="單據事由"
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                />
              </label>

              <button
                className="btn"
                disabled={busy === 'new' || !empNo.trim() || !kind}
                onClick={() => void record()}
              >
                {busy === 'new' ? '開單中…' : '開單'}
              </button>
              <p className="form-note">
                加保、退保與調薪都要完成全部關卡才生效。
              </p>
            </div>

          </section>
        </div>
      )}

      {data && (
        <section>
          <h3 className="section-title section-title--spaced">各類異動的簽核關卡</h3>
          {data.flows.map((f) => (
            <div key={f.kind}>
              <p className="section-note">{f.label}</p>
              <div className="flow">
                {f.steps.map((s, i) => (
                  <div className="flow__step-wrap" key={s.step_no}>
                    <div className="card flow__step">
                      <div className="flow__role">{s.label}</div>
                      <div className="flow__cond">{s.note}</div>
                    </div>
                    {i < f.steps.length - 1 && <span className="flow__arrow">→</span>}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </section>
      )}
    </div>
  );
}
