import { useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ExpenseClaim,
  type MyExpenseClaimList,
} from '../api/client';

interface Props {
  reloadKey: number;
}

/** Status label for a claim. In-progress ones stand out, rejected ones
 *  more so. */
function statusTag(c: ExpenseClaim): string {
  if (c.status === 'rejected') return 'tag tag--bad tag--static';
  if (c.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function MyExpenses({ reloadKey }: Props) {
  const [data, setData] = useState<MyExpenseClaimList | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Which claim's steps are expanded. One at a time is enough. */
  const [open, setOpen] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp<MyExpenseClaimList>('list_my_expense_claims')
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
        <h1 className="page-title">我的請款</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          本清單先以登入帳號上的 <span className="num">employee_id</span> 取得員工資料，
          再以該筆資料的姓名比對報支單申請人。帳號未連結員工時回覆錯誤，
          不以空清單呈現。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  return (
    <div>
      <h1 className="page-title">我的請款</h1>
      <p className="page-lede">應收款項現況與帳齡分析。</p>

      <div className="card pay-bar">
        <div>
          <div className="pay-bar__kicker">尚未撥付</div>
          <div className="pay-bar__total num">{money(data.waiting_total)}</div>
        </div>
        <span className="spacer" />
        <div className="pay-bar__note">
          {data.employee.name}（{data.employee.emp_no}）累計送出 {data.claims.length} 張，
          其中 {data.waiting_count} 張流程進行中。完成最後一關後才會開立傳票，狀態為待過帳；
          簽核不等同入帳，入帳也不等同撥款。
        </div>
      </div>

      <p className="recon">{data.scope_note}</p>

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
                    {c.document_no && (
                      <span className="cell-hint cell-hint--block num">
                        附件 {c.document_no}
                        {c.document_name && ` · ${c.document_name}`}
                      </span>
                    )}
                  </>
                )}
              </td>
              <td className="cell-accounts">
                {c.account_label ?? '—'}
                {/* The account AI guessed and the account a person decided
                    must be distinguishable, for the claimant too. */}
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

      <p className="hint">
        點選標題可展開單據的簽核關卡與已簽核人員。
        <strong>簽核關卡依單據金額比對簽核規則產生</strong>，
        非寫死於程式碼，因此金額較大的單據會多一道關卡。
      </p>
      <p className="hint">
        本畫面僅顯示本人的單據，且由輸入形狀保證：
        <span className="num">list_my_expense_claims</span> 的輸入{' '}
        <strong>不含 claimant 或任何人員欄位</strong>，比對用的姓名由後端自已驗證的
        session 解析。檢視全公司報支為另一操作，位於財務區。
      </p>
      <p className="hint">
        身分來自 HttpOnly session cookie，<strong>請求標頭無法指定</strong>；
        能否呼叫本操作由後端每次即時查詢角色權限決定。
        「本人送出的單據不得由本人核准」屬報支流程的規則，
        <strong>與權限矩陣分屬兩套機制</strong>：負責人具備全部權限，同樣不得核准本人送出的單據。
      </p>
    </div>
  );
}
