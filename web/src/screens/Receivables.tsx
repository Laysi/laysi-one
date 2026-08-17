import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  OpFailed,
  type ArAging,
  type ArInvoice,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

interface RevenueAccount {
  code: string;
  name: string;
  kind: string;
  is_postable: boolean;
  is_active: boolean;
}

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

  const [drawer, setDrawer] = useState(false);
  const [revenueAccounts, setRevenueAccounts] = useState<RevenueAccount[] | null>(null);
  const [customer, setCustomer] = useState('');
  const [invoiceNo, setInvoiceNo] = useState('');
  const [amount, setAmount] = useState('');
  const [dueOn, setDueOn] = useState('');
  const [revenueAccountCode, setRevenueAccountCode] = useState('');
  const [memo, setMemo] = useState('');
  const [savingInvoice, setSavingInvoice] = useState(false);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<ArAging>('get_ar_aging'),
      callOp<{ invoices: ArInvoice[] }>('list_receivables', { only_overdue: onlyOverdue }),
    ])
      .then(([a, l]) => {
        setAging(a);
        setRows(l.invoices);
      })
      .catch((e: Error) => setError(e.message));
  }, [onlyOverdue]);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!drawer || revenueAccounts) return;
    callOp<{ accounts: RevenueAccount[] }>('list_accounts')
      .then((d) => {
        const usable = d.accounts.filter((a) => a.kind === 'revenue' && a.is_postable && a.is_active);
        setRevenueAccounts(usable);
        setRevenueAccountCode((code) => code || usable[0]?.code || '');
      })
      .catch(() => setRevenueAccounts([]));
  }, [drawer, revenueAccounts]);

  async function saveInvoice() {
    if (!customer.trim() || !invoiceNo.trim() || !amount.trim() || !dueOn || !revenueAccountCode) {
      onToast('客戶、單號、金額、到期日與收益科目都要填寫');
      return;
    }
    setSavingInvoice(true);
    try {
      const d = await callOp<{
        invoice: { invoice_no: string; customer: string; amount: string; voucher_no: string };
      }>(
        'create_ar_invoice',
        {
          customer: customer.trim(),
          invoice_no: invoiceNo.trim(),
          amount: amount.trim(),
          due_on: dueOn,
          revenue_account_code: revenueAccountCode,
          memo: memo.trim(),
        },
        `新增應收單 ${customer.trim()}`,
      );
      onToast(
        `已建立應收單 ${d.invoice.invoice_no}，${money(d.invoice.amount)}；傳票 ${d.invoice.voucher_no} 待過帳，過帳後列入未收合計`,
      );
      setCustomer('');
      setInvoiceNo('');
      setAmount('');
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

  async function collect(i: ArInvoice) {
    setBusy(i.invoice_no);
    try {
      const d = await callOp<{ receipt: { voucher_no: string; amount: string } }>(
        'record_receipt',
        { invoice_no: i.invoice_no },
        `登記 ${i.customer} 收款 ${money(i.outstanding)}`,
      );
      onToast(
        `已產生收款傳票 ${d.receipt.voucher_no}，${money(d.receipt.amount)} 待過帳；過帳後未收金額才會減少`,
      );
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
      <p className="page-lede">到期日加註底線者已逾票期。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!aging && !error && <p className="state-note">讀取中…</p>}

      {aging && (
        <>
          <div className="stat-grid stat-grid--five">
            {aging.buckets.map((b) => (
              <div className="card stat-card" key={b.label}>
                <div className="stat-card__kicker">{b.label}</div>
                <div className="stat-card__value num">{money(b.value)}</div>
                <div className="stat-card__note">{b.count} 筆</div>
              </div>
            ))}
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">合計未收</div>
              <div className="stat-card__value num">{money(aging.total)}</div>
              <div className="stat-card__note">到 {aging.as_of} 為止</div>
            </div>
          </div>

          <p className={`recon${aging.reconciled ? '' : ' recon--off'}`}>
            {aging.reconciled ? (
              <>
                這裡的合計就是總帳 <span className="num">1131 應收帳款</span> 的餘額{' '}
                <span className="num">{money(aging.gl_balance)}</span>，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                對不上：明細合計 <span className="num">{money(aging.total)}</span>，總帳{' '}
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
                          onClick={() => void collect(i)}
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
                      沒有符合條件的未收帳款。請由上方「＋ 新增應收單」建立第一筆。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">
            「登記收款」會開立一張借銀行存款、貸應收帳款的傳票，狀態為待過帳，
            而非僅將該列標記為已收。若狀態可改而總帳不動，帳務即產生分歧。
          </p>
        </>
      )}

      {drawer && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">新增應收單</div>
                <div className="drawer__subtitle">
                  同時會產生一張待過帳傳票（借應收帳款／貸指定收益科目）；適用不需電子發票的
                  B2B 月結與國外客戶
                </div>
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
                </label>
                <label className="field">
                  應收單號
                  <input
                    className="input num"
                    value={invoiceNo}
                    placeholder="AR-2607-030"
                    onChange={(e) => setInvoiceNo(e.target.value)}
                  />
                </label>
                <label className="field">
                  應收總額
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={amount}
                    onChange={(e) => setAmount(e.target.value)}
                  />
                </label>
                <label className="field">
                  到期日
                  <input className="input" type="date" value={dueOn} onChange={(e) => setDueOn(e.target.value)} />
                </label>
                <label className="field field--full">
                  貸方收益科目
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
                </label>
                <label className="field field--full">
                  內容說明
                  <input className="input" value={memo} onChange={(e) => setMemo(e.target.value)} />
                </label>
              </div>
            </div>
            <div className="drawer__foot">
              <span className="drawer__foot-note">借方固定是 1131 應收帳款</span>
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
