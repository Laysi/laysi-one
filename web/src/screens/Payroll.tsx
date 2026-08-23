import { useCallback, useEffect, useState } from 'react';
import {
  businessDate,
  callOp,
  isZero,
  money,
  OpFailed,
  type PayrollRunResult,
  type PayrollSummary,
  type PayslipList,
} from '../api/client';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

export function Payroll({ onToast, reloadKey, onChanged }: Props) {
  const [summary, setSummary] = useState<PayrollSummary | null>(null);
  const [slips, setSlips] = useState<PayslipList | null>(null);
  const [slipError, setSlipError] = useState<string | null>(null);
  const [period, setPeriod] = useState<string | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<PayrollRunResult | null>(null);

  const [runPeriod, setRunPeriod] = useState('');
  const [payOn, setPayOn] = useState('');

  const load = useCallback(() => {
    setError(null);
    setSlipError(null);
    callOp<PayrollSummary>('get_payroll_summary', period ? { period } : {})
      .then((s) => {
        setSummary(s);
        setRunPeriod((v) => v || s.next_period || '');
        setPayOn((v) => v || s.suggested_pay_on);

        if (!s.detail_visible) {
          setSlips(null);
          return;
        }
        callOp<PayslipList>('list_payslips', period ? { period } : {})
          .then(setSlips)
          .catch((e: Error) => {
            setSlips(null);
            setSlipError(e.message);
          });
      })
      .catch((e: Error) => setError(e.message));
  }, [period]);

  useEffect(load, [load, reloadKey]);

  async function tryDetail() {
    setSlipError(null);
    try {
      const d = await callOp<PayslipList>('list_payslips', period ? { period } : {});
      setSlips(d);
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setSlipError(msg);
      onToast(`已被拒絕：${msg}`);
    }
  }

  async function run() {
    setBusy(true);
    try {
      const d = await callOp<PayrollRunResult>(
        'run_payroll',
        {
          ...(runPeriod.trim() ? { period: runPeriod.trim() } : {}),
          ...(payOn ? { pay_on: payOn } : {}),
        },
        `計算 ${runPeriod.trim() || '本期'} 薪資`,
      );
      setResult(d);
      onToast(
        `${d.run.period} 已完成計算：${d.run.headcount} 人實發 ${money(d.run.net_total)}，` +
          `${d.run.voucher_no ?? '傳票'} 待過帳`,
      );
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const run0 = summary?.run ?? null;
  const alreadyRun =
    runPeriod.trim() !== '' && (summary?.runs ?? []).some((r) => r.period === runPeriod.trim());

  return (
    <div>
      <h1 className="page-title">薪資作業</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!summary && !error && <p className="state-note">讀取中…</p>}

      {summary && (
        <>
          <div className="toolbar">
            {summary.periods.map((p) => (
              <button
                key={p}
                className={`chip${p === summary.period ? ' chip--on' : ''}`}
                onClick={() => setPeriod(p)}
              >
                {p}
              </button>
            ))}
            <span className="spacer" />
            <span className="section-note">
              {run0 ? `${run0.headcount} 人 · 給付日 ${run0.pay_on}` : ''}
            </span>
          </div>

          {run0 ? (
            <>
              <div className="stat-grid">
                <div className="card stat-card">
                  <div className="stat-card__kicker">應發總額</div>
                  <div className="stat-card__value num">{money(run0.gross_total)}</div>
                  <div className="stat-card__note num">本薪與加班費合計</div>
                </div>
                <div className="card stat-card">
                  <div className="stat-card__kicker">代扣</div>
                  <div className="stat-card__value num">{money(run0.deduction_total)}</div>
                  <div className="stat-card__note num">勞保、健保自付與代扣所得稅</div>
                </div>
                <div className="card stat-card">
                  <div className="stat-card__kicker">實發</div>
                  <div className="stat-card__value num">{money(run0.net_total)}</div>
                  <div className="stat-card__note num">實際匯出金額</div>
                </div>
                <div className="card stat-card stat-card--total">
                  <div className="stat-card__kicker">公司另負擔</div>
                  <div className="stat-card__value num">{money(run0.employer_total)}</div>
                  <div className="stat-card__note num">勞健保與勞退</div>
                </div>
              </div>

              <p className={`recon${summary.reconciled ? '' : ' recon--off'}`}>
                {summary.reconciled ? (
                  <>
                    人資已過帳的應發合計{' '}
                    <span className="num">{money(summary.hr_posted_total)}</span> 等於總帳{' '}
                    <span className="num">6111 薪資支出</span> 的餘額，
                    兩者分開計算，結果相符。
                  </>
                ) : (
                  <>
                    不相符：人資已過帳的應發合計{' '}
                    <span className="num">{money(summary.hr_posted_total)}</span>，總帳{' '}
                    <span className="num">6111</span> 是{' '}
                    <span className="num">{money(summary.gl_balance)}</span>，差{' '}
                    <span className="num">{money(summary.difference)}</span> 元，請先暫緩發薪。
                  </>
                )}
                {!isZero(summary.not_yet_posted) && (
                  <>
                    {' '}另有 <span className="num">{money(summary.not_yet_posted)}</span>{' '}
                    的薪資傳票尚未過帳。
                  </>
                )}
              </p>

              <p className="recon recon--off">{summary.employer_note}</p>

              <div className="card fact-card">
                <div className="fact">
                  <span className="fact__key">薪資傳票</span>
                  <span className="fact__value num">
                    {run0.voucher_no ?? '—'}
                    {run0.voucher_status && (
                      <span className="tag tag--static">
                        {run0.voucher_status === 'posted' ? '已過帳' : '待過帳'}
                      </span>
                    )}
                  </span>
                </div>
                <div className="fact">
                  <span className="fact__key">本期適用規則版本</span>
                  <span className="fact__value num">
                    勞保 {run0.rulesets.labor_insurance}　健保 {run0.rulesets.health_insurance}　勞退{' '}
                    {run0.rulesets.pension}　扣繳 {run0.rulesets.withholding}
                  </span>
                </div>
                <div className="fact">
                  <span className="fact__key">計算人員與時間</span>
                  <span className="fact__value">
                    {run0.created_by} · <span className="num">{businessDate(run0.created_at)}</span>
                    {run0.note && <span className="fact__note">{run0.note}</span>}
                  </span>
                </div>
              </div>
            </>
          ) : (
            <p className="state-note">
              尚無任何期別的薪資計算紀錄。請由下方「計算本期薪資」開始，將產生一張
              待過帳傳票。
            </p>
          )}

          <h3 className="section-title section-title--spaced">逐人薪資明細</h3>

          {!summary.detail_visible && (
            <>

              <p className="recon recon--off">
                <strong>本段限負責人檢視。</strong> {summary.detail_note}
              </p>
              <div className="toolbar">
                <button className="btn btn--compact btn--secondary" onClick={() => void tryDetail()}>
                  仍要嘗試
                </button>
              </div>
            </>
          )}

          {slipError && <p className="state-note state-note--error">{slipError}</p>}

          {slips && (
            <>
              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>員工</th>
                      <th className="align-right">本薪</th>
                      <th className="align-right">加班</th>
                      <th className="align-right">代扣</th>
                      <th className="align-right">實發</th>
                      <th className="align-right">公司另負擔</th>
                    </tr>
                  </thead>
                  <tbody>
                    {slips.rows.map((s) => (
                      <tr key={s.emp_no} className={s.recompute_matches ? undefined : 'row--pending'}>
                        <td>
                          {s.name}
                          <span className="cell-hint cell-hint--block num">
                            {s.emp_no} · {s.dept_label ?? '未指定'} · {s.title}
                          </span>
                          {s.prorated && (
                            <span className="cell-hint cell-hint--block">{s.note}</span>
                          )}
                          {!s.recompute_matches && (
                            <span className="tag tag--bad tag--static">
                              重算結果與儲存值不符
                            </span>
                          )}
                        </td>
                        <td className="num align-right">{money(s.base_salary)}</td>
                        <td className="num align-right">{money(s.overtime)}</td>
                        <td className="num align-right">
                          {money(s.deduction)}
                          <span className="cell-hint cell-hint--block num">
                            勞保 {money(s.labor_employee)} · 健保 {money(s.health_employee)} · 稅{' '}
                            {money(s.income_tax)}
                          </span>
                        </td>
                        <td className="num align-right">{money(s.net)}</td>
                        <td className="num align-right">
                          {money(s.employer_total)}
                          <span className="cell-hint cell-hint--block num">
                            勞保 {money(s.labor_employer)} · 健保 {money(s.health_employer)} · 勞退{' '}
                            {money(s.pension_employer)}
                          </span>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <p className={`recon${slips.recompute_all_match ? '' : ' recon--off'}`}>
                {slips.recompute_note}
                {!slips.recompute_all_match &&
                  ' 部分列不符，常見原因為投保級距已變更，而該期薪資單保存的是當時的計算結果。'}
              </p>
            </>
          )}

          <h3 className="section-title section-title--spaced">計算本期薪資</h3>
          <div className="split-columns">
            <div className="card form-card">
              <div className="field-row">
                <label className="field field--date">
                  期別
                  <input
                    className="input input--date"
                    placeholder="YYYY-MM"
                    value={runPeriod}
                    onChange={(e) => setRunPeriod(e.target.value)}
                  />
                </label>
                <label className="field field--date">
                  給付日
                  <DateField
                    className="input input--date"
                    value={payOn}
                    onChange={(v) => setPayOn(v)}
                  />
                </label>
                <span className="spacer" />
              </div>
              <button
                className="btn"
                disabled={busy}
                onClick={() => void run()}
              >
                {busy ? '計算中…' : alreadyRun ? '補算並產生傳票' : '計算薪資並產生傳票'}
              </button>
              {alreadyRun && (
                <p className="form-note">
                  {runPeriod.trim()} 已計算過。已計薪的人不會再算一次，只有後來到職或
                  當時漏算的人會算進來，另外產生一張傳票。金額記錯請到「傳票 · 總帳」
                  開立傳票更正。
                </p>
              )}
              <p className="form-note">
                保費依<strong>期末日</strong>、扣繳依<strong>給付日</strong>認定。
                兩者通常同月，跨月發薪時才分開。
              </p>
              <p className="form-note">
                本操作一次決定全公司當月薪資並產生傳票，傳票狀態為<strong>待過帳</strong>。
              </p>
            </div>

            <div>
              {result ? (
                <div className="card doc-card">
                  <div className="doc-card__head">
                    <h3 className="section-title doc-card__title">
                      {result.run.period} 已完成計算
                    </h3>
                    <span className="doc-card__badge num">
                      {result.run.voucher_no ?? '—'} 待過帳
                    </span>
                  </div>
                  <div className="doc-row">
                    <span>應發總額</span>
                    <span className="num">{money(result.run.gross_total)}</span>
                  </div>
                  <div className="doc-row doc-row--sub">
                    <span>勞保自付</span>
                    <span className="num">
                      {money(result.deduction_breakdown.labor_employee)}
                    </span>
                  </div>
                  <div className="doc-row doc-row--sub">
                    <span>健保自付</span>
                    <span className="num">
                      {money(result.deduction_breakdown.health_employee)}
                    </span>
                  </div>
                  <div className="doc-row doc-row--sub">
                    <span>
                      代扣所得稅（{result.withholding.income_name}，
                      <span className="num">{result.withholding.rate_percent}%</span>）
                    </span>
                    <span className="num">{money(result.deduction_breakdown.income_tax)}</span>
                  </div>
                  <div className="doc-row doc-row--total">
                    <span>實發</span>
                    <span className="num">{money(result.run.net_total)}</span>
                  </div>

                  {result.warnings.length > 0 && (
                    <p className="recon recon--off">
                      下列人員低於法定最低工資，薪資仍照常發放：
                      {result.warnings.map((w) => (
                        <span className="step-chip" key={w.emp_no}>
                          {w.name}（{w.emp_no}）：{w.issue}
                        </span>
                      ))}
                    </p>
                  )}
                  {result.skipped.length > 0 && (
                    <p className="hint">
                      本期未計薪人員：
                      {result.skipped.map((s) => (
                        <span className="step-chip" key={s.emp_no}>
                          {s.name}（{s.emp_no}）：{s.reason}
                        </span>
                      ))}
                    </p>
                  )}
                  <p className="hint">
                    扣繳率依 {result.withholding.ruleset.source_note}（
                    {result.withholding.ruleset.effective_from} 起適用）。
                  </p>
                  <p className="hint">{result.employer_note}</p>
                </div>
              ) : (
                <p className="section-note">
                  傳票的三段是：借 <span className="num">6111 薪資支出</span>（逐部門一行，
                  部門損益才看得到人事成本）／貸 <span className="num">2117 代扣款</span>／貸{' '}
                  <span className="num">1102 銀行存款</span>。
                  公司負擔的勞健保與勞退不在本張傳票內。
                </p>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
