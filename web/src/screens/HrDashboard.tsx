import { Fragment, useEffect, useState, type ReactNode } from 'react';
import {
  callOp,
  hrDashboard,
  isZero,
  money,
  type HrDashboard as HrDashboardData,
} from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

const DESTINATION: Record<string, ScreenId> = {
  hr: 'hr',
  hr_att: 'att',
  hr_pay: 'pay',
  hr_ins: 'ins',
  gl: 'gl',
  budget: 'budget',
};

function emphasise(text: string): ReactNode {
  return text.split('**').map((part, i) =>
    i % 2 === 1 ? <strong key={i}>{part}</strong> : <Fragment key={i}>{part}</Fragment>,
  );
}

export function HrDashboard({ onGo, reloadKey }: Props) {
  const [data, setData] = useState<HrDashboardData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    callOp('get_hr_dashboard', {}, undefined, hrDashboard)
      .then((d) => live && setData(d))
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
    };
  }, [reloadKey]);

  if (error) return <p className="state-note state-note--error">{error}</p>;
  if (!data) return <p className="state-note">讀取中…</p>;

  return (
    <div>
      <h1 className="page-title page-title--hero">人資總覽</h1>

      <div className="stat-grid">
        {data.stats.map((s) => (
          <div className="card stat-card" key={s.kicker}>
            <div className="stat-card__kicker">{s.kicker}</div>
            <div className="stat-card__value num">{s.value}</div>
            <div className="stat-card__note num">{s.note}</div>
          </div>
        ))}
      </div>

      <div className="dash-columns">
        <section>
          <h2 className="section-title">待處理事項</h2>
          {data.todos.length === 0 ? (
            <p className="state-note">目前沒有待處理的人資事項。</p>
          ) : (
            <div className="todo-list">
              {data.todos.map((t) => {
                const to = DESTINATION[t.screen];
                return (
                  <button
                    className="todo"
                    key={t.label}
                    disabled={!to}
                    onClick={() => to && onGo(to)}
                  >
                    <span className="todo__count num">{t.count}</span>
                    <span className="todo__label">{t.label}</span>
                    <span className="todo__due">{t.due}</span>
                  </button>
                );
              })}
            </div>
          )}

          <h2 className="section-title section-title--spaced">薪資與總帳勾稽</h2>
          <p className={`recon${data.payroll_reconciled ? '' : ' recon--off'}`}>
            {data.payroll_reconciled ? (
              <>
                人資已過帳的應發合計 <span className="num">{money(data.payroll_posted_total)}</span>{' '}
                等於總帳 <span className="num">6111 薪資支出</span> 的餘額，兩者分開計算。
              </>
            ) : (
              <>
                不相符：人資已過帳的應發合計{' '}
                <span className="num">{money(data.payroll_posted_total)}</span> 對總帳{' '}
                <span className="num">6111 薪資支出</span> 的{' '}
                <span className="num">{money(data.payroll_gl_total)}</span>。
                請先暫緩發薪，並至「薪資作業」查明異常的期別。
              </>
            )}
            {!isZero(data.payroll_not_yet_posted) && (
              <>
                {' '}另有 <span className="num">{money(data.payroll_not_yet_posted)}</span>{' '}
                的薪資傳票尚未過帳。
              </>
            )}
          </p>
        </section>

        <section>
          <h2 className="section-title">人資與財務銜接狀態</h2>
          <div className="card fact-card">
            {data.links.map((l) => (
              <div className="fact fact--stacked" key={l.label}>
                <span className="fact__key">
                  {l.label}
                  {!l.implemented && (
                    <span className="tag tag--bad tag--static">尚未接上</span>
                  )}
                  {l.implemented && l.partial && (
                    <span className="tag tag--warn tag--static">部分接上</span>
                  )}
                  {l.implemented && !l.partial && (
                    <span className="tag tag--pale tag--static">已接上</span>
                  )}
                </span>
                <span className="fact__value">
                  {emphasise(l.detail)}
                  {DESTINATION[l.screen] && (
                    <span className="fact__note">
                      <button
                        className="link-cell"
                        onClick={() => {
                          const to = DESTINATION[l.screen];
                          if (to) onGo(to);
                        }}
                      >
                        前往該畫面 →
                      </button>
                    </span>
                  )}
                </span>
              </div>
            ))}
          </div>
        </section>
      </div>

      <p className="hint">逐人薪資明細與年度扣繳憑單限負責人檢視。</p>
    </div>
  );
}
