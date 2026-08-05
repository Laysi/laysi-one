import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  OpFailed,
  type BudgetStatus,
  type DepartmentPl,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

export function Budget({ onToast, reloadKey }: Props) {
  const [status, setStatus] = useState<BudgetStatus | null>(null);
  const [pl, setPl] = useState<DepartmentPl | null>(null);
  const [version, setVersion] = useState<string | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState('');

  // The edit affordance appears on every row. Whether it works is decided by
  // the backend's role check; the frontend does not recompute who the owner
  // is, which would become a second permission rule that drifts.
  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<BudgetStatus>('get_budget_status', version ? { version } : {}),
      callOp<DepartmentPl>('get_department_pl'),
    ])
      .then(([b, d]) => {
        setStatus(b);
        setPl(d);
      })
      .catch((e: Error) => setError(e.message));
  }, [version]);

  useEffect(load, [load, reloadKey]);

  async function save(code: string) {
    try {
      await callOp<unknown>('set_budget', {
        account_code: code,
        amount: draft.trim(),
        ...(status ? { version: status.version } : {}),
      });
      onToast(`已更新 ${code} 的預算；實際數不受影響，一律由總帳即時計算`);
      setEditing(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  return (
    <div>
      <h1 className="page-title">預算與管理</h1>
      <p className="page-lede">
        {status
          ? `預算版本 ${status.version_label}。執行率超過 ${status.alert_percent}% 將標示。`
          : '預算執行率與部門損益。'}
      </p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!status && !error && <p className="state-note">讀取中…</p>}

      {status && pl && (
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
          </div>

          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-card__kicker">年度預算</div>
              <div className="stat-card__value num">{money(status.budget_total)}</div>
              <div className="stat-card__note">{status.version_label}</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">實際支用</div>
              <div className="stat-card__value num">{money(status.actual_total)}</div>
              <div className="stat-card__note">由總帳即時計算</div>
            </div>
            <div className="card stat-card">
              <div className="stat-card__kicker">可用餘額</div>
              <div className="stat-card__value num">{money(status.remaining_total)}</div>
              <div className="stat-card__note">預算 − 實際</div>
            </div>
            <div className="card stat-card stat-card--total">
              <div className="stat-card__kicker">需注意科目</div>
              <div className="stat-card__value num">{status.alert_count}</div>
              <div className="stat-card__note">執行率超過 {status.alert_percent}%</div>
            </div>
          </div>

          {/* This one sentence is the whole module. */}
          <p className="recon">
            預算數存於資料表，<strong>實際數一律不儲存</strong>，
            每一格皆為{status.actual_source}。若另存一份實際數，同一科目的年度支用將出現
            兩個答案，並在補開傳票或作廢後產生分歧。
          </p>

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
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          <p className="hint">
            每一列均提供編輯，實際能否修改由後端判定：
            <span className="num">set_budget</span> 標示為「限老闆」，權限不足時會被拒絕並
            顯示後端訊息，該次呼叫同樣寫入稽核。
          </p>

          <h3 className="section-title section-title--spaced">部門損益</h3>
          <p className="section-note">
            {pl.from} 至 {pl.to}。未指定部門的支出單獨列示，不分攤。
          </p>

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
                        款項已支出，但未歸屬任何部門
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
                    本期間尚無可歸屬部門的收入或成本。
                  </td>
                </tr>
              )}
            </tbody>
          </table>

          {/* Departmental P&L must sum to the income statement. State any
              mismatch. */}
          <p className={`recon${pl.reconciled ? '' : ' recon--off'}`}>
            {pl.reconciled ? (
              <>
                各部門合計 <span className="num">{money(pl.total_profit)}</span> 即損益表的
                本期損益，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                對不上：部門合計 <span className="num">{money(pl.total_profit)}</span>，損益表{' '}
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
