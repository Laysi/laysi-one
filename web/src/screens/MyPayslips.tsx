import { useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  OpFailed,
  trimDecimal,
  type MyPayslipList,
} from '../api/client';

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
    callOp<MyPayslipList>('list_my_payslips')
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
        <p className="hint">
          本帳號尚未對應員工編號。請管理員完成連結後再回到本頁。
        </p>
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
        <p className="state-note">
          尚無任何期別的薪資紀錄。薪資按期計算，算完才會產生薪資單。
        </p>
      ) : (
        <>
          <div className="toolbar">
            {data.periods.map((p) => (
              <button
                key={p}
                className={`chip${shown?.period === p ? ' chip--on' : ''}`}
                onClick={() => setPeriod(p)}
              >
                {p}
              </button>
            ))}
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
                  <span>健保自付</span>
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

                <h3 className="section-title section-title--spaced">本期適用規則版本</h3>
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
                </div>

                <p className="hint">
                  這四個版本保存於本張薪資單。
                </p>
              </section>
            </div>
          )}
        </>
      )}

    </div>
  );
}
