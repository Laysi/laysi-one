import { useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  myPayslipList,
  OpFailed,
  trimDecimal,
  type MyPayslipList,
} from '../api/client';
import { PeriodPicker } from '../components/PeriodPicker';

interface Props {
  reloadKey: number;
}

export function MyPayslips({ reloadKey }: Props) {
  const [data, setData] = useState<MyPayslipList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [period, setPeriod] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp('list_my_payslips', {}, undefined, myPayslipList)
      .then((d) => live && setData(d))
      .catch((e) => {
        if (!live) return;
        setError(e instanceof OpFailed ? e.error.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [reloadKey]);

  if (error) {
    return (
      <div>
        <h1 className="page-title">我的薪資單</h1>
        <p className="state-note state-note--error">{error}</p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const slips = data.payslips;
  const shown = slips.find((s) => s.period === period) ?? slips[0] ?? null;

  return (
    <div>
      <h1 className="page-title">我的薪資單</h1>

      <div className="card pay-bar">
        <div>
          <div className="pay-bar__kicker">{data.employee.name} 累計實發</div>
          <div className="pay-bar__total num">{money(data.net_total)}</div>
        </div>
        <span className="spacer" />
        <div className="pay-bar__note">
          <span className="num">{data.employee.emp_no}</span> ·{' '}
          {data.employee.dept_label ?? '未指定'} · {data.employee.title} · 共{' '}
          <span className="num">{slips.length}</span> 期，應發合計{' '}
          <span className="num">{money(data.gross_total)}</span>。
        </div>
      </div>

      {slips.length === 0 ? (
        <p className="state-note">尚無薪資單。每期薪資算完後在本頁出現。</p>
      ) : (
        <>
          <div className="toolbar">
            <PeriodPicker periods={data.periods} value={shown?.period} onChange={setPeriod} />
          </div>

          <div className="table-scroll">
            <table className="table table--inset">
              <thead>
                <tr>
                  <th>期別</th>
                  <th>給付日</th>
                  <th className="align-right">應發</th>
                  <th className="align-right">代扣</th>
                  <th className="align-right">實發</th>
                </tr>
              </thead>
              <tbody>
                {slips.map((s) => (
                  <tr
                    key={s.period}
                    className={`row--pickable${s.period === shown?.period ? ' row--pending' : ''}`}
                    onClick={() => setPeriod(s.period)}
                  >
                    <td className="num">
                      {s.period}
                      {s.prorated && (
                        <span className="cell-hint cell-hint--block">
                          {trimDecimal(s.paid_days)} / {trimDecimal(s.month_days)} 日，按曆日比例計薪
                        </span>
                      )}
                    </td>
                    <td className="num">{s.pay_on}</td>
                    <td className="num align-right">{money(s.gross)}</td>
                    <td className="num align-right">{money(s.deductions.total)}</td>
                    <td className="num align-right">{money(s.net)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {shown && (
            <div className="split-columns split-columns--spaced">
              <section className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">{shown.period} 的明細</h3>
                  <span className="doc-card__badge num">{shown.pay_on} 給付</span>
                </div>

                <div className="doc-row">
                  <span>本薪</span>
                  <span className="num">{money(shown.base_salary)}</span>
                </div>
                {!isZero(shown.overtime) && (
                  <div className="doc-row">
                    <span>加班費</span>
                    <span className="num">{money(shown.overtime)}</span>
                  </div>
                )}
                {!isZero(shown.allowance) && (
                  <div className="doc-row">
                    <span>津貼</span>
                    <span className="num">{money(shown.allowance)}</span>
                  </div>
                )}
                <div className="doc-row doc-row--sub">
                  <span>應發合計</span>
                  <span className="num">{money(shown.gross)}</span>
                </div>

                <div className="doc-row doc-row--sub">
                  <span>勞保自付</span>
                  <span className="num">{money(shown.deductions.labor_insurance)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>
                    健保自付
                    {shown.health_dependents > 0 && `（含眷屬 ${shown.health_dependents} 口）`}
                  </span>
                  <span className="num">{money(shown.deductions.health_insurance)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>代扣所得稅</span>
                  <span className="num">{money(shown.deductions.income_tax)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>代扣合計</span>
                  <span className="num">{money(shown.deductions.total)}</span>
                </div>

                <div className="doc-row doc-row--total">
                  <span>實發</span>
                  <span className="num">{money(shown.net)}</span>
                </div>

                {shown.prorated && (
                  <p className="recon">
                    本期按曆日比例計薪：{trimDecimal(shown.paid_days)} 日 /{' '}
                    {trimDecimal(shown.month_days)} 日，適用於到職或離職當月。
                  </p>
                )}
                {shown.note && <p className="hint">{shown.note}</p>}
              </section>

              <section>
                <h3 className="section-title">公司負擔部分</h3>
                <div className="card fact-card">
                  <div className="fact">
                    <span className="fact__key">勞保雇主負擔</span>
                    <span className="fact__value num">
                      {money(shown.employer_contributions.labor_insurance)}
                    </span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">健保雇主負擔</span>
                    <span className="fact__value num">
                      {money(shown.employer_contributions.health_insurance)}
                    </span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">勞退提繳</span>
                    <span className="fact__value num">
                      {money(shown.employer_contributions.pension)}
                    </span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">合計</span>
                    <span className="fact__value num">
                      {money(shown.employer_contributions.total)}
                      <span className="fact__note">
                        上列三項由公司負擔。
                      </span>
                    </span>
                  </div>
                </div>

                <h3 className="section-title section-title--spaced">計算當時的規則版本</h3>
                <div className="card fact-card">
                  <div className="fact">
                    <span className="fact__key">勞保</span>
                    <span className="fact__value num">{shown.rulesets.labor_insurance}</span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">健保</span>
                    <span className="fact__value num">{shown.rulesets.health_insurance}</span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">勞退</span>
                    <span className="fact__value num">{shown.rulesets.pension}</span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">扣繳</span>
                    <span className="fact__value num">{shown.rulesets.withholding}</span>
                  </div>
                  {shown.rulesets.health_dependents && (
                    <div className="fact">
                      <span className="fact__key">健保眷屬計費</span>
                      <span className="fact__value num">{shown.rulesets.health_dependents}</span>
                    </div>
                  )}
                  {shown.rulesets.partial_month && (
                    <div className="fact">
                      <span className="fact__key">不足月計費</span>
                      <span className="fact__value num">{shown.rulesets.partial_month}</span>
                    </div>
                  )}
                </div>
              </section>
            </div>
          )}
        </>
      )}

    </div>
  );
}
