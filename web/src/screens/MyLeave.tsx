import { useEffect, useState } from 'react';
import {
  businessDate,
  callOp,
  myLeaveList,
  myLeaveWithdrawal,
  OpFailed,
  trimDecimal,
  wallSpan,
  type MyLeaveList,
  type MyLeaveRequest,
} from '../api/client';
import type { ScreenId } from '../nav';
import { useCurrentUser } from '../auth';
import { CancelledHint } from '../components/CancelledHint';
import { RulesetNote } from '../components/RulesetNote';
import { TakeBackModal } from '../components/TakeBackModal';

interface Props {
  onGo: (screen: ScreenId) => void;
  onToast: (msg: string) => void;
  onChanged: () => void;
  reloadKey: number;
}

function statusTag(status: string): string {
  if (status === 'rejected') return 'tag tag--bad tag--static';
  if (status === 'approved') return 'tag tag--static';
  if (status === 'withdrawn' || status === 'revoked') return 'tag tag--pale tag--static';
  return 'tag tag--warn tag--static';
}

export function MyLeave({ onGo, onToast, onChanged, reloadKey }: Props) {
  const [data, setData] = useState<MyLeaveList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [asking, setAsking] = useState<MyLeaveRequest | null>(null);
  const [busy, setBusy] = useState(false);
  const canWithdraw = useCurrentUser().permissions.includes('withdraw_my_leave_request');

  async function withdraw(r: MyLeaveRequest, reason: string) {
    setBusy(true);
    try {
      await callOp(
        'withdraw_my_leave_request',
        { leave_no: r.leave_no, ...(reason ? { reason } : {}) },
        `撤回休假單 ${r.leave_no}`,
        myLeaveWithdrawal,
      );
      onToast(`${r.leave_no} 已撤回`);
      setAsking(null);
      onChanged();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    let live = true;
    setError(null);
    callOp('list_my_leave_requests', {}, undefined, myLeaveList)
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
                <th className="align-right">時數</th>
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
                  <td className="num cell-nowrap">{wallSpan(r.from_at, r.to_at)}</td>
                  <td className="num align-right">
                    {trimDecimal(r.hours)}
                    <span className="cell-hint cell-hint--block num">
                      {trimDecimal(r.days)} 日
                    </span>
                  </td>
                  <td>
                    <span className={statusTag(r.status)}>{r.status_label}</span>
                    {r.status !== 'pending' && r.decided_by && (
                      <span className="cell-hint cell-hint--block">
                        {r.decided_by}
                        {r.decided_role && `（${r.decided_role}）`}
                        {r.decision_note && ` · ${r.decision_note}`}
                      </span>
                    )}
                    {r.cancelled && <CancelledHint cancelled={r.cancelled} />}
                    {r.status === 'pending' && (
                      <span className="cell-hint cell-hint--block">待主管簽核</span>
                    )}
                    {r.status === 'pending' && canWithdraw && (
                      <button
                        className="btn btn--compact btn--secondary"
                        onClick={() => setAsking(r)}
                      >
                        撤回
                      </button>
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

        {asking && (
          <TakeBackModal
            title="撤回休假單"
            subtitle={`${asking.leave_no} · ${asking.kind_label} · ${wallSpan(asking.from_at, asking.to_at)}`}
            warning="撤回後無法復原，同一段時間要休假請重新申請。"
            reasonLabel="理由（選填）"
            reasonRequired={false}
            confirmLabel="確認撤回"
            busy={busy}
            onConfirm={(reason) => void withdraw(asking, reason)}
            onCancel={() => setAsking(null)}
          />
        )}

        <p className="hint">{al.note}</p>
        <RulesetNote
          items={[
            {
              label: '特休',
              from: al.ruleset.effective_from,
              source: al.ruleset.source_note,
            },
          ]}
        />
      </section>
    </div>
  );
}
