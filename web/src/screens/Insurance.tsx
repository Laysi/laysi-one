import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  insuranceQuote,
  insuranceSummary,
  money,
  OpFailed,
  type InsuranceQuote,
  type InsuranceSummary,
} from '../api/client';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

export function Insurance({ onToast, reloadKey }: Props) {
  const [data, setData] = useState<InsuranceSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [quote, setQuote] = useState<InsuranceQuote | null>(null);
  const [salary, setSalary] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    setError(null);
    callOp('get_insurance_summary', {}, undefined, insuranceSummary)
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function compute() {
    setBusy(true);
    try {
      const d = await callOp(
                        'compute_insurance',
                        {
          insured_salary: salary.trim(),
        },
                        undefined,
                        insuranceQuote,
                      );
      setQuote(d);
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <h1 className="page-title">勞健保與勞退</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="stat-grid stat-grid--five">
            <div className="card stat-card">
              <div className="stat-card__kicker">勞保</div>
              <div className="stat-card__value num">{money(data.labor_employer)}</div>
              <div className="stat-card__note num">員工自付 {money(data.labor_employee)}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">健保</div>
              <div className="stat-card__value num">{money(data.health_employer)}</div>
              <div className="stat-card__note num">員工自付 {money(data.health_employee)}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">勞退</div>
              <div className="stat-card__value num">{money(data.pension_employer)}</div>
              <div className="stat-card__note num">全額由雇主提繳，員工不負擔</div>
            </div>
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">公司負擔合計</div>
              <div className="stat-card__value num">{money(data.employer_total)}</div>
              <div className="stat-card__note num">{data.headcount} 人 · {data.as_of} 適用</div>
              {data.missing_bracket_count > 0 && (
                <div className="stat-card__note num">
                  其中 {data.missing_bracket_count} 人尚未設定投保級距
                </div>
              )}
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">員工自付合計</div>
              <div className="stat-card__value num">{money(data.employee_total)}</div>
              <div className="stat-card__note num">自薪資扣除的兩項</div>
            </div>
          </div>

          {data.missing_bracket_note && (
            <p className="recon recon--off">{data.missing_bracket_note}</p>
          )}

          {!data.posted_to_ledger && <p className="recon recon--off">{data.posting_note}</p>}

          <div className="card fact-card">
            {data.notes.map((n) => (
              <div className="fact fact--stacked" key={n.title}>
                <span className="fact__key">{n.title}</span>
                <span className="fact__value">
                  {n.body}
                  <span className="fact__note">
                    依 {n.version}　{n.source}
                  </span>
                </span>
              </div>
            ))}
          </div>

          <h3 className="section-title section-title--spaced">逐人投保級距與保費</h3>
          <p className="section-note">
            高於規則上限時按上限計費，目前有 {data.capped_count} 位。
          </p>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>員工</th>
                  <th className="align-right">勞保級距</th>
                  <th className="align-right">勞保</th>
                  <th className="align-right">健保級距</th>
                  <th className="align-right">健保</th>
                  <th className="align-right">勞退</th>
                  <th className="align-right">公司合計</th>
                </tr>
              </thead>
              <tbody>
                {data.rows.map((r) => (
                  <tr
                    key={r.emp_no}
                    className={r.brackets_missing || r.labor_capped ? 'row--pending' : undefined}
                  >
                    <td>
                      {r.name}
                      {r.brackets_missing && (
                        <span className="tag tag--bad tag--static">尚未設定投保級距</span>
                      )}
                      <span className="cell-hint cell-hint--block num">
                        {r.emp_no} · {r.dept_label ?? '未指定'} · {r.title}
                      </span>
                    </td>
                    <td className="num align-right">
                      {r.labor_bracket_missing ? (
                        '尚未設定'
                      ) : (
                        <>
                          {money(r.labor_insured_salary)}
                          {r.labor_capped && (
                            <span className="cell-hint cell-hint--block num">
                              按上限 {money(r.labor_charged_on)} 計費
                            </span>
                          )}
                        </>
                      )}
                    </td>
                    <td className="num align-right">
                      {r.labor_bracket_missing ? (
                        <span className="cell-muted">—</span>
                      ) : (
                        <>
                          {money(r.labor_employer)}
                          <span className="cell-hint cell-hint--block num">
                            自付 {money(r.labor_employee)}
                          </span>
                        </>
                      )}
                    </td>
                    <td className="num align-right">
                      {r.health_bracket_missing ? '尚未設定' : money(r.health_insured_salary)}
                    </td>
                    <td className="num align-right">
                      {r.health_bracket_missing ? (
                        <span className="cell-muted">—</span>
                      ) : (
                        <>
                          {money(r.health_employer)}
                          <span className="cell-hint cell-hint--block num">
                            自付 {money(r.health_employee)}
                          </span>
                        </>
                      )}
                    </td>
                    <td className="num align-right">
                      {r.pension_wage_missing ? (
                        <>
                          <span className="cell-muted">—</span>
                          <span className="cell-hint cell-hint--block">提繳工資尚未設定</span>
                        </>
                      ) : (
                        <>
                          {money(r.pension_employer)}
                          <span className="cell-hint cell-hint--block num">
                            提繳工資 {money(r.pension_wage)}
                          </span>
                        </>
                      )}
                    </td>
                    <td className="num align-right">
                      {r.labor_bracket_missing &&
                      r.health_bracket_missing &&
                      r.pension_wage_missing ? (
                        <span className="cell-muted">—</span>
                      ) : (
                        money(r.employer_total)
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <h3 className="section-title section-title--spaced">試算一個級距</h3>
          <div className="split-columns">
            <div className="card form-card">
              <label className="field">
                勞保投保薪資
                <input
                  className="input"
                  placeholder="45800"
                  value={salary}
                  onChange={(e) => setSalary(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void compute();
                  }}
                />
              </label>
              <button className="btn" disabled={busy || !salary.trim()} onClick={() => void compute()}>
                {busy ? '試算中…' : '試算'}
              </button>
              <p className="form-note">
                健保投保金額與勞退提繳工資未填時，比照勞保投保薪資。
              </p>
            </div>

            {quote && (
              <div className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">
                    級距 <span className="num">{money(quote.insured_salary)}</span>
                  </h3>
                  <span className="doc-card__badge num">{quote.as_of} 適用</span>
                </div>
                <div className="doc-row">
                  <span>
                    勞保（率 <span className="num">{quote.labor.rate_percent}%</span>，計費基礎{' '}
                    <span className="num">{money(quote.labor_charged_on)}</span>
                    {quote.labor_capped && '，已達上限'}）
                  </span>
                  <span className="num">{money(quote.labor.employer)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>員工自付 · 政府補助</span>
                  <span className="num">
                    {money(quote.labor.employee)} · {money(quote.labor.government)}
                  </span>
                </div>
                <div className="doc-row">
                  <span>
                    健保（率 {quote.health.rate_percent}%，平均眷口{' '}
                    {quote.health.avg_dependents}）
                  </span>
                  <span className="num">{money(quote.health.employer)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>員工自付</span>
                  <span className="num">{money(quote.health.employee)}</span>
                </div>
                <div className="doc-row">
                  <span>勞退（提繳率 {quote.pension.rate_percent}%）</span>
                  <span className="num">{money(quote.pension.employer)}</span>
                </div>
                <div className="doc-row doc-row--total">
                  <span>公司負擔</span>
                  <span className="num">{money(quote.employer_total)}</span>
                </div>
                <div className="doc-row doc-row--total">
                  <span>員工自付</span>
                  <span className="num">{money(quote.employee_total)}</span>
                </div>
              </div>
            )}
          </div>

          <RulesetNote
            items={[
              { label: '勞保', version: data.rulesets.labor.version },
              { label: '健保', version: data.rulesets.health.version },
              { label: '勞退', version: data.rulesets.pension.version },
            ]}
          />
        </>
      )}
    </div>
  );
}
