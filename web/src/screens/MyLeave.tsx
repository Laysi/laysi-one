import { useEffect, useState } from 'react';
import { businessDate, callOp, OpFailed, trimDecimal, type MyLeaveList } from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

function statusTag(status: string): string {
  if (status === 'rejected') return 'tag tag--bad tag--static';
  if (status === 'approved') return 'tag tag--static';
  return 'tag tag--warn tag--static';
}

export function MyLeave({ onGo, reloadKey }: Props) {
  const [data, setData] = useState<MyLeaveList | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp<MyLeaveList>('list_my_leave_requests')
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
        <h1 className="page-title">我的休假</h1>
        <p className="state-note state-note--error">{error}</p>
        <p className="hint">
          本帳號尚未對應員工編號。請管理員完成連結後再回到本頁。
        </p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const al = data.annual_leave;

  return (
    <div>
      <h1 className="page-title">我的休假</h1>

      <div className="stat-grid">
        <div className="card stat-card">
          <div className="stat-card__kicker">{al.year} 年特休額度</div>
          <div className="stat-card__value num">{trimDecimal(al.entitlement)}</div>
          <div className="stat-card__note num">日 · 依年資即時計算</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">已休</div>
          <div className="stat-card__value num">{trimDecimal(al.taken)}</div>
          <div className="stat-card__note num">日 · 僅計已核准</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">餘額</div>
          <div className="stat-card__value num">{trimDecimal(al.remaining)}</div>
          <div className="stat-card__note num">日</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">待簽核</div>
          <div className="stat-card__value num">{data.pending_count}</div>
          <div className="stat-card__note num">張 · 累計送出 {data.requests.length} 張</div>
        </div>
      </div>

      <div className="toolbar">
        <button className="btn btn--compact" onClick={() => onGo('meleavenew')}>
          休假申請 →
        </button>
      </div>

      <section>
        <h3 className="section-title">本人歷史休假紀錄</h3>
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th>休假單</th>
                <th>假別</th>
                <th>期間</th>
                <th className="align-right">天數</th>
                <th>狀態</th>
              </tr>
            </thead>
            <tbody>
              {data.requests.map((r) => (
                <tr
                  key={r.leave_no}
                  className={r.status === 'pending' ? 'row--pending' : undefined}
                >
                  <td className="num">
                    {r.leave_no}
                    <span className="cell-hint cell-hint--block">{businessDate(r.created_at)} 送出</span>
                  </td>
                  <td>
                    {r.kind_label}
                    {r.reason && <span className="cell-hint cell-hint--block">{r.reason}</span>}
                  </td>
                  <td className="num cell-nowrap">
                    {r.from_on} 至 {r.to_on}
                  </td>
                  <td className="num align-right">{trimDecimal(r.days)}</td>
                  <td>
                    <span className={statusTag(r.status)}>{r.status_label}</span>
                    {r.status !== 'pending' && r.decided_by && (
                      <span className="cell-hint cell-hint--block">
                        {r.decided_by}
                        {r.decided_role && `（${r.decided_role}）`}
                        {r.decision_note && ` · ${r.decision_note}`}
                      </span>
                    )}
                    {r.status === 'pending' && (
                      <span className="cell-hint cell-hint--block">待主管簽核</span>
                    )}
                  </td>
                </tr>
              ))}
              {data.requests.length === 0 && (
                <tr>
                  <td colSpan={5} className="cell-muted">
                    尚無休假紀錄。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>

        <p className="hint">{al.note}</p>
        <p className="hint">
          額度依年資即時計算，依據 {al.ruleset.source_note}（
          {al.ruleset.effective_from} 起適用）。
        </p>
      </section>
    </div>
  );
}
