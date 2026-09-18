import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  hrCompliance,
  isZero,
  maskId,
  money,
  withholdingStatements,
  type ComplianceCheck,
  type HrCompliance,
  type WithholdingStatements,
} from '../api/client';
import { screenTitle, type ScreenId } from '../nav';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onGo: (screen: ScreenId) => void;
  canViewStatements: boolean;
  reloadKey: number;
}

const DESTINATION: Record<string, ScreenId> = {
  hr: 'hr',
  hr_att: 'att',
};

function destination(c: ComplianceCheck): ScreenId | undefined {
  return c.screen ? DESTINATION[c.screen] : undefined;
}

export function Withholding({ onGo, canViewStatements, reloadKey }: Props) {
  const [compliance, setCompliance] = useState<HrCompliance | null>(null);
  const [statements, setStatements] = useState<WithholdingStatements | null>(null);
  const [year, setYear] = useState<number | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [showIds, setShowIds] = useState(false);
  const [showPayroll, setShowPayroll] = useState(false);

  const load = useCallback(() => {
    let live = true;
    const done = () => {
      live = false;
    };
    setError(null);
    callOp('get_hr_compliance', {}, undefined, hrCompliance)
      .then((d) => live && setCompliance(d))
      .catch((e: Error) => live && setError(e.message));

    if (!canViewStatements) return done;
    callOp('list_withholding_statements', year ? { year } : {}, undefined, withholdingStatements)
      .then((d) => live && setStatements(d))
      .catch((e: Error) => live && setError(e.message));
    return done;
  }, [year, canViewStatements]);

  useEffect(load, [load, reloadKey]);

  const failing = compliance?.checks.filter((c) => c.checked && c.passed === false) ?? [];
  const unchecked = compliance?.checks.filter((c) => !c.checked) ?? [];
  const passing = compliance?.checks.filter((c) => c.checked && c.passed === true) ?? [];
  const recon = statements?.payroll_reconciliation;
  const anonymous = statements
    ? statements.statement_count - statements.person_specific_count
    : 0;

  return (
    <div>
      <h1 className="page-title">扣繳與法遵</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!compliance && !error && <p className="state-note">讀取中…</p>}

      {compliance && (
        <>
          <div className="toolbar">
            <h2 className="section-title">法遵檢核</h2>
            <span className="spacer" />
            <span className="section-note num">
              {compliance.as_of} · 在職 {compliance.employee_count} 人
            </span>
          </div>

          {failing.map((c) => {
            const to = destination(c);
            return (
              <section className="card doc-card" key={c.name}>
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">
                    {c.name}
                    <span className="tag tag--bad tag--static">{c.failures.length} 人待確認</span>
                  </h3>
                  {to && (
                    <button className="btn btn--compact btn--secondary" onClick={() => onGo(to)}>
                      前往{screenTitle(to)} →
                    </button>
                  )}
                </div>
                <table className="table">
                  <tbody>
                    {c.failures.map((f) => (
                      <tr key={f.emp_no}>
                        <td className="cell-fit">
                          {f.name}
                          <span className="cell-hint num">{f.emp_no}</span>
                        </td>
                        <td>{f.message}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                {c.scope_note && <p className="hint">{c.scope_note}</p>}
              </section>
            );
          })}

          {unchecked.length > 0 && (
            <div className="card fact-card">
              {unchecked.map((c) => (
                <div className="fact fact--stacked" key={c.name}>
                  <span className="fact__key">
                    {c.name}
                    <span className="tag tag--warn tag--static">未檢查</span>
                  </span>
                  <span className="fact__value">{c.not_checked_reason}</span>
                </div>
              ))}
            </div>
          )}

          {passing.length > 0 && (
            <div className="toolbar toolbar--spaced">
              {passing.map((c) => (
                <span className="tag tag--pale tag--static" key={c.name}>
                  ✓ {c.name}
                </span>
              ))}
            </div>
          )}
        </>
      )}

      {statements && recon && (
        <>
          <div className="toolbar toolbar--spaced">
            <h2 className="section-title">年度扣繳憑單</h2>
            <span className="spacer" />
            <button className="chip" onClick={() => setYear(statements.year - 1)}>
              ← {statements.year - 1}
            </button>
            <button className="chip chip--on">{statements.year} 年度</button>
            <button className="chip" onClick={() => setYear(statements.year + 1)}>
              {statements.year + 1} →
            </button>
            <button
              className={`chip${showIds ? ' chip--on' : ''}`}
              onClick={() => setShowIds((v) => !v)}
            >
              顯示身分證字號
            </button>
          </div>

          <div className="stat-grid stat-grid--two">
            <div className="card stat-card">
              <div className="stat-card__kicker">全年給付</div>
              <div className="stat-card__value num">{money(statements.gross_total)}</div>
              <div className="stat-card__note num">
                {statements.statement_count} 位給付對象
                {anonymous > 0 && ` · ${anonymous} 位缺識別資料`}
              </div>
            </div>
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">扣繳稅額</div>
              <div className="stat-card__value num">
                {money(statements.withheld_total ?? statements.known_withheld_total)}
              </div>
              {!statements.withheld_complete && (
                <div className="stat-card__note num">
                  另有 {statements.missing_table_amounts} 筆來源未帶稅額
                </div>
              )}
            </div>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>給付對象</th>
                  <th>身分證字號 / 統一編號</th>
                  <th className="align-right">全年給付</th>
                  <th className="align-right">扣繳稅額</th>
                  <th className="align-right">筆數</th>
                </tr>
              </thead>
              <tbody>
                {statements.statements.map((s) => {
                  const key = `${s.payee}-${s.payee_id}`;
                  return (
                    <tr key={key} className={s.person_specific ? undefined : 'row--waiting'}>
                      <td>
                        <button
                          className="link-cell"
                          onClick={() => setOpen(open === key ? null : key)}
                        >
                          {s.payee}
                        </button>
                        {open === key && (
                          <table className="table table--inset">
                            <thead>
                              <tr>
                                <th>所得格式</th>
                                <th className="align-right">給付</th>
                                <th className="align-right">扣繳</th>
                                <th className="align-right">筆數</th>
                              </tr>
                            </thead>
                            <tbody>
                              {s.income_lines.map((l) => (
                                <tr key={l.income_code}>
                                  <td>
                                    <span className="num">{l.income_code}</span> {l.income_name}
                                    <span className="cell-hint cell-hint--block">
                                      {l.basis_labels.join('、')}
                                    </span>
                                  </td>
                                  <td className="num align-right">{money(l.gross_amount)}</td>
                                  <td className="num align-right">
                                    {money(l.known_withheld_amount)}
                                    {!l.withheld_complete && (
                                      <span className="tag tag--warn tag--static">來源未帶稅額</span>
                                    )}
                                  </td>
                                  <td className="num align-right">{l.payment_count}</td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        )}
                      </td>
                      <td className="num">
                        {s.payee_id ? (
                          showIds ? (
                            s.payee_id
                          ) : (
                            maskId(s.payee_id)
                          )
                        ) : (
                          <span className="tag tag--warn tag--static">可能為多人合併</span>
                        )}
                      </td>
                      <td className="num align-right">{money(s.gross_amount)}</td>
                      <td className="num align-right">
                        {money(s.known_withheld_amount)}
                        {!s.withheld_complete && (
                          <span className="cell-hint cell-hint--block">來源未帶稅額</span>
                        )}
                      </td>
                      <td className="num align-right">{s.payment_count}</td>
                    </tr>
                  );
                })}
                {statements.statements.length === 0 && (
                  <tr>
                    <td colSpan={5} className="cell-muted">
                      {statements.year} 年度尚無扣繳紀錄。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <div className="toolbar toolbar--spaced">
            <h2 className="section-title">與薪資模組對帳</h2>
            <span
              className={`tag tag--static${recon.reconciled ? ' tag--pale' : ' tag--bad'}`}
            >
              {recon.reconciled ? '相符' : '不相符'}
            </span>
            <span className="spacer" />
            <button
              className={`chip${showPayroll ? ' chip--on' : ''}`}
              onClick={() => setShowPayroll((v) => !v)}
            >
              逐人明細
            </button>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>薪資所得</th>
                  <th className="align-right">薪資模組</th>
                  <th className="align-right">扣繳紀錄</th>
                  <th className="align-right">差額</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>全年給付</td>
                  <td className="num align-right">{money(recon.payroll_gross)}</td>
                  <td className="num align-right">
                    {money(recon.withholding_record_salary_gross)}
                  </td>
                  <td
                    className={`num align-right${
                      isZero(recon.gross_difference) ? '' : ' cell-hint--alarm'
                    }`}
                  >
                    {money(recon.gross_difference)}
                  </td>
                </tr>
                <tr>
                  <td>扣繳稅額</td>
                  <td className="num align-right">{money(recon.payroll_withheld)}</td>
                  <td className="num align-right">
                    {money(recon.withholding_record_known_salary_withheld)}
                  </td>
                  <td
                    className={`num align-right${
                      recon.withheld_difference !== null && isZero(recon.withheld_difference)
                        ? ''
                        : ' cell-hint--alarm'
                    }`}
                  >
                    {recon.withheld_difference === null
                      ? '來源未帶稅額'
                      : money(recon.withheld_difference)}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>

          {showPayroll && (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>員工</th>
                    <th>身分證字號</th>
                    <th className="align-right">全年給付</th>
                    <th className="align-right">扣繳稅額</th>
                    <th className="align-right">計薪月數</th>
                  </tr>
                </thead>
                <tbody>
                  {statements.employee_income.map((e) => (
                    <tr key={e.emp_no}>
                      <td>
                        {e.name}
                        <span className="cell-hint num">{e.emp_no}</span>
                      </td>
                      <td className="num">
                        {e.payee_id ? (showIds ? e.payee_id : maskId(e.payee_id)) : '未登錄'}
                      </td>
                      <td className="num align-right">{money(e.gross_amount)}</td>
                      <td className="num align-right">{money(e.withheld_amount)}</td>
                      <td className="num align-right">{e.payroll_months}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          <RulesetNote
            items={statements.rulesets.map((r) => ({
              label: '扣繳',
              source: r.source_note,
            }))}
          />
        </>
      )}
    </div>
  );
}
