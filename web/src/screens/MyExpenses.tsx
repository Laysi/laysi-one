import { useCallback, useEffect, useRef, useState } from 'react';
import {
  callOp,
  fileSizeLabel,
  MAX_UPLOAD_BYTES,
  money,
  OpFailed,
  readFileBase64,
  type AttachReceiptResult,
  type ExpenseClaim,
  type MyExpenseClaimList,
} from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  onToast: (msg: string) => void;
  reloadKey: number;
}

function statusTag(c: ExpenseClaim): string {
  if (c.status === 'rejected') return 'tag tag--bad tag--static';
  if (c.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function MyExpenses({ onGo, onToast, reloadKey }: Props) {
  const [data, setData] = useState<MyExpenseClaimList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const attachTo = useRef<string | null>(null);
  const filePicker = useRef<HTMLInputElement | null>(null);

  const load = useCallback(() => {
    setError(null);
    callOp<MyExpenseClaimList>('list_my_expense_claims')
      .then(setData)
      .catch((e) => setError(e instanceof OpFailed ? e.error.message : String(e)));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function attach(picked: FileList | null) {
    const claimNo = attachTo.current;
    if (filePicker.current) filePicker.current.value = '';
    if (!picked || picked.length === 0 || !claimNo) return;
    const files = Array.from(picked).filter((f) => f.size <= MAX_UPLOAD_BYTES);
    if (files.length < picked.length) {
      onToast(`有檔案超過 ${fileSizeLabel(MAX_UPLOAD_BYTES)}，未上傳`);
    }
    setUploading(claimNo);
    let done = 0;
    for (const file of files) {
      try {
        await callOp<AttachReceiptResult>(
          'attach_my_expense_receipt',
          {
            claim_no: claimNo,
            file_name: file.name,
            content_base64: await readFileBase64(file),
          },
          `為 ${claimNo} 附上收據 ${file.name}`,
        );
        done += 1;
      } catch (e) {
        onToast(e instanceof OpFailed ? e.error.message : String(e));
      }
    }
    if (done > 0) onToast(`${claimNo} 已附上 ${done} 份收據`);
    setUploading(null);
    load();
  }

  if (error) {
    return (
      <div>
        <h1 className="page-title">我的請款</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          你的帳號尚未對應員工編號。請管理員完成連結後再回到本頁。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  return (
    <div>
      <h1 className="page-title">我的請款</h1>

      <div className="card pay-bar">
        <div>
          <div className="pay-bar__kicker">尚未撥付</div>
          <div className="pay-bar__total num">{money(data.waiting_total)}</div>
        </div>
        <span className="spacer" />
        <div className="pay-bar__note">
          {data.employee.name}（{data.employee.emp_no}）累計送出 {data.claims.length} 張，
          其中 {data.waiting_count} 張流程進行中。完成最後一關後才會開立傳票，狀態為待過帳。
        </div>
      </div>

      <div className="toolbar">
        <button className="btn btn--compact" onClick={() => onGo('meexpnew')}>
          請款申請 →
        </button>
      </div>

      <input
        ref={filePicker}
        className="file-input file-input--hidden"
        type="file"
        multiple
        onChange={(e) => void attach(e.target.files)}
      />

      <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th>報支內容</th>
              <th>科目</th>
              <th className="align-right">金額</th>
              <th>目前關卡</th>
            </tr>
          </thead>
          <tbody>
            {data.claims.map((c) => (
              <tr key={c.claim_no}>
                <td>
                  <button
                    className="link-cell"
                    onClick={() => setOpen(open === c.claim_no ? null : c.claim_no)}
                  >
                    {c.title}
                  </button>
                  <span className="cell-hint cell-hint--block num">
                    {c.claim_no} · {c.spent_on} · {c.pay_method_label}
                    {c.receipt_count > 0 && ` · 收據 ${c.receipt_count} 份`}
                  </span>
                  {open === c.claim_no && (
                    <>
                      <span className="cell-hint cell-hint--block">
                        {c.required_steps.map((s) => (
                          <span key={s.step_no} className="step-chip">
                            {s.done ? '✓' : '○'} {s.label}
                            {s.reason && <span className="step-chip__why">（{s.reason}）</span>}
                          </span>
                        ))}
                      </span>
                      {c.history.length > 0 && (
                        <span className="cell-hint cell-hint--block">
                          {c.history.map((h) => (
                            <span key={`${h.step_no}-${h.decided_at}`} className="step-chip">
                              {h.actor}（{h.actor_role}）
                              {h.decision === 'approved' ? '已簽核' : '已退回'}
                              {h.note && `：${h.note}`}
                            </span>
                          ))}
                        </span>
                      )}
                      {c.memo && <span className="cell-hint cell-hint--block">備註：{c.memo}</span>}
                      {c.receipts.map((r) => (
                        <span className="cell-hint cell-hint--block num" key={r.doc_no}>
                          收據 {r.doc_no} · {r.file_name} · {r.size_label}
                        </span>
                      ))}
                      {c.accepts_receipt && (
                        <span className="cell-hint cell-hint--block">
                          <button
                            className="link-cell"
                            disabled={uploading !== null}
                            onClick={() => {
                              attachTo.current = c.claim_no;
                              filePicker.current?.click();
                            }}
                          >
                            {uploading === c.claim_no
                              ? '上傳中…'
                              : c.receipt_count > 0
                                ? '再附一份收據'
                                : '補上收據'}
                          </button>
                        </span>
                      )}
                    </>
                  )}
                </td>
                <td className="cell-accounts">
                  {c.account_label ?? '—'}

                  {!c.account_confirmed && c.suggested_account_code && (
                    <span className="cell-hint cell-hint--block">AI 建議，尚未確認</span>
                  )}
                </td>
                <td className="num align-right">{money(c.amount)}</td>
                <td>
                  <span className={statusTag(c)}>{c.step_label}</span>
                  {c.next_step && (
                    <span className="cell-hint cell-hint--block">
                      待 {c.next_step.role_name ?? c.next_step.label}
                      {c.next_step.reason && `：${c.next_step.reason}`}
                    </span>
                  )}
                  {c.voucher_no && (
                    <span className="cell-hint cell-hint--block num">
                      {c.voucher_no}
                      {c.voucher_status === 'posted' ? ' 已過帳' : ' 待過帳'}
                    </span>
                  )}
                </td>
              </tr>
            ))}
            {data.claims.length === 0 && (
              <tr>
                <td colSpan={4} className="cell-muted">
                  尚無請款紀錄。
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <p className="hint">
        金額較大的單據會多一道關卡。已核准或已退回的單不再收件。
      </p>
    </div>
  );
}
