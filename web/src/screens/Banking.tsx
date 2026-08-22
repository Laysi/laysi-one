import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  signedMoney,
  type BankTxn,
  type CashAccount,
  type MatchCandidate,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

interface GlAccount {
  code: string;
  name: string;
  kind: string;
  is_postable: boolean;
  is_active: boolean;
}

export function Banking({ onToast, reloadKey, onChanged }: Props) {
  const [accounts, setAccounts] = useState<CashAccount[] | null>(null);
  const [total, setTotal] = useState<string>('0');
  const [unattributed, setUnattributed] = useState<string>('0');
  const [rows, setRows] = useState<BankTxn[] | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [candidates, setCandidates] = useState<MatchCandidate[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const [drawer, setDrawer] = useState(false);
  const [glAccounts, setGlAccounts] = useState<GlAccount[] | null>(null);
  const [bankName, setBankName] = useState('');
  const [bankNo, setBankNo] = useState('');
  const [glCode, setGlCode] = useState('');
  const [saving, setSaving] = useState(false);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<{ accounts: CashAccount[]; total: string; unattributed: string }>(
        'get_cash_position',
      ),
      callOp<{ transactions: BankTxn[] }>('list_bank_transactions'),
    ])
      .then(([c, t]) => {
        setAccounts(c.accounts);
        setTotal(c.total);
        setUnattributed(c.unattributed);
        setRows(t.transactions);
      })
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!drawer || glAccounts) return;
    callOp<{ accounts: GlAccount[] }>('list_accounts')
      .then((d) => {
        const usable = d.accounts.filter((a) => a.kind === 'asset' && a.is_postable && a.is_active);
        setGlAccounts(usable);
        setGlCode((code) => code || usable[0]?.code || '');
      })
      .catch(() => setGlAccounts([]));
  }, [drawer, glAccounts]);

  async function saveBankAccount() {
    if (!bankName.trim() || !bankNo.trim() || !glCode) {
      onToast('帳戶名稱、帳號與總帳科目都要填寫');
      return;
    }
    setSaving(true);
    try {
      const d = await callOp<{ bank_account: { id: number; name: string; gl_code: string } }>(
        'create_bank_account',
        { name: bankName.trim(), account_no: bankNo.trim(), gl_code: glCode },
        `新增銀行帳戶 ${bankName.trim()}`,
      );
      onToast(`已建立銀行帳戶「${d.bank_account.name}」，綁定總帳科目 ${d.bank_account.gl_code}`);
      setBankName('');
      setBankNo('');
      setDrawer(false);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  useEffect(() => {
    if (selected === null) {
      setCandidates(null);
      return;
    }
    let live = true;
    setCandidates(null);
    callOp<{ candidates: MatchCandidate[] }>('suggest_bank_matches', { txn_id: selected, limit: 4 })
      .then((d) => live && setCandidates(d.candidates))
      .catch(() => live && setCandidates([]));
    return () => {
      live = false;
    };
  }, [selected, reloadKey]);

  async function match(c: MatchCandidate) {
    if (selected === null) return;
    setBusy(c.voucher_no);
    try {
      const d = await callOp<{ transaction: BankTxn }>(
        'match_bank_transaction',
        { txn_id: selected, voucher_no: c.voucher_no },
        `核銷至 ${c.voucher_no}`,
      );
      onToast(`已核銷：${d.transaction.memo} ↔ ${c.voucher_no}`);
      setSelected(null);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  const unmatched = rows?.filter((t) => !t.matched_voucher_no).length ?? 0;
  const current = rows?.find((t) => t.id === selected);

  return (
    <div>
      <h1 className="page-title">銀行與現金</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!accounts && !error && <p className="state-note">讀取中…</p>}

      {accounts && (
        <div className="toolbar">
          <span className="spacer" />
          <button className="btn btn--compact" onClick={() => setDrawer(true)}>
            ＋ 新增銀行帳戶
          </button>
        </div>
      )}

      {accounts && accounts.length === 0 && (
        <div className="placeholder">
          <div className="placeholder__title">尚未建立銀行帳戶</div>
          <p>
            收款、付款與薪資都要指定資金進出帳戶。請由上方「＋ 新增銀行帳戶」建立第一個。
          </p>
        </div>
      )}

      {accounts && accounts.length > 0 && (
        <div className="stat-grid stat-grid--four">
          {accounts.map((a) => (
            <div className="card stat-card" key={a.name}>
              <div className="stat-card__kicker">{a.name}</div>
              <div className="stat-card__value num">{money(a.balance)}</div>
              <div className="stat-card__note num">{a.account_no}</div>
            </div>
          ))}
          {Number(unattributed) !== 0 && (
            <div className="card stat-card">
              <div className="stat-card__kicker">尚未指定資金帳戶</div>
              <div className="stat-card__value num">{money(unattributed)}</div>
              <div className="stat-card__note num">零用金，或分錄沒有指定帳戶</div>
            </div>
          )}
          <div className="card stat-card stat-card--total">
            <div className="stat-card__kicker">合計</div>
            <div className="stat-card__value num">{money(total)}</div>
            <div className="stat-card__note num">等於總帳的現金餘額</div>
          </div>
        </div>
      )}

      {rows && rows.length === 0 && accounts && accounts.length > 0 && (
        <div className="placeholder">
          <div className="placeholder__title">尚無銀行明細</div>
          <p>銀行明細由銀行對帳檔匯入後顯示於本頁，供逐筆與傳票核銷。</p>
        </div>
      )}

      {rows && rows.length > 0 && accounts && accounts.length > 0 && (
        <div className="recon-columns">
          <section>
            <h2 className="section-title">銀行流水（未核銷 {unmatched}）</h2>
            <p className="section-note">灰色為已核銷項目。</p>
            <div className="bank-list">
              {rows.map((t) => {
                const done = Boolean(t.matched_voucher_no);
                return (
                  <button
                    key={t.id}
                    className={`bank-row${selected === t.id ? ' bank-row--on' : ''}${
                      done ? ' bank-row--done' : ''
                    }`}
                    disabled={done}
                    onClick={() => setSelected(t.id)}
                  >
                    <span className="bank-row__head">
                      <span>{t.memo}</span>
                      <span className="num">{signedMoney(t.amount)}</span>
                    </span>
                    <span className="bank-row__meta num">
                      {t.txn_date.slice(5).replace('-', '/')}　{t.bank_name}　{t.kind}
                      {done && `　· 已核銷 ${t.matched_voucher_no}`}
                    </span>
                  </button>
                );
              })}
            </div>
          </section>

          <section>
            <h2 className="section-title">系統帳候選傳票</h2>
            <p className="section-note">
              以金額為主、日期為輔比對。入帳日期常有一至兩日落差，金額則應一致。
            </p>

            {selected === null && (
              <div className="placeholder">請先於左側選取一筆銀行明細</div>
            )}
            {selected !== null && !candidates && <p className="state-note">找候選中…</p>}
            {selected !== null && candidates && candidates.length === 0 && (
              <div className="placeholder">
                <div className="placeholder__title">帳上查無可對應的傳票</div>
                <p>請先於「傳票 · 總帳」建立對應傳票，再回到本頁核銷。</p>
              </div>
            )}
            {current && candidates && candidates.length > 0 && (
              <div className="candidate-list">
                {candidates.map((c) => (
                  <div className={`candidate${c.exact ? ' candidate--exact' : ''}`} key={c.voucher_no}>
                    <div className="candidate__body">
                      <div>{c.memo}</div>
                      <div className="candidate__meta num">
                        {`${c.voucher_no}　${c.date.slice(5).replace('-', '/')}　${signedMoney(
                          c.net_effect,
                        )}`}
                      </div>
                    </div>
                    <div className="candidate__score">{c.reason}</div>
                    <button
                      className="btn btn--compact"
                      disabled={busy === c.voucher_no}
                      onClick={() => void match(c)}
                    >
                      {busy === c.voucher_no ? '核銷中…' : '核銷'}
                    </button>
                  </div>
                ))}
              </div>
            )}
          </section>
        </div>
      )}

      {drawer && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">新增銀行帳戶</div>
                <div className="drawer__subtitle">要綁定一個啟用中的資產科目，供收付款與核銷入帳</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(false)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              {!glAccounts && <p className="state-note">讀取中…</p>}
              {glAccounts && glAccounts.length === 0 && (
                <p className="form-warn">
                  目前沒有啟用中的資產科目可以綁定，請先到「基礎設定」新增一個現金或銀行存款科目。
                </p>
              )}
              {glAccounts && glAccounts.length > 0 && (
                <div className="field-grid">
                  <label className="field">
                    帳戶名稱
                    <input
                      className="input"
                      value={bankName}
                      placeholder="示範銀行 城中分行"
                      onChange={(e) => setBankName(e.target.value)}
                    />
                  </label>
                  <label className="field">
                    銀行帳號
                    <input
                      className="input num"
                      value={bankNo}
                      placeholder="123-456-789012"
                      onChange={(e) => setBankNo(e.target.value)}
                    />
                  </label>
                  <label className="field field--full">
                    綁定的總帳科目
                    <select className="input" value={glCode} onChange={(e) => setGlCode(e.target.value)}>
                      {glAccounts.map((a) => (
                        <option key={a.code} value={a.code}>
                          {a.code} {a.name}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>
              )}
            </div>
            <div className="drawer__foot">
              <span className="drawer__foot-note">建立資金帳戶</span>
              <button
                className={`btn${saving ? ' btn--off' : ''}`}
                disabled={saving || !glAccounts || glAccounts.length === 0}
                onClick={() => void saveBankAccount()}
              >
                {saving ? '存檔中…' : '存檔'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
