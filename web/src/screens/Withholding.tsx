import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ComplianceCheck,
  type HrCompliance,
  type WithholdingStatements,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

function checkMark(c: ComplianceCheck): { mark: string; tag: string; label: string } {
  if (!c.checked) return { mark: '？', tag: 'tag tag--warn tag--static', label: '未檢查' };
  if (c.passed === false) {
    return {
      mark: '！',
      tag: 'tag tag--bad tag--static',
      label: `${c.failures.length} 筆待確認`,
    };
  }
  return { mark: '✓', tag: 'tag tag--pale tag--static', label: '已檢查 · 無差異' };
}

export function Withholding({ onToast, reloadKey }: Props) {
  const [compliance, setCompliance] = useState<HrCompliance | null>(null);
  const [statements, setStatements] = useState<WithholdingStatements | null>(null);
  const [statementError, setStatementError] = useState<string | null>(null);
  const [year, setYear] = useState<number | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    setStatementError(null);
    callOp<HrCompliance>('get_hr_compliance')
      .then(setCompliance)
      .catch((e: Error) => setError(e.message));

    callOp<WithholdingStatements>('list_withholding_statements', year ? { year } : {})
      .then((d) => {
        setStatements(d);
        setStatementError(null);
      })
      .catch((e) => {
        setStatements(null);
        setStatementError(e instanceof OpFailed ? e.error.message : String(e));
      });
  }, [year]);

  useEffect(load, [load, reloadKey]);

  async function tryStatements() {
    setStatementError(null);
    try {
      const d = await callOp<WithholdingStatements>(
        'list_withholding_statements',
        year ? { year } : {},
      );
      setStatements(d);
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setStatementError(msg);
      onToast(`已被拒絕：${msg}`);
    }
  }

  return (
    <div>
      <h1 className="page-title">扣繳與法遵</h1>
      <p className="page-lede">年度扣繳憑單含逐人全年所得，僅負責人可檢視。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!compliance && !error && <p className="state-note">讀取中…</p>}

      {compliance && (
        <>
          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">已檢查</div>
              <div className="stat-card__value num">{compliance.checked_count}</div>
              <div className="stat-card__note">規則庫已收錄的項目</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">未檢查</div>
              <div className="stat-card__value num">{compliance.unchecked_count}</div>
              <div className="stat-card__note">規則庫尚未收錄</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">待確認差異</div>
              <div className="stat-card__value num">{compliance.failure_count}</div>
              <div className="stat-card__note">
                {compliance.checked_all_passed ? '目前無差異' : '需逐筆確認'}
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">檢核基準日</div>
              <div className="stat-card__value num">{compliance.as_of}</div>
              <div className="stat-card__note">{compliance.employee_count} 位在職員工</div>
            </div>
          </div>

          <p className="recon recon--off">{compliance.coverage_note}</p>

          <div className="card fact-card">
            {compliance.checks.map((c) => {
              const state = checkMark(c);
              return (
                <div className="fact fact--stacked" key={c.name}>
                  <span className="fact__key">
                    <span className="num">{state.mark}</span> {c.name}
                    <span className={state.tag}>{state.label}</span>
                  </span>
                  <span className="fact__value">

                    {!c.checked && (
                      <>
                        <strong>{c.not_checked_reason}</strong>
                        <span className="fact__note">
                          這項規則收錄後，本表會自動開始檢查。
                        </span>
                      </>
                    )}
                    {c.checked && c.scope_note}

                    {c.failures.map((f) => (
                      <span className="step-chip" key={`${c.name}-${f.emp_no}`}>
                        {f.name}（{f.emp_no}）：{f.message}
                      </span>
                    ))}

                    {c.evidence && (
                      <span className="fact__note num">
                        已逐人試算 {c.evidence.employee_count} 位：公司負擔勞保{' '}
                        {money(c.evidence.labor_employer)}、健保{' '}
                        {money(c.evidence.health_employer)}、勞退{' '}
                        {money(c.evidence.pension_employer)}
                      </span>
                    )}

                    {c.rulesets.length > 0 && (
                      <span className="fact__note">
                        依 {c.rulesets.map((r) => r.source_note).join('；')}
                      </span>
                    )}
                  </span>
                </div>
              );
            })}
          </div>

          <h3 className="section-title section-title--spaced">年度扣繳憑單</h3>

          {statementError && (
            <>
              <p className="recon recon--off">
                扣繳憑單含個人全年所得，僅負責人可檢視。每次嘗試都會留下稽核紀錄。
              </p>
              <p className="state-note state-note--error">{statementError}</p>
              <div className="toolbar">
                <button
                  className="btn btn--compact btn--secondary"
                  onClick={() => void tryStatements()}
                >
                  再次嘗試
                </button>
              </div>
            </>
          )}

          {statements && (
            <>
              <div className="toolbar">
                <button className="chip" onClick={() => setYear(statements.year - 1)}>
                  ← {statements.year - 1} 年度
                </button>
                <button className="chip chip--on">{statements.year} 年度</button>
                <button className="chip" onClick={() => setYear(statements.year + 1)}>
                  {statements.year + 1} 年度 →
                </button>
                <span className="spacer" />
                <span className="section-note">
                  {statements.from} 至 {statements.to} · {statements.statement_count} 位給付對象
                  （其中 {statements.person_specific_count} 位有逐人識別資料）
                </span>
              </div>

              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>給付對象</th>
                      <th className="align-right">全年給付</th>
                      <th className="align-right">扣繳稅額</th>
                      <th className="align-right">筆數</th>
                    </tr>
                  </thead>
                  <tbody>
                    {statements.statements.map((s) => (
                      <tr
                        key={`${s.payee}-${s.payee_id}`}
                        className={s.person_specific ? undefined : 'row--waiting'}
                      >
                        <td>
                          <button
                            className="link-cell"
                            onClick={() =>
                              setOpen(open === `${s.payee}-${s.payee_id}` ? null : `${s.payee}-${s.payee_id}`)
                            }
                          >
                            {s.payee}
                          </button>
                          <span className="cell-hint cell-hint--block num">
                            {s.payee_id || '來源無識別資料'}
                          </span>
                          {!s.person_specific && (
                            <span className="cell-hint cell-hint--block">{s.source_note}</span>
                          )}
                          {open === `${s.payee}-${s.payee_id}` && (
                            <span className="cell-hint cell-hint--block">
                              {s.income_lines.map((l) => (
                                <span className="step-chip" key={l.income_code}>
                                  {l.income_code} {l.income_name}：給付{' '}
                                  <span className="num">{money(l.gross_amount)}</span>、扣繳{' '}
                                  <span className="num">
                                    {l.withheld_amount === null
                                      ? `已知 ${money(l.known_withheld_amount)}（來源缺值）`
                                      : money(l.withheld_amount)}
                                  </span>
                                  <span className="step-chip__why">
                                    （{l.payment_count} 筆 · {l.basis_labels.join('、')}）
                                  </span>
                                </span>
                              ))}
                            </span>
                          )}
                        </td>
                        <td className="num align-right">{money(s.gross_amount)}</td>
                        <td className="num align-right">
                          {s.withheld_amount === null ? (
                            <>
                              {money(s.known_withheld_amount)}
                              <span className="cell-hint cell-hint--block">
                                來源未提供完整稅額，本頁不代為推算
                              </span>
                            </>
                          ) : (
                            money(s.withheld_amount)
                          )}
                        </td>
                        <td className="num align-right">{s.payment_count}</td>
                      </tr>
                    ))}
                    {statements.statements.length === 0 && (
                      <tr>
                        <td colSpan={4} className="cell-muted">
                          {statements.year} 年度尚無扣繳紀錄。
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>

              <div className="wht-total">
                <span className="cell-muted">全年給付合計</span>
                <span className="num">{money(statements.gross_total)}</span>
              </div>
              <div className="wht-total">
                <span className="cell-muted">
                  扣繳稅額合計
                  {!statements.withheld_complete &&
                    `（另有 ${statements.missing_table_amounts} 筆來源未提供稅額）`}
                </span>
                <span className="num">
                  {statements.withheld_total ?? money(statements.known_withheld_total)}
                </span>
              </div>

              <p
                className={`recon${
                  statements.payroll_reconciliation.reconciled ? '' : ' recon--off'
                }`}
              >
                {statements.payroll_reconciliation.reconciled ? (
                  <>
                    薪資模組的逐人年度合計{' '}
                    <span className="num">
                      {money(statements.payroll_reconciliation.payroll_gross)}
                    </span>{' '}
                    與扣繳紀錄相符。
                  </>
                ) : (
                  <>
                    對不上：薪資模組全年給付{' '}
                    <span className="num">
                      {money(statements.payroll_reconciliation.payroll_gross)}
                    </span>
                    ，扣繳紀錄{' '}
                    <span className="num">
                      {money(statements.payroll_reconciliation.withholding_record_table_gross)}
                    </span>
                    ，差{' '}
                    <span className="num">
                      {money(statements.payroll_reconciliation.gross_difference)}
                    </span>{' '}
                    元。
                    {!statements.payroll_reconciliation.table_withheld_complete &&
                      ' 另有來源沒有帶稅額，因此稅額的差異無法計算。'}
                  </>
                )}
                {' '}
                {statements.payroll_reconciliation.note}
              </p>

              <h3 className="section-title section-title--spaced">薪資模組的逐人年度合計</h3>
              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>員工</th>
                      <th className="align-right">全年給付</th>
                      <th className="align-right">已扣繳</th>
                      <th className="align-right">計薪月數</th>
                    </tr>
                  </thead>
                  <tbody>
                    {statements.employee_income.map((e) => (
                      <tr key={e.emp_no}>
                        <td>
                          {e.name}
                          <span className="cell-hint num">
                            {e.emp_no} · {e.payee_id || '未登錄'}
                          </span>
                        </td>
                        <td className="num align-right">{money(e.gross_amount)}</td>
                        <td className="num align-right">{money(e.withheld_amount)}</td>
                        <td className="num align-right">{e.payroll_months}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>

              <p className="hint">{statements.table_note}</p>
              <p className="hint">{statements.source_note}</p>
              {statements.rulesets.length > 0 && (
                <p className="hint">
                  依 {statements.rulesets.map((r) => r.source_note).join('；')} 計算。
                </p>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
