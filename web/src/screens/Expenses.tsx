import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ExpenseClaim,
  type ExpenseClaimList,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

function statusTag(c: ExpenseClaim): string {
  if (c.status === 'rejected') return 'tag tag--bad tag--static';
  if (c.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function Expenses({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<ExpenseClaimList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [includeDone, setIncludeDone] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [account, setAccount] = useState<Record<string, string>>({});

  const load = useCallback(() => {
    setError(null);
    callOp<ExpenseClaimList>('list_expense_claims', { include_done: includeDone })
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, [includeDone]);

  useEffect(load, [load, reloadKey]);

  async function decide(c: ExpenseClaim, approve: boolean) {
    const picked = (account[c.claim_no] ?? '').trim();
    setBusy(c.claim_no);
    try {
      if (approve) {
        const d = await callOp<{ claim: ExpenseClaim; voucher: { no: string } | null }>(
          'approve_expense_claim',
          {
            claim_no: c.claim_no,

            ...(picked ? { account_code: picked } : {}),
          },
          `核准 ${c.claimant} 的${c.title}`,
        );
        onToast(
          d.voucher
            ? `${c.claim_no} 已完成全部簽核，產生傳票 ${d.voucher.no}（待過帳）；過帳後才會進總帳`
            : `${c.claim_no} 本關已簽核，尚待 ${d.claim.step_label}`,
        );
      } else {
        await callOp<unknown>('reject_expense_claim', {
          claim_no: c.claim_no,
          note: '資料不齊，請補收據',
        });
        onToast(`${c.claim_no} 已退回給 ${c.claimant}`);
      }
      setAccount((a) => ({ ...a, [c.claim_no]: '' }));
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
      <h1 className="page-title">費用報支</h1>
      <p className="page-lede">收據由 AI 初步分類，會計確認科目後才進入簽核。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && (
        <>
          <div className="card pay-bar">
            <div>
              <div className="pay-bar__kicker">待處理費用報支</div>
              <div className="pay-bar__total num">{money(data.waiting_total)}</div>
            </div>
            <span className="spacer" />
            <div className="pay-bar__note">
              {data.waiting_count} 張尚未完成流程。核准至最後一關才會產生傳票，
              且狀態為待過帳；簽核不等同入帳。
            </div>
          </div>

          <p className="recon">
            簽核關卡依單據金額比對簽核規則產生，門檻來自{data.thresholds_source}。
            僅輪到的關卡可以簽核，老闆可代理任一關卡，
            但<strong>申請人不得核准本人送出的單據，老闆也不例外</strong>。
            無法簽核時，下方會顯示原因。
          </p>

          <div className="toolbar">
            <button
              className={`chip${includeDone ? ' chip--on' : ''}`}
              onClick={() => setIncludeDone((v) => !v)}
            >
              併同顯示已入帳與已退回
            </button>
            <span className="spacer" />
            <span className="section-note">
              簽核流程：{data.flow.map((s) => s.label).join(' → ')}
            </span>
          </div>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>報支內容</th>
                  <th>科目</th>
                  <th className="align-right">金額</th>
                  <th>目前關卡</th>
                  <th className="align-right">動作</th>
                </tr>
              </thead>
              <tbody>
                {data.claims.map((c) => (
                  <tr key={c.claim_no}>
                    <td>
                      <button className="link-cell" onClick={() => setOpen(open === c.claim_no ? null : c.claim_no)}>
                        {c.title}
                      </button>
                      <span className="cell-hint cell-hint--block">
                        {c.claimant}
                        {c.dept_label ? ` · ${c.dept_label}` : ''} ·{' '}
                        {c.spent_on.slice(5).replace('-', '/')} · {c.claim_no}
                      </span>
                      {open === c.claim_no && (
                        <span className="cell-hint cell-hint--block">
                          {c.required_steps.map((s) => (
                            <span key={s.step_no} className="step-chip">
                              {s.done ? '✓' : '○'} {s.label}
                              <span className="step-chip__why">（{s.reason}）</span>
                            </span>
                          ))}
                          {c.memo && <span className="step-chip">備註：{c.memo}</span>}
                        </span>
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
                      {c.voucher_no && (
                        <span className="cell-hint cell-hint--block num">
                          {c.voucher_no}
                          {c.voucher_status === 'posted' ? '已過帳' : '待過帳'}
                        </span>
                      )}
                    </td>
                    <td className="align-right">
                      {c.next_step ? (
                        <span className="cell-actions">

                          {!c.account_confirmed && (
                            <input
                              className="entry-input entry-input--code"
                              placeholder={c.suggested_account_code ?? '科目'}
                              value={account[c.claim_no] ?? ''}
                              onChange={(e) =>
                                setAccount((a) => ({ ...a, [c.claim_no]: e.target.value }))
                              }
                            />
                          )}
                          <button
                            className="btn btn--compact"
                            disabled={busy === c.claim_no}
                            title={`本關由${c.next_step.role_name ?? '其他角色'}負責，其他角色簽核會被拒絕`}
                            onClick={() => void decide(c, true)}
                          >
                            {busy === c.claim_no ? '處理中…' : '簽核'}
                          </button>
                          <button
                            className="btn btn--compact btn--secondary"
                            disabled={busy === c.claim_no}
                            onClick={() => void decide(c, false)}
                          >
                            退回
                          </button>
                        </span>
                      ) : (
                        <span className="cell-muted">
                          {c.status === 'rejected' ? '已退回' : '流程已完成'}
                        </span>
                      )}
                    </td>
                  </tr>
                ))}
                {data.claims.length === 0 && (
                  <tr>
                    <td colSpan={5} className="cell-muted">
                      沒有待處理的報支單。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">
            最後一關簽核後才會開立傳票（借費用科目、貸應付帳款或零用金），狀態為待過帳。
          </p>

        </>
      )}
    </div>
  );
}
