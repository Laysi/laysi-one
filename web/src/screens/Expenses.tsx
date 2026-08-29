import { useCallback, useEffect, useState } from 'react';
import {
  accountList,
  businessToday,
  callOp,
  dimensionList,
  expenseApproval,
  expenseClaimList,
  expenseClaimResult,
  money,
  OpFailed,
  type AccountList,
  type DimensionList,
  type ExpenseClaim,
  type ExpenseClaimList,
} from '../api/client';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
  canSubmit: boolean;
}

function statusTag(c: ExpenseClaim): string {
  if (c.status === 'rejected') return 'tag tag--bad tag--static';
  if (c.status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function Expenses({ onToast, reloadKey, onChanged, canSubmit }: Props) {
  const [data, setData] = useState<ExpenseClaimList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [includeDone, setIncludeDone] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [account, setAccount] = useState<Record<string, string>>({});
  const [accounts, setAccounts] = useState<AccountList | null>(null);
  const [depts, setDepts] = useState<DimensionList | null>(null);

  const [drawer, setDrawer] = useState(false);
  const [title, setTitle] = useState('');
  const [amount, setAmount] = useState('');
  const [spentOn, setSpentOn] = useState('');
  const [claimant, setClaimant] = useState('');
  const [deptCode, setDeptCode] = useState('');
  const [payMethod, setPayMethod] = useState('reimburse');
  const [memo, setMemo] = useState('');
  const [saving, setSaving] = useState(false);

  const load = useCallback(() => {
    setError(null);
    callOp('list_expense_claims', { include_done: includeDone }, undefined, expenseClaimList)
      .then(setData)
      .catch((e: Error) => setError(e.message));
  }, [includeDone]);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    callOp('list_accounts', {}, undefined, accountList)
      .then(setAccounts)
      .catch(() => setAccounts(null));
  }, []);

  useEffect(() => {
    if (!drawer || depts) return;
    callOp('list_dimensions', {}, undefined, dimensionList)
      .then(setDepts)
      .catch(() => setDepts(null));
  }, [drawer, depts]);

  function openDrawer() {
    setTitle('');
    setAmount('');
    setSpentOn(businessToday());
    setClaimant('');
    setDeptCode('');
    setPayMethod('reimburse');
    setMemo('');
    setDrawer(true);
  }

  async function saveClaim() {
    if (!title.trim() || !amount.trim() || !spentOn || !claimant.trim()) {
      onToast('事由、金額、支出日期與申請人都要填寫');
      return;
    }
    setSaving(true);
    try {
      const d = await callOp(
                        'submit_expense_claim',
                        {
            title: title.trim(),
            amount: amount.trim(),
            spent_on: spentOn,
            claimant: claimant.trim(),
            pay_method: payMethod,
            ...(deptCode ? { dept_code: deptCode } : {}),
            ...(memo.trim() ? { memo: memo.trim() } : {}),
          },
                        `代 ${claimant.trim()} 建立報支單 ${money(amount.trim())}`,
                        expenseClaimResult,
                      );
      onToast(`已建立報支單 ${d.claim.claim_no}，目前關卡：${d.claim.step_label}`);
      setDrawer(false);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function decide(c: ExpenseClaim, approve: boolean) {
    const picked = (account[c.claim_no] ?? c.suggested_account_code ?? '').trim();
    setBusy(c.claim_no);
    try {
      if (approve) {
        const d = await callOp(
                          'approve_expense_claim',
                          {
              claim_no: c.claim_no,
  
              ...(picked ? { account_code: picked } : {}),
            },
                          `核准 ${c.claimant} 的${c.title}`,
                          expenseApproval,
                        );
        onToast(
          d.voucher
            ? `${c.claim_no} 已完成全部簽核，產生傳票 ${d.voucher.no}（待過帳）；過帳後才進總帳`
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
            <div className="pay-bar__note">{data.waiting_count} 張尚未完成流程。</div>
          </div>

          <p className="recon">
            簽核門檻來自{data.thresholds_source}。
          </p>

          <div className="toolbar">
            <button
              className={`chip${includeDone ? ' chip--on' : ''}`}
              onClick={() => setIncludeDone((v) => !v)}
            >
              併同顯示已入帳與已退回
            </button>
            {canSubmit && (
              <button className="btn btn--compact" onClick={openDrawer}>
                ＋ 建立報支單
              </button>
            )}
            <span className="spacer" />
            <span className="section-note">
              簽核流程：
              {data.flow
                .map((s) =>
                  s.min_amount ? `${s.label}（超過 ${money(s.min_amount)}）` : s.label,
                )
                .join(' → ')}
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
                        <span className="cell-hint cell-hint--block">尚未確認</span>
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
                      {c.can_approve ? (
                        <span className="cell-actions">

                          {!c.account_confirmed && (
                            <select
                              className="entry-input entry-input--code"
                              value={account[c.claim_no] ?? c.suggested_account_code ?? ''}
                              onChange={(e) =>
                                setAccount((a) => ({ ...a, [c.claim_no]: e.target.value }))
                              }
                            >
                              <option value="">費用科目</option>
                              {(accounts?.accounts ?? [])
                                .filter((a) => a.kind === 'expense' && a.is_postable)
                                .map((a) => (
                                  <option key={a.code} value={a.code}>
                                    {a.code} {a.name}
                                  </option>
                                ))}
                            </select>
                          )}
                          <button
                            className="btn btn--compact"
                            disabled={busy === c.claim_no}
                            title={
                              c.next_step
                                ? `本關由${c.next_step.role_name ?? '其他角色'}負責，其他角色簽核會被拒絕`
                                : '應經的關卡都已簽核，開立待過帳的費用傳票'
                            }
                            onClick={() => void decide(c, true)}
                          >
                            {busy === c.claim_no
                              ? '處理中…'
                              : c.next_step
                                ? '簽核'
                                : '產生傳票'}
                          </button>
                          {c.next_step && (
                            <button
                              className="btn btn--compact btn--secondary"
                              disabled={busy === c.claim_no}
                              onClick={() => void decide(c, false)}
                            >
                              退回
                            </button>
                          )}
                        </span>
                      ) : c.approve_blocked_reason ? (
                        <span className="cell-muted cell-reason">
                          {c.approve_blocked_reason}
                        </span>
                      ) : (
                        <span className="cell-muted">
                          {c.status === 'rejected' ? '已退回' : '已入帳'}
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
            最後一關核准後開立待過帳傳票：借費用科目、貸應付帳款或零用金。
            月底彙總匯款的單據同時成為應付帳單，匯款在「應付帳款」送出。
          </p>

        </>
      )}

      {drawer && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">建立報支單</div>
                <div className="drawer__subtitle">代沒有登入帳號的員工送單</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(false)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              <div className="field-grid">
                <label className="field field--full">
                  事由
                  <input
                    className="input"
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                  />
                </label>
                <label className="field">
                  金額（含稅）
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={amount}
                    onChange={(e) => setAmount(e.target.value)}
                  />
                </label>
                <label className="field">
                  支出日期
                  <DateField value={spentOn} onChange={(v) => setSpentOn(v)} />
                </label>
                <label className="field">
                  申請人
                  <input
                    className="input"
                    value={claimant}
                    onChange={(e) => setClaimant(e.target.value)}
                  />
                </label>
                <label className="field">
                  部門
                  <select
                    className="input"
                    value={deptCode}
                    onChange={(e) => setDeptCode(e.target.value)}
                  >
                    <option value="">未指定</option>
                    {(depts?.dimensions ?? [])
                      .filter((d) => d.code === 'department')
                      .flatMap((d) => d.values)
                      .filter((v) => v.active)
                      .map((v) => (
                        <option key={v.code} value={v.code}>
                          {v.label}
                        </option>
                      ))}
                  </select>
                </label>
                <label className="field">
                  付款方式
                  <select
                    className="input"
                    value={payMethod}
                    onChange={(e) => setPayMethod(e.target.value)}
                  >
                    <option value="reimburse">月底彙總匯款</option>
                    <option value="petty_cash">零用金即時支付</option>
                  </select>
                </label>
                <label className="field field--full">
                  備註
                  <input className="input" value={memo} onChange={(e) => setMemo(e.target.value)} />
                </label>
              </div>
              <p className="form-note">費用科目在會計那一關決定。</p>
            </div>
            <div className="drawer__foot">
              <button className="btn btn--secondary" onClick={() => setDrawer(false)}>
                取消
              </button>
              <button className="btn" disabled={saving} onClick={() => void saveClaim()}>
                {saving ? '建立中…' : '建立報支單'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
