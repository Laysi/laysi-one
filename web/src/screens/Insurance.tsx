import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type InsuranceQuote,
  type InsuranceSummary,
} from '../api/client';

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
    callOp<InsuranceSummary>('get_insurance_summary')
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function compute() {
    setBusy(true);
    try {
      const d = await callOp<InsuranceQuote>('compute_insurance', {
        insured_salary: salary.trim(),
      });
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
      <p className="page-lede">
        每月公司負擔與員工自付金額。本頁所有費率均取自法規規則庫，非寫於程式碼。
      </p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="stat-grid stat-grid--five">
            <div className="card stat-card">
              <div className="stat-card__kicker">勞保</div>
              <div className="stat-card__value num">{money(data.labor_employer)}</div>
              <div className="stat-card__note">員工自付 {money(data.labor_employee)}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">健保</div>
              <div className="stat-card__value num">{money(data.health_employer)}</div>
              <div className="stat-card__note">員工自付 {money(data.health_employee)}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">勞退</div>
              <div className="stat-card__value num">{money(data.pension_employer)}</div>
              <div className="stat-card__note">全額由雇主提繳，員工不負擔</div>
            </div>
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">公司負擔合計</div>
              <div className="stat-card__value num">{money(data.employer_total)}</div>
              <div className="stat-card__note">{data.headcount} 人 · {data.as_of} 適用</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">員工自付合計</div>
              <div className="stat-card__value num">{money(data.employee_total)}</div>
              <div className="stat-card__note">自薪資扣除的兩項</div>
            </div>
          </div>

          {/* The employer share is not in the ledger yet. State it, so nobody
              assumes it is already booked as an expense. */}
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
            投保級距是這家公司替員工<strong>申報</strong>的數字，屬於事實；上限住在規則裡。
            級距未及時更新時，計費仍受規則上限約束；目前有 {data.capped_count} 位按上限計費。
          </p>

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
                <tr key={r.emp_no} className={r.labor_capped ? 'row--pending' : undefined}>
                  <td>
                    {r.name}
                    <span className="cell-hint cell-hint--block num">
                      {r.emp_no} · {r.dept_label ?? '未指定'} · {r.title}
                    </span>
                  </td>
                  <td className="num align-right">
                    {money(r.labor_insured_salary)}
                    {r.labor_capped && (
                      <span className="cell-hint cell-hint--block num">
                        按上限 {money(r.labor_charged_on)} 計費
                      </span>
                    )}
                  </td>
                  <td className="num align-right">
                    {money(r.labor_employer)}
                    <span className="cell-hint cell-hint--block num">
                      自付 {money(r.labor_employee)}
                    </span>
                  </td>
                  <td className="num align-right">{money(r.health_insured_salary)}</td>
                  <td className="num align-right">
                    {money(r.health_employer)}
                    <span className="cell-hint cell-hint--block num">
                      自付 {money(r.health_employee)}
                    </span>
                  </td>
                  <td className="num align-right">
                    {money(r.pension_employer)}
                    <span className="cell-hint cell-hint--block num">
                      提繳工資 {money(r.pension_wage)}
                    </span>
                  </td>
                  <td className="num align-right">{money(r.employer_total)}</td>
                </tr>
              ))}
            </tbody>
          </table>

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
                費率依日期解析：系統不存在「當前費率」，僅有「某日適用的費率」。
              </p>
            </div>

            {quote && (
              <div className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">
                    級距 {money(quote.insured_salary)}
                  </h3>
                  <span className="doc-card__badge num">{quote.as_of} 適用</span>
                </div>
                <div className="doc-row">
                  <span>
                    勞保（率 {quote.labor.rate_percent}%，計費基礎{' '}
                    {money(quote.labor_charged_on)}
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
                <p className="hint">
                  員工自付合計 <span className="num">{money(quote.employee_total)}</span>。
                  依 labor_insurance {quote.labor.ruleset.version}、health_insurance{' '}
                  {quote.health.ruleset.version}、pension {quote.pension.ruleset.version} 計算。
                </p>
              </div>
            )}
          </div>

          <p className="hint">
            這一頁的費率、上限與平均眷口數全部來自{' '}
            <span className="num">
              labor_insurance {data.rulesets.labor.version}、health_insurance{' '}
              {data.rulesets.health.version}、pension {data.rulesets.pension.version}
            </span>
            。法規修正時僅需新增一份生效日較晚的規則檔，畫面與計算都不用調整，
            上方三段說明中的數字也會跟著更新。
          </p>
          <p className="hint">
            投保級距開放檢視（供會計核對繳款單），個人月薪、加班與實發金額不在本頁；
            該等資料位於「薪資作業」的逐人明細，並限負責人檢視。
          </p>
        </>
      )}
    </div>
  );
}
