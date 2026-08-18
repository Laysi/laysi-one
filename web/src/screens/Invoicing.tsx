import { useCallback, useEffect, useState } from 'react';
import {
  addDecimals,
  callOp,
  money,
  multiplyToDollars,
  OpFailed,
  type EInvoice,
  type EInvoiceList,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function tagClass(e: EInvoice): string {
  if (e.upload_status === 'failed') return 'tag tag--bad';
  if (e.upload_status === 'void') return 'tag tag--pale tag--static';
  return 'tag tag--static';
}

export function Invoicing({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<EInvoiceList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [buyer, setBuyer] = useState('');
  const [item, setItem] = useState('');
  const [amount, setAmount] = useState('');

  const load = useCallback(() => {
    setError(null);
    callOp<EInvoiceList>('list_einvoices')
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  const rate = data?.vat.rate ?? null;
  const tax = rate && amount.trim() ? multiplyToDollars(amount, rate) : null;
  const total = tax ? addDecimals(amount, tax) : null;

  async function issue() {
    if (!total) {
      onToast('請先填寫未稅金額');
      return;
    }
    setBusy('issue');
    try {
      const d = await callOp<{
        invoice: { invoice_no: string; total_amount: string; tax_amount: string; vat_version: string; remaining: number };
        voucher: { no: string };
      }>('issue_einvoice', { buyer: buyer.trim() || '未填買方', item: item.trim(), net_amount: amount.trim() },
        `開立發票予${buyer.trim() || '客戶'}`);
      onToast(
        `${d.invoice.invoice_no} 已開立，含稅 ${money(d.invoice.total_amount)}（營業稅 ${money(d.invoice.tax_amount)} 依營業稅規則 ${d.invoice.vat_version}）；傳票 ${d.voucher.no} 待過帳`,
      );
      setBuyer('');
      setItem('');
      setAmount('');
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function retry(e: EInvoice) {
    setBusy(e.invoice_no);
    try {
      await callOp('retry_einvoice_upload', { invoice_no: e.invoice_no }, `重新上傳 ${e.invoice_no}`);
      onToast(`${e.invoice_no} 已重新排入上傳佇列；財政部接收後狀態才會轉為已上傳`);
      load();
    } catch (err) {
      onToast(err instanceof OpFailed ? err.error.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div>
      <h1 className="page-title">電子發票</h1>
      <p className="page-lede">開立後排入上傳佇列；上傳失敗者留在本頁供重新送出。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <div className="split-columns split-columns--wide-left">
          <section>
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th>字軌號碼</th>
                    <th>買方</th>
                    <th className="align-right">含稅金額</th>
                    <th className="align-right">上傳狀態</th>
                  </tr>
                </thead>
                <tbody>
                  {data.invoices.map((e) => (
                    <tr key={e.invoice_no} className={e.recognised ? undefined : 'row--waiting'}>
                      <td className="num cell-nowrap">
                        {e.invoice_no}
                        <span className="cell-hint">{e.issued_on.slice(5).replace('-', '/')}</span>
                      </td>
                      <td className="cell-accounts">
                        {e.buyer}
                        {!e.recognised && <span className="tag tag--pale">開票傳票未過帳</span>}
                      </td>
                      <td className="num align-right cell-nowrap">
                        {money(e.total_amount)}
                        <span className="cell-hint cell-hint--block">稅 {money(e.tax_amount)}</span>
                      </td>
                      <td className="align-right">
                        {e.upload_status === 'failed' ? (
                          <button
                            className="tag tag--warn"
                            disabled={busy === e.invoice_no}
                            onClick={() => void retry(e)}
                            title={e.last_error ?? undefined}
                          >
                            {busy === e.invoice_no ? '排隊中…' : '上傳失敗 · 重送'}
                          </button>
                        ) : (
                          <span className={tagClass(e)}>{e.status_label}</span>
                        )}
                      </td>
                    </tr>
                  ))}
                  {data.invoices.length === 0 && (
                    <tr>
                      <td colSpan={4} className="cell-muted">
                        本字軌尚未開立發票。
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>

            <p className="hint">
              字軌 {data.track.track} {data.track_label}，剩{' '}
              <span className="num">{data.track.remaining}</span> 號可用。作廢號碼也計入已使用。
            </p>

            {data.invoices.some((e) => e.last_error) && (
              <p className="recon recon--off">
                {data.failed_count} 張被平台退回：
                {data.invoices.find((e) => e.last_error)?.last_error}
              </p>
            )}
          </section>

          <section className="card form-card">
            <h3 className="section-title">開一張新發票</h3>
            <label className="field">
              買方統編／名稱
              <input
                className="input"
                value={buyer}
                onChange={(ev) => setBuyer(ev.target.value)}
                placeholder="範例科技股份有限公司"
              />
            </label>
            <label className="field">
              品項
              <input
                className="input"
                value={item}
                onChange={(ev) => setItem(ev.target.value)}
                placeholder="7 月維護服務費"
              />
            </label>
            <label className="field">
              未稅金額
              <input
                className="input num"
                value={amount}
                onChange={(ev) => setAmount(ev.target.value)}
                placeholder="45000"
                inputMode="decimal"
              />
            </label>

            <div className="form-line">
              <span className="cell-muted">營業稅 {data.vat.rate_percent}%</span>
              <span className="num">{tax ? money(tax) : '—'}</span>
            </div>
            <div className="form-line form-line--total">
              <span>含稅總計</span>
              <span className="num form-line__amount">{total ? money(total) : '—'}</span>
            </div>

            <button className="btn" disabled={busy === 'issue' || !total} onClick={() => void issue()}>
              {busy === 'issue' ? '開立中…' : '開票並排入上傳'}
            </button>

            <p className="form-note">
              開票同時會產生一張待過帳傳票：借 <span className="num">1131 應收帳款</span>、貸{' '}
              <span className="num">4111 銷貨收入</span> 與 <span className="num">2211 代收營業稅</span>，
              並同時建立應收明細，不用另行登錄。
            </p>
            <p className="form-note">
              稅額依開立日適用的營業稅規則計算（目前 <span className="num">{data.vat.version}</span>，
              {data.vat.effective_from} 起），並隨發票一併保存；
              日後稅率改版不影響本張發票的稅額。
            </p>
          </section>
        </div>
      )}
    </div>
  );
}
