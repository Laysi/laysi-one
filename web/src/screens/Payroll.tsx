import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessDate,
  callOp,
  isZero,
  money,
  OpFailed,
  payrollRunResult,
  payrollSummary,
  payslipList,
  voidedPayroll,
  type PayrollRunResult,
  type PayrollSummary,
  type PayslipList,
} from '../api/client';
import { DateField } from '../components/DateField';
import { RulesetNote } from '../components/RulesetNote';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
  canVoid: boolean;
}

export function Payroll({ onToast, reloadKey, onChanged, canVoid }: Props) {
  const [summary, setSummary] = useState<PayrollSummary | null>(null);
  const [slips, setSlips] = useState<PayslipList | null>(null);
  const [slipError, setSlipError] = useState<string | null>(null);
  const [period, setPeriod] = useState<string | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<PayrollRunResult | null>(null);

  const [runPeriod, setRunPeriod] = useState('');
  const [payOn, setPayOn] = useState('');
  const [voidReason, setVoidReason] = useState('');
  const [voidOpen, setVoidOpen] = useState(false);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    setSlipError(null);
    callOp('get_payroll_summary', period ? { period } : {}, undefined, payrollSummary)
      .then((s) => {
        if (!live) return;
        setSummary(s);
        setRunPeriod((v) => v || s.next_period || '');
        setPayOn((v) => v || s.suggested_pay_on);

        if (!s.detail_visible) {
          setSlips(null);
          return;
        }
        callOp('list_payslips', period ? { period } : {}, undefined, payslipList)
          .then((d) => live && setSlips(d))
          .catch((e: Error) => {
            if (!live) return;
            setSlips(null);
            setSlipError(e.message);
          });
      })
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, [period]);

  useEffect(load, [load, reloadKey]);

  async function tryDetail() {
    setSlipError(null);
    try {
      const d = await callOp('list_payslips', period ? { period } : {}, undefined, payslipList);
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
      const d = await callOp(
                        'run_payroll',
                        {
            period: runPeriod.trim(),
            ...(payOn ? { pay_on: payOn } : {}),
          },
                        `計算 ${runPeriod.trim()} 薪資`,
                        payrollRunResult,
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

  async function voidRun(target: string) {
    setBusy(true);
    try {
      const d = await callOp(
        'void_payroll_run',
        { period: target, reason: voidReason.trim() },
        `作廢 ${target} 薪資作業`,
        voidedPayroll,
      );
      onToast(
        `${d.voided.period} 薪資作業已作廢：${d.voided.headcount} 人的薪資單與` +
          `${d.voided.voucher_nos.join('、') || '傳票'}已刪除，可以重新計算。`,
      );
      setVoidOpen(false);
      setVoidReason('');
      setResult(null);
      setPeriod(undefined);
      setRunPeriod(target);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }


  const run0 = summary?.run ?? null;
  const periodRuns = summary?.period_runs ?? [];
  const allPending =
    periodRuns.length > 0 && periodRuns.every((r) => r.voucher_status === 'pending');
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
                {summary.period_runs.map((r, i) => (
                  <div key={r.voucher_no ?? i}>
                    <div className="fact">
                      <span className="fact__key">
                        {run0.run_count > 1 ? `第 ${run0.run_count - i} 次計算的傳票` : '薪資傳票'}
                      </span>
                      <span className="fact__value num">
                        {r.voucher_no ?? '—'}
                        {r.voucher_status && (
                          <span className="tag tag--static">
                            {r.voucher_status === 'posted' ? '已過帳' : '待過帳'}
                          </span>
                        )}
                        <span className="cell-hint cell-hint--block num">
                          {r.headcount} 人 · 應發 {money(r.gross_total)}
                        </span>
                      </span>
                    </div>
                    <div className="fact">
                      <span className="fact__key">適用規則版本</span>
                      <span className="fact__value num">
                        勞保 {r.rulesets.labor_insurance}　健保 {r.rulesets.health_insurance}　勞退{' '}
                        {r.rulesets.pension}　扣繳 {r.rulesets.withholding}
                      </span>
                    </div>
                    <div className="fact">
                      <span className="fact__key">計算人員與時間</span>
                      <span className="fact__value">
                        {r.created_by} · <span className="num">{businessDate(r.created_at)}</span>
                        {r.note && <span className="fact__note">{r.note}</span>}
                      </span>
                    </div>
                  </div>
                ))}
              </div>

              {canVoid && allPending && (
                <div className="card form-card">
                  {voidOpen ? (
                    <>
                      <p className="form-warn">
                        作廢後<strong>無法復原</strong>：{run0.period} 的 {run0.headcount}{' '}
                        人薪資單、扣繳紀錄與{' '}
                        <span className="num">
                          {periodRuns.map((r) => r.voucher_no ?? '—').join('、')}
                        </span>{' '}
                        一併刪除，之後可以重新計算。
                      </p>
                      <label className="field field--full">
                        作廢理由（必填，會寫進稽核摘要）
                        <textarea
                          className="input"
                          rows={2}
                          value={voidReason}
                          placeholder="例如：到職日更正，重新計算本期"
                          onChange={(e) => setVoidReason(e.target.value)}
                        />
                      </label>
                      <div className="toolbar">
                        <button className="btn btn--ghost" onClick={() => setVoidOpen(false)}>
                          取消
                        </button>
                        <span className="spacer" />
                        <button
                          className={`btn${busy ? ' btn--off' : ''}`}
                          disabled={busy || voidReason.trim() === ''}
                          onClick={() => void voidRun(run0.period)}
                        >
                          {busy ? '處理中…' : '確認作廢，無法復原'}
                        </button>
                      </div>
                    </>
                  ) : (
                    <button
                      className="btn btn--secondary btn--compact"
                      onClick={() => setVoidOpen(true)}
                    >
                      作廢本期薪資作業，重新計算
                    </button>
                  )}
                </div>
              )}
            </>
          ) : (
            <p className="state-note">尚無薪資計算紀錄，請由右側算出第一期。</p>
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
                          {s.note && <span className="cell-hint cell-hint--block">{s.note}</span>}
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
                  <span className="field__hint">勞健保勞退依期末日</span>
                </label>
                <label className="field field--date">
                  給付日
                  <DateField
                    className="input input--date"
                    value={payOn}
                    onChange={(v) => setPayOn(v)}
                  />
                  <span className="field__hint">扣繳依給付日</span>
                </label>
                <span className="spacer" />
              </div>
              <button
                className="btn"
                disabled={busy || runPeriod.trim() === ''}
                onClick={() => void run()}
              >
                {busy ? '計算中…' : alreadyRun ? '補算並產生傳票' : '計算薪資並產生傳票'}
              </button>
              {alreadyRun && (
                <p className="form-note">
                  {runPeriod.trim()} 已計算過。這次只算後來到職或當時漏算的人，另外產生一張傳票。
                </p>
              )}
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
                  {result.attendance_checks.length > 0 && (
                    <p className="recon recon--off">
                      下列出勤未計入本期金額，請確認後再過帳：
                      {result.attendance_checks.map((c) => (
                        <span className="step-chip" key={c.emp_no}>
                          {c.name}（{c.emp_no}）：
                          {[
                            c.absent_dates.length > 0 &&
                              `曠職 ${c.absent_dates.map((d) => d.slice(5)).join('、')}`,
                            c.open_dates.length > 0 &&
                              `未打下班 ${c.open_dates.map((d) => d.slice(5)).join('、')}`,
                            c.pending_leaves.length > 0 &&
                              `休假單待簽核 ${c.pending_leaves.join('、')}`,
                            c.holiday_dates.length > 0 &&
                              `國定假日 ${c.holiday_dates.map((d) => d.slice(5)).join('、')} 的工資`,
                          ]
                            .filter(Boolean)
                            .join('；')}
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
                  <RulesetNote
                    items={[
                      {
                        label: '扣繳',
                        from: result.withholding.ruleset.effective_from,
                        source: result.withholding.ruleset.source_note,
                      },
                    ]}
                  />
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
