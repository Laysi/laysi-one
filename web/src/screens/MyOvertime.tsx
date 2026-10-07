import { useEffect, useState } from 'react';
import {
  businessDate,
  callOp,
  money,
  myOvertimeList,
  myOvertimeWithdrawal,
  OpFailed,
  trimDecimal,
  wallSpan,
  type MyOvertimeList,
  type OvertimeRequestRow,
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

export function MyOvertime({ onGo, onToast, onChanged, reloadKey }: Props) {
  const [data, setData] = useState<MyOvertimeList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [asking, setAsking] = useState<OvertimeRequestRow | null>(null);
  const [busy, setBusy] = useState(false);
  const canWithdraw = useCurrentUser().permissions.includes('withdraw_my_overtime_request');

  async function withdraw(r: OvertimeRequestRow, reason: string) {
    setBusy(true);
    try {
      await callOp(
        'withdraw_my_overtime_request',
        { request_no: r.request_no, ...(reason ? { reason } : {}) },
        `撤回加班申請 ${r.request_no}`,
        myOvertimeWithdrawal,
      );
      onToast(`${r.request_no} 已撤回`);
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
    callOp('list_my_overtime_requests', {}, undefined, myOvertimeList)
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
        <h1 className="page-title">我的加班</h1>
        <p className="state-note state-note--error">{error}</p>
      </div>
    );
  }

  if (!data) return <p className="state-note">讀取中…</p>;

  const approvedPay = data.requests
    .filter((r) => r.status === 'approved' && r.worked_on.startsWith(data.month) && r.pay)
    .reduce((sum, r) => sum + Number(r.pay), 0);

  return (
    <div>
      <h1 className="page-title">我的加班</h1>

      <div className="stat-grid">
        <div className="card stat-card">
          <div className="stat-card__kicker">{data.month} 已核准時數</div>
          <div className="stat-card__value num">{trimDecimal(data.month_approved_hours)}</div>
          <div className="stat-card__note num">
            小時 · 本月上限 {trimDecimal(data.monthly_max_hours)}
          </div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">{data.month} 已核准加班費</div>
          <div className="stat-card__value num">{money(String(approvedPay))}</div>
          <div className="stat-card__note num">隨當期薪資發放</div>
        </div>
        <div className="card stat-card">
          <div className="stat-card__kicker">待簽核</div>
          <div className="stat-card__value num">{data.pending_count}</div>
          <div className="stat-card__note num">張 · 累計送出 {data.requests.length} 張</div>
        </div>
      </div>

      <div className="toolbar">
        <button className="btn btn--compact" onClick={() => onGo('meotnew')}>
          加班申請 →
        </button>
      </div>

      <section>
        <h3 className="section-title">本人歷史加班紀錄</h3>
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th>單號</th>
                <th>加班日</th>
                <th>期間</th>
                <th className="align-right">時數</th>
                <th className="align-right">加班費</th>
                <th>狀態</th>
              </tr>
            </thead>
            <tbody>
              {data.requests.map((r) => (
                <tr
                  key={r.request_no}
                  className={r.status === 'pending' ? 'row--pending' : undefined}
                >
                  <td className="num">
                    {r.request_no}
                    <span className="cell-hint cell-hint--block">
                      {businessDate(r.created_at)} 送出
                    </span>
                  </td>
                  <td>
                    {r.day_type_label}
                    {r.reason && <span className="cell-hint cell-hint--block">{r.reason}</span>}
                  </td>
                  <td className="num cell-nowrap">{wallSpan(r.from_at, r.to_at)}</td>
                  <td className="num align-right">{trimDecimal(r.hours)}</td>
                  <td className="num align-right">
                    {r.pay ? (
                      money(r.pay)
                    ) : (
                      <span className="cell-hint">{r.pay_blocked_reason}</span>
                    )}
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
                  <td colSpan={6} className="state-note">
                    尚未送出加班申請。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>

      {asking && (
        <TakeBackModal
          title="撤回加班申請"
          subtitle={`${asking.request_no} · ${asking.day_type_label} · ${wallSpan(asking.from_at, asking.to_at)}`}
          warning="撤回後無法復原，同一段時間要加班請重新申請。"
          reasonLabel="理由（選填）"
          reasonRequired={false}
          confirmLabel="確認撤回"
          busy={busy}
          onConfirm={(reason) => void withdraw(asking, reason)}
          onCancel={() => setAsking(null)}
        />
      )}

      <RulesetNote items={[{ label: '加班費', source: data.ruleset.source_note }]} />
    </div>
  );
}
