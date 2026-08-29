import { useCallback, useEffect, useState } from 'react';
import {
  accountList,
  budgetStatus,
  callOp,
  departmentPl,
  isZero,
  money,
  newBudgetVersion,
  OpFailed,
  type Account,
  type BudgetStatus,
  type DepartmentPl,
} from '../api/client';
import { useCurrentUser } from '../auth';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

export function Budget({ onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const canCreateVersion = me.permissions.includes('create_budget_version');
  const [creating, setCreating] = useState(false);
  const [status, setStatus] = useState<BudgetStatus | null>(null);
  const [pl, setPl] = useState<DepartmentPl | null>(null);
  const [version, setVersion] = useState<string | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [accounts, setAccounts] = useState<Account[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [newCode, setNewCode] = useState('');
  const [newAmount, setNewAmount] = useState('');
  const canSetBudget = me.permissions.includes('set_budget');

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp('get_budget_status', version ? { version } : {}, undefined, budgetStatus),
      callOp('get_department_pl', {}, undefined, departmentPl),
    ])
      .then(([b, d]) => {
        setStatus(b);
        setPl(d);
      })
      .catch((e: Error) => setError(e.message));
  }, [version]);

  useEffect(load, [load, reloadKey]);

  useEffect(() => {
    if (!adding || accounts) return;
    callOp('list_accounts', {}, undefined, accountList)
      .then((d) =>
        setAccounts(
          d.accounts.filter(
            (a) => a.is_postable && a.is_active && (a.kind === 'expense' || a.kind === 'revenue'),
          ),
        ),
      )
      .catch((e: Error) => onToast(e.message));
  }, [adding, accounts, onToast]);

  function openAdd() {
    setNewCode('');
    setNewAmount('');
    setAdding(true);
  }

  async function addLine() {
    if (!newCode) {
      onToast('請先選一個科目');
      return;
    }
    await save(newCode, newAmount);
    setAdding(false);
  }

  async function createFirstVersion() {
    setCreating(true);
    try {
      const d = await callOp('create_budget_version', { code: 'V1' }, '建立第一版年度預算', newBudgetVersion);
      onToast(`已建立 ${d.version.label}，接著逐科目填入金額`);
      setVersion(d.version.code);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setCreating(false);
    }
  }

  async function save(code: string, amount = draft) {
    try {
      await callOp<unknown>('set_budget', {
        account_code: code,
        amount: amount.trim() || '0',
        ...(status ? { version: status.version } : {}),
      });
      onToast(`已更新 ${code} 的預算`);
      setEditing(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  return (
    <div>
      <h1 className="page-title">預算與管理</h1>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!status && !error && <p className="state-note">讀取中…</p>}

      {status && !status.version && (
        <div className="placeholder">
          <div className="placeholder__title">尚未建立預算</div>
          <p>
            {canCreateVersion
              ? '建立一版之後，逐個科目填入年度金額。'
              : '建立預算版本限負責人，請洽負責人。'}
          </p>
          {canCreateVersion && (
            <button
              className={`btn${creating ? ' btn--off' : ''}`}
              disabled={creating}
              onClick={() => void createFirstVersion()}
            >
              {creating ? '建立中…' : '建立第一版預算'}
            </button>
          )}
        </div>
      )}

      {status?.version && pl && (
        <>
          <div className="toolbar">
            {status.versions.map((v) => (
              <button
                key={v.code}
                className={`chip${v.code === status.version ? ' chip--on' : ''}`}
                onClick={() => setVersion(v.code)}
              >
                {v.label}
                {v.is_current && ' · 現行'}
              </button>
            ))}
            <span className="spacer" />
            <span className="section-note">
              {status.from} 至 {status.to} 累計
            </span>
            {canSetBudget && (
              <button className="btn btn--compact" onClick={openAdd}>
                ＋ 編列科目預算
              </button>
            )}
          </div>

          {adding && (
            <div className="card form-card">
              <h3 className="section-title">編列科目預算</h3>
              <p className="section-note">
                {status.version_label}。已編列的科目再選一次會覆蓋原本的金額。
              </p>
              <div className="field-row">
                <label className="field field--grow">
                  科目
                  <select
                    className="input"
                    value={newCode}
                    onChange={(e) => setNewCode(e.target.value)}
                  >
                    <option value="">請選擇</option>
                    {(accounts ?? []).map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field field--date">
                  年度預算金額
                  <input
                    className="input num"
                    inputMode="decimal"
                    value={newAmount}
                    placeholder="1200000"
                    onChange={(e) => setNewAmount(e.target.value)}
                  />
                </label>
              </div>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={() => setAdding(false)}>
                  取消
                </button>
                <button className="btn" onClick={() => void addLine()}>
                  儲存
                </button>
              </div>
            </div>
          )}

          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">年度預算</div>
              <div className="stat-card__value num">{money(status.budget_total)}</div>
              <div className="stat-card__note num">{status.version_label}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">實際支用</div>
              <div className="stat-card__value num">{money(status.actual_total)}</div>
              <div className="stat-card__note num">由總帳即時計算</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">可用餘額</div>
              <div className="stat-card__value num">{money(status.remaining_total)}</div>
              <div className="stat-card__note num">預算 − 實際</div>
            </div>
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">需注意科目</div>
              <div className="stat-card__value num">
                {status.alert_percent ? status.alert_count : '—'}
              </div>
              <div className="stat-card__note num">
                {status.alert_percent
                  ? `執行率超過 ${status.alert_percent}%`
                  : '尚未設定提醒門檻'}
              </div>
            </div>
          </div>

          <p className="recon">
            <strong>實際數為{status.actual_source}</strong>。
          </p>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>科目</th>
                  <th className="align-right">預算</th>
                  <th className="align-right">實際</th>
                  <th className="align-right">執行率</th>
                  <th className="align-right">可用餘額</th>
                </tr>
              </thead>
              <tbody>
                {status.rows.map((r) => (
                  <tr key={r.account_code} className={r.alert ? 'row--pending' : undefined}>
                    <td>
                      <span className="num report-code">{r.account_code}</span>
                      {r.account_name}
                      {r.note && <span className="cell-hint">{r.note}</span>}
                    </td>
                    <td className="num align-right">
                      {editing === r.account_code ? (
                        <span className="fact__edit">
                          <input
                            className="entry-input entry-input--amount"
                            value={draft}
                            autoFocus
                            onChange={(e) => setDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter') void save(r.account_code);
                              if (e.key === 'Escape') setEditing(null);
                            }}
                          />
                          <button className="btn btn--compact" onClick={() => void save(r.account_code)}>
                            儲存
                          </button>
                        </span>
                      ) : (
                        <>
                          {money(r.budget)}
                          {canSetBudget && (
                          <button
                            className="btn--icon fact__pencil"
                            title="編輯預算"
                            onClick={() => {
                              setEditing(r.account_code);
                              setDraft(r.budget);
                            }}
                          >
                            ✎
                          </button>
                          )}
                        </>
                      )}
                    </td>
                    <td className="num align-right">
                      {money(r.actual)}
                      <span className="cell-hint cell-hint--block">{r.line_count} 筆分錄</span>
                    </td>
                    <td className="num align-right">
                      <span className={r.alert ? 'due due--over' : 'due'}>{r.rate_label}</span>
                    </td>
                    <td className="num align-right">
                      {r.over_budget ? (
                        <span className="tag tag--bad tag--static">超支 {money(r.remaining)}</span>
                      ) : (
                        money(r.remaining)
                      )}
                    </td>
                  </tr>
                ))}
                {status.rows.length === 0 && (
                  <tr>
                    <td colSpan={5} className="cell-muted">
                      本版本尚未編列任何科目預算。
                      {canSetBudget && (
                        <button className="link-cell" onClick={openAdd}>
                          編列第一個科目
                        </button>
                      )}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className="hint">異動預算限負責人。</p>
        </>
      )}

      {pl && (
        <>
          <h3 className="section-title section-title--spaced">部門損益</h3>
          <p className="section-note">
            {pl.from} 至 {pl.to}。未指定部門的支出單獨列示，不分攤。
          </p>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>部門</th>
                  <th className="align-right">收入</th>
                  <th className="align-right">成本費用</th>
                  <th className="align-right">損益</th>
                  <th className="align-right">利潤率</th>
                </tr>
              </thead>
              <tbody>
                {pl.rows.map((r) => (
                  <tr key={r.code ?? 'none'} className={r.unallocated ? 'row--waiting' : undefined}>
                    <td>
                      {r.label}
                      {r.unallocated && (
                        <span className="cell-hint cell-hint--block">
                          分錄沒有指定部門
                        </span>
                      )}
                    </td>
                    <td className="num align-right">{money(r.revenue)}</td>
                    <td className="num align-right">{money(r.cost)}</td>
                    <td className="num align-right">{money(r.profit)}</td>
                    <td className="num align-right cell-muted">{r.margin_label}</td>
                  </tr>
                ))}
                {pl.rows.length === 0 && (
                  <tr>
                    <td colSpan={5} className="cell-muted">
                      這段期間沒有可歸屬部門的收入或成本。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <p className={`recon${pl.reconciled ? '' : ' recon--off'}`}>
            {pl.reconciled ? (
              <>
                各部門合計 <span className="num">{money(pl.total_profit)}</span> 等於損益表的
                本期損益，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                不相符：部門合計 <span className="num">{money(pl.total_profit)}</span>，損益表{' '}
                <span className="num">{money(pl.statement_profit)}</span>，差{' '}
                <span className="num">{money(pl.difference)}</span> 元。
              </>
            )}
            {pl.unallocated_cost && !isZero(pl.unallocated_cost) && (
              <>
                {' '}其中 <span className="num">{money(pl.unallocated_cost)}</span> 未指定部門：
                {pl.unallocated_note}
              </>
            )}
          </p>
        </>
      )}
    </div>
  );
}
