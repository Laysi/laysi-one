import { useCallback, useEffect, useState } from 'react';
import {
  accountList,
  arAging,
  businessToday,
  callOp,
  docNumberHint,
  isZero,
  money,
  newArInvoice,
  newReceipt,
  OpFailed,
  receivableList,
  type Account,
  type ArAging,
  type ArInvoice,
  type VatInfo,
} from '../api/client';
import {
  EMPTY_TAX_TRIPLE,
  TaxAmountFields,
  type TaxTriple,
} from '../components/TaxAmountFields';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

const FIELD_LABEL: Record<string, string> = {
  customer: '客戶名稱',
  invoiceNo: '應收單號',
  amount: '金額',
  issuedOn: '開立日期',
  dueOn: '到期日',
  revenue: '收入科目',
};

function dueNote(i: ArInvoice): string {
  if (i.days_overdue > 0) return `逾期 ${i.days_overdue} 天`;
  if (i.days_overdue === 0) return '本日到期';
  return `尚餘 ${-i.days_overdue} 天`;
}

export function Receivables({ onToast, reloadKey, onChanged }: Props) {
  const [aging, setAging] = useState<ArAging | null>(null);
  const [rows, setRows] = useState<ArInvoice[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [onlyOverdue, setOnlyOverdue] = useState(false);

  const [receipt, setReceipt] = useState<ArInvoice | null>(null);
  const [receiptAmount, setReceiptAmount] = useState('');
  const [receiptOn, setReceiptOn] = useState('');

  const [drawer, setDrawer] = useState(false);
  const [revenueAccounts, setRevenueAccounts] = useState<Account[] | null>(null);
  const [customer, setCustomer] = useState('');
  const [invoiceNo, setInvoiceNo] = useState('');
  const [amounts, setAmounts] = useState<TaxTriple>(EMPTY_TAX_TRIPLE);
  const [vat, setVat] = useState<VatInfo | null>(null);
  const [issuedOn, setIssuedOn] = useState('');
  const [dueOn, setDueOn] = useState('');
  const [revenueAccountCode, setRevenueAccountCode] = useState('');
  const [memo, setMemo] = useState('');
  const [savingInvoice, setSavingInvoice] = useState(false);
  const [missing, setMissing] = useState<string[]>([]);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp('get_ar_aging', {}, undefined, arAging),
      callOp(
        'list_receivables',
        {
          only_overdue: onlyOverdue,
        },
        undefined,
        receivableList,
      ),
    ])
      .then(([a, l]) => {
        setAging(a);
        setRows(l.invoices);
        setVat(l.vat);
        setIssuedOn((v) => v || businessToday());
      })
      .catch((e: Error) => setError(e.message));
  }, [onlyOverdue]);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!drawer || revenueAccounts) return;
    callOp('list_accounts', {}, undefined, accountList)
      .then((d) => {
        const usable = d.accounts.filter((a) => a.kind === 'revenue' && a.is_postable && a.is_active);
        setRevenueAccounts(usable);
        setRevenueAccountCode((code) => code || usable[0]?.code || '');
      })
      .catch(() => setRevenueAccounts([]));
  }, [drawer, revenueAccounts]);

  async function saveInvoice() {
    const blank: string[] = [];
    if (!customer.trim()) blank.push('customer');
    if (!invoiceNo.trim()) blank.push('invoiceNo');
    if (!amounts.total.trim() && !amounts.net.trim()) blank.push('amount');
    if (!issuedOn) blank.push('issuedOn');
    if (!dueOn) blank.push('dueOn');
    if (!revenueAccountCode) blank.push('revenue');
    setMissing(blank);
    if (blank.length > 0) {
      onToast(`還要填：${blank.map((k) => FIELD_LABEL[k]).join('、')}`);
      return;
    }
    setSavingInvoice(true);
    try {
      const d = await callOp(
                        'create_ar_invoice',
                        {
            customer: customer.trim(),
            invoice_no: invoiceNo.trim(),
            issued_on: issuedOn,
            ...(amounts.total.trim() ? { amount: amounts.total.trim() } : {}),
            ...(amounts.net.trim() ? { net_amount: amounts.net.trim() } : {}),
            ...(amounts.tax.trim() ? { tax_amount: amounts.tax.trim() } : {}),
            due_on: dueOn,
            revenue_account_code: revenueAccountCode,
            memo: memo.trim(),
          },
                        `新增應收單 ${customer.trim()}`,
                        newArInvoice,
                      );
      onToast(
        `已建立應收單 ${d.invoice.invoice_no}，${money(d.invoice.amount)}；傳票 ${d.invoice.voucher_no} 待過帳，過帳後列入未收合計`,
      );
      setCustomer('');
      setInvoiceNo('');
      setAmounts(EMPTY_TAX_TRIPLE);
      setIssuedOn(businessToday());
      setDueOn('');
      setMemo('');
      setDrawer(false);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingInvoice(false);
    }
  }

  function openReceipt(i: ArInvoice) {
    setReceipt(i);
    setReceiptAmount(i.outstanding);
    setReceiptOn(businessToday());
  }

  async function collect() {
    const i = receipt;
    if (!i) return;
    if (!receiptAmount.trim() || !receiptOn) {
      onToast('實收金額與收款日期都要填寫');
      return;
    }
    setBusy(i.invoice_no);
    try {
      const d = await callOp(
                        'record_receipt',
                        { invoice_no: i.invoice_no, amount: receiptAmount.trim(), received_on: receiptOn },
                        `登記 ${i.customer} 收款 ${money(receiptAmount.trim())}`,
                        newReceipt,
                      );
      onToast(
        `已產生收款傳票 ${d.receipt.voucher_no}，${money(d.receipt.amount)} 待過帳；過帳後未收金額才會減少`,
      );
      setReceipt(null);
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
      <h1 className="page-title">應收帳款</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!aging && !error && <p className="state-note">讀取中…</p>}

      {aging && (
        <>
          <div className="stat-grid stat-grid--five">
            {aging.buckets.map((b) => (
              <div className="card stat-card" key={b.label}>
                <div className="stat-card__kicker">{b.label}</div>
                <div className="stat-card__value num">{money(b.value)}</div>
                <div className="stat-card__note num">{b.count} 筆</div>
              </div>
            ))}
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">合計未收</div>
              <div className="stat-card__value num">{money(aging.total)}</div>
              <div className="stat-card__note num">到 {aging.as_of} 為止</div>
            </div>
          </div>

          <p className={`recon${aging.reconciled ? '' : ' recon--off'}`}>
            {aging.reconciled ? (
              <>
                本頁合計等於總帳 <span className="num">1131 應收帳款</span> 的餘額{' '}
                <span className="num">{money(aging.gl_balance)}</span>，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                不相符：明細合計 <span className="num">{money(aging.total)}</span>，總帳{' '}
                <span className="num">1131</span> 是 <span className="num">{money(aging.gl_balance)}</span>
                ，差額 <span className="num">{money(aging.difference)}</span> 元，請先暫緩出具帳齡表。
              </>
            )}
            {!isZero(aging.not_yet_posted) && (
              <>
                {' '}另有 <span className="num">{money(aging.not_yet_posted)}</span> 的發票，其開票傳票尚未過帳，
                故未計入。
              </>
            )}
          </p>
        </>
      )}

      {rows && (
        <>
          <div className="toolbar">
            <button
              className={`chip${onlyOverdue ? ' chip--on' : ''}`}
              onClick={() => setOnlyOverdue((v) => !v)}
            >
              僅顯示逾期
            </button>
            <span className="spacer" />
            {aging && aging.overdue_count > 0 && (
              <span className="section-note">{aging.overdue_count} 家已逾票期</span>
            )}
            <button className="btn btn--compact" onClick={() => setDrawer(true)}>
              ＋ 新增應收單
            </button>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>客戶</th>
                  <th>發票號碼</th>
                  <th>到期日</th>
                  <th className="align-right">未收金額</th>
                  <th className="align-right">動作</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((i) => (
                  <tr key={i.invoice_no} className={i.recognised ? undefined : 'row--waiting'}>
                    <td>
                      {i.customer}
                      {!i.recognised && (
                        <span className="tag tag--pale">開票傳票未過帳</span>
                      )}
                    </td>
                    <td className="num cell-accounts">{i.invoice_no}</td>
                    <td>
                      <span className={i.days_overdue > 0 ? 'due due--over' : 'due'}>
                        {i.due_on.slice(5).replace('-', '/')}
                      </span>
                      <span className="cell-hint">{dueNote(i)}</span>
                    </td>
                    <td className="num align-right">{money(i.outstanding)}</td>
                    <td className="align-right">
                      {i.state === 'settled' && <span className="cell-muted">已收款</span>}
                      {i.state === 'receiving' && (
                        <span className="tag tag--warn tag--static">
                          {i.receipt_voucher_no} 待過帳
                        </span>
                      )}
                      {i.state === 'open' && (
                        <button
                          className="btn btn--compact"
                          disabled={busy === i.invoice_no || !i.recognised}
                          onClick={() => openReceipt(i)}
                        >
                          {busy === i.invoice_no ? '處理中…' : '登記收款'}
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={5} className="cell-muted">
                      沒有符合條件的未收帳款。
                      <button className="link-cell" onClick={() => setDrawer(true)}>
                        新增應收單
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">
            「登記收款」會開立一張借銀行存款、貸應收帳款的傳票，狀態為待過帳。
          </p>
        </>
      )}

      {receipt && (
        <div className="overlay overlay--center" onClick={() => setReceipt(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">登記收款</div>
                <div className="drawer__subtitle">
                  {receipt.customer}　{receipt.invoice_no}　未收 {money(receipt.outstanding)}
                </div>
              </div>
              <button className="btn btn--close" onClick={() => setReceipt(null)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              <div className="field-grid">
                <label className="field">
                  實收金額
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={receiptAmount}
                    onChange={(e) => setReceiptAmount(e.target.value)}
                  />
                </label>
                <label className="field">
                  收款日期
                  <DateField value={receiptOn} onChange={(v) => setReceiptOn(v)} />
                </label>
              </div>
              <p className="form-note">
                收款進入 1102 銀行存款。實收少於未收金額時，差額留在本單，傳票過帳後可再登記下一次收款。
              </p>
            </div>
            <div className="drawer__foot">
              <button className="btn btn--secondary" onClick={() => setReceipt(null)}>
                取消
              </button>
              <button
                className="btn"
                disabled={busy === receipt.invoice_no}
                onClick={() => void collect()}
              >
                {busy === receipt.invoice_no ? '處理中…' : '產生收款傳票'}
              </button>
            </div>
          </div>
        </div>
      )}

      {drawer && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">新增應收單</div>
                <div className="drawer__subtitle">營業稅由稅率自動拆出，零稅率填 0</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(false)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              <div className="field-grid">
                <label className="field">
                  客戶名稱
                  <input className="input" value={customer} onChange={(e) => setCustomer(e.target.value)} />
                  {missing.includes('customer') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <label className="field">
                  應收單號
                  <input
                    className="input num"
                    value={invoiceNo}
                    placeholder={docNumberHint('AR-')}
                    onChange={(e) => setInvoiceNo(e.target.value)}
                  />
                  {missing.includes('invoiceNo') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <TaxAmountFields
                  label="應收總額"
                  taxLabel="代收營業稅"
                  value={amounts}
                  rate={vat?.rate ?? null}
                  ratePercent={vat?.rate_percent ?? null}
                  missing={missing.includes('amount')}
                  onChange={setAmounts}
                />
                <label className="field">
                  開立日期
                  <DateField
                    className="input"
                    value={issuedOn}
                    onChange={(v) => setIssuedOn(v)}
                  />
                  {missing.includes('issuedOn') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <label className="field">
                  到期日
                  <DateField
                    className="input"
                    value={dueOn}
                    onChange={(v) => setDueOn(v)}
                  />
                  {missing.includes('dueOn') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <label className="field field--full">
                  收入科目
                  <select
                    className="input"
                    value={revenueAccountCode}
                    onChange={(e) => setRevenueAccountCode(e.target.value)}
                  >
                    {(revenueAccounts ?? []).map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                  {missing.includes('revenue') && (
                    <span className="field__hint field__hint--missing">未填</span>
                  )}
                </label>
                <label className="field field--full">
                  內容說明
                  <input className="input" value={memo} onChange={(e) => setMemo(e.target.value)} />
                </label>
              </div>
            </div>
            <div className="drawer__foot">
              <span className="drawer__foot-note">
                同時產生待過帳傳票（借 1131 應收帳款／貸指定收益科目與 2211 代收營業稅）
              </span>
              <button
                className={`btn${savingInvoice ? ' btn--off' : ''}`}
                disabled={savingInvoice}
                onClick={() => void saveInvoice()}
              >
                {savingInvoice ? '存檔中…' : '建立應收單'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
