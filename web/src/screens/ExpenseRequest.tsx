import { useCallback, useEffect, useRef, useState } from 'react';
import {
  callOp,
  fileSizeLabel,
  MAX_UPLOAD_BYTES,
  money,
  OpFailed,
  readFileBase64,
  type AttachReceiptResult,
  type ExpenseReceipt,
  type MyExpenseClaimList,
  type MyExpenseClaimResult,
} from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onToast: (msg: string) => void;
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
  onChanged: () => void;
}

function today(): string {
  const d = new Date();
  const two = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
}

export function ExpenseRequest({ onToast, onGo, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<MyExpenseClaimList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [result, setResult] = useState<MyExpenseClaimResult | null>(null);

  const [title, setTitle] = useState('');
  const [amount, setAmount] = useState('');
  const [spentOn, setSpentOn] = useState(today);
  const [payMethod, setPayMethod] = useState('reimburse');
  const [memo, setMemo] = useState('');
  const [files, setFiles] = useState<File[]>([]);
  const [receipts, setReceipts] = useState<ExpenseReceipt[]>([]);
  const [unattached, setUnattached] = useState<string[]>([]);
  const filePicker = useRef<HTMLInputElement | null>(null);

  const load = useCallback(() => {
    setError(null);
    callOp<MyExpenseClaimList>('list_my_expense_claims')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  const ready = title.trim() !== '' && amount.trim() !== '' && spentOn !== '';

  function addFiles(picked: FileList | null) {
    if (!picked) return;
    const tooBig = Array.from(picked).find((f) => f.size > MAX_UPLOAD_BYTES);
    if (tooBig) {
      onToast(`${tooBig.name} 超過 ${fileSizeLabel(MAX_UPLOAD_BYTES)}，無法上傳`);
    }
    const kept = Array.from(picked).filter((f) => f.size <= MAX_UPLOAD_BYTES);
    setFiles((current) => [
      ...current,
      ...kept.filter((f) => !current.some((c) => c.name === f.name && c.size === f.size)),
    ]);
    if (filePicker.current) filePicker.current.value = '';
  }

  async function attachAll(claimNo: string): Promise<string[]> {
    const attached: ExpenseReceipt[] = [];
    const failed: string[] = [];
    for (const file of files) {
      try {
        const r = await callOp<AttachReceiptResult>(
          'attach_my_expense_receipt',
          {
            claim_no: claimNo,
            file_name: file.name,
            content_base64: await readFileBase64(file),
          },
          `為 ${claimNo} 附上收據 ${file.name}`,
        );
        attached.push(r.receipt);
      } catch {
        failed.push(file.name);
      }
    }
    setReceipts(attached);
    return failed;
  }

  async function submit() {
    setBusy(true);
    setFormError(null);
    setReceipts([]);
    setUnattached([]);
    try {
      const d = await callOp<MyExpenseClaimResult>(
        'submit_my_expense_claim',
        {
          title: title.trim(),
          amount: amount.trim(),
          spent_on: spentOn,
          pay_method: payMethod,
          ...(memo.trim() ? { memo: memo.trim() } : {}),
        },
        `報支${title.trim()} ${amount.trim()} 元`,
      );
      setResult(d);
      const failed = await attachAll(d.claim.claim_no);
      setUnattached(failed);
      onToast(
        failed.length > 0
          ? `${d.claim.claim_no} 已送出，但有 ${failed.length} 個檔案沒附上`
          : `${d.claim.claim_no} 已送出：${d.claim.title} ${money(d.claim.amount)}`,
      );
      setTitle('');
      setAmount('');
      setSpentOn(today());
      setPayMethod('reimburse');
      setMemo('');
      setFiles([]);
      onChanged();
      load();
    } catch (e) {
      const msg = e instanceof OpFailed ? e.error.message : String(e);
      setFormError(msg);
      onToast(msg);
    } finally {
      setBusy(false);
    }
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">請款申請</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          你的帳號尚未對應員工編號，請管理員完成連結後再送出請款單。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  return (
    <div>
      <h1 className="page-title">請款申請</h1>
      <p className="page-lede">先由會計確認費用科目，再依金額決定要經過幾關。</p>

      <div className="stat-grid stat-grid--two">
        <div className="card stat-card">
          <div className="stat-card__kicker">尚未撥付</div>
          <div className="stat-card__value num">{money(data.waiting_total)}</div>
          <div className="stat-card__note">{data.waiting_count} 張進行中</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">累計送出</div>
          <div className="stat-card__value num">{data.claims.length}</div>
          <div className="stat-card__note">張</div>
        </div>
      </div>

      <div className="split-columns">
        <section>
          <h3 className="section-title">申請內容</h3>
          <div className="card form-card">
            <label className="field">
              報支項目
              <input
                className="input"
                placeholder="例如 客戶拜訪高鐵票"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
              />
            </label>

            <div className="field-row">
              <label className="field field--grow">
                金額
                <input
                  className="input"
                  inputMode="decimal"
                  placeholder="含稅"
                  value={amount}
                  onChange={(e) => setAmount(e.target.value)}
                />
              </label>
              <label className="field field--date">
                消費日期
                <input
                  className="input input--date"
                  type="date"
                  value={spentOn}
                  onChange={(e) => setSpentOn(e.target.value)}
                />
              </label>
            </div>

            <label className="field">
              付款方式
              <select
                className="input"
                value={payMethod}
                onChange={(e) => setPayMethod(e.target.value)}
              >
                <option value="reimburse">月底彙總匯款</option>
                <option value="petty_cash">已由零用金支付</option>
              </select>
            </label>

            <label className="field">
              備註
              <input
                className="input"
                placeholder="選填"
                value={memo}
                onChange={(e) => setMemo(e.target.value)}
              />
            </label>

            <div className="field">
              收據
              <input
                ref={filePicker}
                className="file-input"
                type="file"
                multiple
                onChange={(e) => addFiles(e.target.files)}
              />
              {files.length > 0 && (
                <ul className="file-list">
                  {files.map((f) => (
                    <li className="file-list__item" key={`${f.name}-${f.size}`}>
                      <span className="file-list__name">{f.name}</span>
                      <span className="cell-hint num">{fileSizeLabel(f.size)}</span>
                      <button
                        className="link-cell"
                        onClick={() => setFiles((c) => c.filter((x) => x !== f))}
                      >
                        移除
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <button className="btn" disabled={busy || !ready} onClick={() => void submit()}>
              {busy ? '送出中…' : '送出請款單'}
            </button>

            {formError && <p className="state-note state-note--error">{formError}</p>}

            <p className="form-note">
              申請人為 {data.employee.name}（{data.employee.emp_no}）；
              費用科目由會計確認，不在此填寫。收據單檔上限{' '}
              {fileSizeLabel(MAX_UPLOAD_BYTES)}，送出後仍可在「我的請款」補附。
            </p>
          </div>
        </section>

        <section>
          <h3 className="section-title">送出結果</h3>
          {result ? (
            <>
              <div className="card doc-card">
                <div className="doc-card__head">
                  <h3 className="section-title doc-card__title">{result.claim.claim_no}</h3>
                  <span className="doc-card__badge">{result.claim.step_label}</span>
                </div>
                <div className="doc-row">
                  <span>{result.claim.title}</span>
                  <span className="num">{money(result.claim.amount)}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>消費日期</span>
                  <span className="num">{result.claim.spent_on}</span>
                </div>
                <div className="doc-row doc-row--sub">
                  <span>付款方式</span>
                  <span>{result.claim.pay_method_label}</span>
                </div>

                {receipts.map((r) => (
                  <div className="doc-row doc-row--sub" key={r.doc_no}>
                    <span>收據 {r.file_name}</span>
                    <span className="num">{r.size_label}</span>
                  </div>
                ))}

                {result.claim.required_steps.map((s) => (
                  <div className="doc-row doc-row--sub" key={s.step_no}>
                    <span>
                      第 {s.step_no} 關 {s.label}
                    </span>
                    <span>{s.done ? '已完成' : (s.reason ?? '')}</span>
                  </div>
                ))}
              </div>

              {unattached.length > 0 && (
                <p className="state-note state-note--error">
                  請款單已建立，但 {unattached.join('、')} 沒有附上。
                  請到「我的請款」重新附一次，不要重送整張單。
                </p>
              )}
              <button
                className="btn btn--compact btn--secondary btn--spaced"
                onClick={() => onGo('meexp')}
              >
                查看我的請款 →
              </button>
            </>
          ) : (
            <p className="state-note">送出後，單號與要經過的關卡會顯示在這裡。</p>
          )}
        </section>
      </div>

      <p className="hint">
        關卡依單據金額比對簽核規則產生，金額較大的單據會多一道關卡。
        完成最後一關才會開立傳票。
      </p>
    </div>
  );
}
