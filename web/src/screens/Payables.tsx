import { useCallback, useEffect, useRef, useState } from 'react';
import {
  businessToday,
  callOp,
  docNumberHint,
  isZero,
  money,
  newApBill,
  newPaymentBatch,
  OpFailed,
  payablesResult,
  type ApBill,
  type PayablesResult,
} from '../api/client';
import {
  EMPTY_TAX_TRIPLE,
  TaxAmountFields,
  type TaxTriple,
} from '../components/TaxAmountFields';
import { DateField } from '../components/DateField';
import { useCurrentUser } from '../auth';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function sumCents(values: string[]): string {
  const cents = values.reduce((acc, v) => {
    const neg = v.startsWith('-');
    const [whole = '0', frac = ''] = (neg ? v.slice(1) : v).split('.');
    const n = BigInt(whole) * 100n + BigInt((frac + '00').slice(0, 2));
    return neg ? acc - n : acc + n;
  }, 0n);
  const neg = cents < 0n;
  const abs = neg ? -cents : cents;
  return `${neg ? '-' : ''}${abs / 100n}.${String(abs % 100n).padStart(2, '0')}`;
}

export function Payables({ onToast, reloadKey, onChanged }: Props) {
  const canPay = useCurrentUser().permissions.includes('create_payment_batch');
  const [data, setData] = useState<PayablesResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);

  const [drawer, setDrawer] = useState(false);
  const [vendor, setVendor] = useState('');
  const [billNo, setBillNo] = useState('');
  const [amounts, setAmounts] = useState<TaxTriple>(EMPTY_TAX_TRIPLE);
  const [billedOn, setBilledOn] = useState('');
  const [dueOn, setDueOn] = useState('');
  const [debitAccountCode, setDebitAccountCode] = useState('');
  const [memo, setMemo] = useState('');
  const [savingBill, setSavingBill] = useState(false);

  const running = useRef<(() => void) | null>(null);

  const load = useCallback(() => {
    running.current?.();
    let live = true;
    running.current = () => {
      live = false;
    };
    setError(null);
    callOp('list_payables', {}, undefined, payablesResult)
      .then((d) => {
        if (!live) return;
        setData(d);
        setBilledOn((v) => v || businessToday());
        setPicked((p) => p.filter((no) => d.bills.some((b) => b.bill_no === no && b.state === 'open')));
      })
      .catch((e: Error) => live && setError(e.message));
    return () => running.current?.();
  }, []);

  useEffect(load, [load, reloadKey]);

  async function saveBill() {
    if (
      !vendor.trim() ||
      !billNo.trim() ||
      (!amounts.total.trim() && !amounts.net.trim()) ||
      !billedOn ||
      !dueOn ||
      !debitAccountCode
    ) {
      onToast('供應商、帳單號碼、金額、帳單日期、到期日與借方科目都要填寫');
      return;
    }
    setSavingBill(true);
    try {
      const d = await callOp(
                        'create_ap_bill',
                        {
            vendor: vendor.trim(),
            bill_no: billNo.trim(),
            billed_on: billedOn,
            ...(amounts.total.trim() ? { amount: amounts.total.trim() } : {}),
            ...(amounts.net.trim() ? { net_amount: amounts.net.trim() } : {}),
            ...(amounts.tax.trim() ? { tax_amount: amounts.tax.trim() } : {}),
            due_on: dueOn,
            debit_account_code: debitAccountCode,
            memo: memo.trim(),
          },
                        `新增應付帳單 ${vendor.trim()}`,
                        newApBill,
                      );
      onToast(
        `已建立應付帳單 ${d.bill.bill_no}，${money(d.bill.amount)}；傳票 ${d.bill.voucher_no} 待過帳，過帳後列入未付合計`,
      );
      setVendor('');
      setBillNo('');
      setAmounts(EMPTY_TAX_TRIPLE);
      setBilledOn(businessToday());
      setDueOn('');
      setDebitAccountCode('');
      setMemo('');
      setDrawer(false);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingBill(false);
    }
  }

  function toggle(b: ApBill) {
    if (!canPay || b.state !== 'open' || !b.recognised) return;
    setPicked((p) => (p.includes(b.bill_no) ? p.filter((x) => x !== b.bill_no) : [...p, b.bill_no]));
  }

  const bills = data?.bills ?? [];
  const chosen = bills.filter((b) => picked.includes(b.bill_no));
  const total = sumCents(chosen.map((b) => b.outstanding));

  async function submit() {
    if (chosen.length === 0) return;
    setBusy(true);
    try {
      const d = await callOp(
                        'create_payment_batch',
                        { bill_nos: picked },
                        `付款 ${chosen.length} 筆`,
                        newPaymentBatch,
                      );
      onToast(
        `付款單 ${d.batch.batch_no} 已送出，${money(d.batch.total)}；傳票 ${d.batch.voucher_no} 待過帳，待出納執行匯款`,
      );
      setPicked([]);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <h1 className="page-title">應付帳款</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          {canPay ? (
            <div className="card pay-bar">
              <div>
                <div className="pay-bar__kicker">已勾選 {chosen.length} 筆，合計</div>
                <div className="pay-bar__total num">{money(total)}</div>
              </div>
              <span className="spacer" />

              <div className="pay-bar__note">
                實際匯款由出納執行。
              </div>
              <button
                className={`btn${chosen.length === 0 ? ' btn--off' : ''}`}
                disabled={chosen.length === 0 || busy}
                onClick={() => void submit()}
              >
                {busy ? '送出中…' : '送出付款單'}
              </button>
            </div>
          ) : (
            <p className="state-note">付款單由出納送出。</p>
          )}

          <p className={`recon${data.reconciled ? '' : ' recon--off'}`}>
            {data.reconciled ? (
              <>
                未付合計 <span className="num">{money(data.total)}</span> 等於總帳{' '}
                <span className="num">2101 應付帳款</span> 的餘額，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                不相符：明細合計 <span className="num">{money(data.total)}</span>，總帳{' '}
                <span className="num">2101</span> 是 <span className="num">{money(data.gl_balance)}</span>
                ，差 <span className="num">{money(data.difference)}</span> 元。
              </>
            )}
            {!isZero(data.not_yet_posted) && (
              <>
                {' '}另有 <span className="num">{money(data.not_yet_posted)}</span> 的帳單，來源傳票尚未過帳。
              </>
            )}
          </p>

          <div className="toolbar">
            <span className="spacer" />
            <button className="btn btn--compact" onClick={() => setDrawer(true)}>
              ＋ 新增應付帳單
            </button>
          </div>

          <div className="table-scroll">
            <table className="table table--inset">
              <thead>
                <tr>
                  {canPay && <th className="col-check" />}
                  <th>供應商</th>
                  <th>內容</th>
                  <th>付款日</th>
                  <th className="align-right">金額</th>
                </tr>
              </thead>
              <tbody>
                {bills.map((b) => {
                  const on = picked.includes(b.bill_no);
                  const pickable = canPay && b.state === 'open' && b.recognised;
                  return (
                    <tr
                      key={b.bill_no}
                      className={`${pickable ? 'row--pickable' : ''}${
                        b.recognised ? '' : ' row--waiting'
                      }`}
                      onClick={() => toggle(b)}
                    >
                      {canPay && (
                        <td>
                          <span className={`checkbox${on ? ' checkbox--on' : ''}`}>
                            {on ? '✓' : ''}
                          </span>
                        </td>
                      )}
                      <td>
                        {b.vendor}
                        {!b.recognised && <span className="tag tag--pale">來源傳票未過帳</span>}
                        {b.state === 'paying' && (
                          <span className="tag tag--warn tag--static">{b.batch_no} 已送出</span>
                        )}
                      </td>
                      <td className="cell-accounts">{b.memo}</td>
                      <td>
                        <span className={b.days_overdue > 0 ? 'due due--over' : 'due'}>
                          {b.due_on.slice(5).replace('-', '/')}
                        </span>
                        {b.days_overdue > 0 && <span className="cell-hint">已逾期</span>}
                      </td>
                      <td className="num align-right">{money(b.outstanding)}</td>
                    </tr>
                  );
                })}
                {bills.length === 0 && (
                  <tr>
                    <td colSpan={canPay ? 5 : 4} className="cell-muted">
                      目前沒有待付帳款。
                      <button className="link-cell" onClick={() => setDrawer(true)}>
                        新增應付帳單
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          {canPay && (
            <p className="hint">
              送出付款單會開立一張借應付帳款、貸銀行存款的傳票，每張帳單一行。
            </p>
          )}
        </>
      )}

      {drawer && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">新增應付帳單</div>
                <div className="drawer__subtitle">這不會直接匯款</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(false)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              <div className="field-grid">
                <label className="field">
                  供應商
                  <input className="input" value={vendor} onChange={(e) => setVendor(e.target.value)} />
                </label>
                <label className="field">
                  帳單號碼
                  <input
                    className="input num"
                    value={billNo}
                    placeholder={docNumberHint('AP-')}
                    onChange={(e) => setBillNo(e.target.value)}
                  />
                </label>
                <TaxAmountFields
                  label="應付總額"
                  taxLabel="進項稅額"
                  value={amounts}
                  rate={data?.vat.rate ?? null}
                  ratePercent={data?.vat.rate_percent ?? null}
                  onChange={setAmounts}
                />
                <label className="field">
                  帳單日期
                  <DateField
                    className="input"
                    value={billedOn}
                    onChange={(v) => setBilledOn(v)}
                  />
                </label>
                <label className="field">
                  到期日
                  <DateField
                    className="input"
                    value={dueOn}
                    onChange={(v) => setDueOn(v)}
                  />
                </label>
                <label className="field field--full">
                  借方科目（費用、存貨、預付款或固定資產）
                  <select
                    className="input"
                    value={debitAccountCode}
                    onChange={(e) => setDebitAccountCode(e.target.value)}
                  >
                    <option value="">請選擇</option>
                    {(data?.debit_accounts ?? []).map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field field--full">
                  帳單內容
                  <input className="input" value={memo} onChange={(e) => setMemo(e.target.value)} />
                </label>
              </div>
            </div>
            <div className="drawer__foot">
              <span className="drawer__foot-note">
                同時產生待過帳傳票（借指定科目與 1284 進項稅額／貸 2101 應付帳款）
              </span>
              <button
                className={`btn${savingBill ? ' btn--off' : ''}`}
                disabled={savingBill}
                onClick={() => void saveBill()}
              >
                {savingBill ? '存檔中…' : '建立帳單'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
