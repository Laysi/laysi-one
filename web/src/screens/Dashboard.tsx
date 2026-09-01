import { useEffect, useState } from 'react';
import { callOp, dashboard, type Dashboard as DashboardData } from '../api/client';
import { useCurrentUser } from '../auth';
import { screenOp, type ScreenId } from '../nav';

function scaleOf(peak: number): { div: number; unit: string; digits: number } {
  if (peak >= 100_000_000) {
    const top = peak / 100_000_000;
    return { div: 100_000_000, unit: '億元', digits: top >= 100 ? 0 : top >= 10 ? 1 : 2 };
  }
  if (peak >= 10_000) {
    const top = peak / 10_000;
    return { div: 10_000, unit: '萬元', digits: top >= 100 ? 0 : top >= 10 ? 1 : 2 };
  }
  return { div: 1, unit: '元', digits: 0 };
}

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

export function Dashboard({ onGo, reloadKey }: Props) {
  const { permissions } = useCurrentUser();
  const [data, setData] = useState<DashboardData | null>(null);
  const [error, setError] = useState<string | null>(null);

  const destination = (key: string): ScreenId | null => {
    const id = key as ScreenId;
    const op = screenOp(id);
    return op && permissions.includes(op) ? id : null;
  };

  useEffect(() => {
    let live = true;
    callOp('get_dashboard', {}, undefined, dashboard)
      .then((d) => live && setData(d))
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
    };
  }, [reloadKey]);

  if (error) return <p className="state-note state-note--error">{error}</p>;
  if (!data) return <p className="state-note">讀取中…</p>;

  const values = data.cash_bars.map((b) => Number(b.value));
  const peak = Math.max(...values, 1);
  const scale = scaleOf(peak);

  return (
    <div>
      <h1 className="page-title page-title--hero">財務總覽</h1>

      {data.ledger_started ? (
        <div className="stat-grid">
          {data.stats.map((s) => (
            <div className="card stat-card" key={s.kicker}>
              <div className="stat-card__kicker">{s.kicker}</div>
              <div className="stat-card__value num">{s.value}</div>
              <div className="stat-card__note num">{s.note}</div>
            </div>
          ))}
        </div>
      ) : (
        <div className="placeholder">
          <div className="placeholder__title">帳上沒有已過帳的傳票</div>
          <p>請先在「傳票 · 總帳」記一筆並過帳。</p>
          <div className="placeholder__actions">
            <button className="btn" onClick={() => onGo('gl')}>
              前往傳票 · 總帳
            </button>
            <button className="btn" onClick={() => onGo('ar')}>
              前往應收帳款
            </button>
          </div>
        </div>
      )}

      <div className="dash-columns">
        <section>
          <h2 className="section-title">待處理事項</h2>
          {data.todos.length === 0 ? (
            <p className="section-note">沒有待處理事項。</p>
          ) : (
            <div className="todo-list">
              {data.todos.map((t) => {
                const to = destination(t.screen);
                return (
                  <button className="todo" key={t.label} disabled={!to} onClick={() => to && onGo(to)}>
                    <span className="todo__count num">{t.count}</span>
                    <span className="todo__label">{t.label}</span>
                    <span className="todo__due">{t.due}</span>
                  </button>
                );
              })}
            </div>
          )}

          {data.ledger_started && (
            <>
              <h2 className="section-title section-title--spaced">近半年現金水位</h2>
              <p className="section-note">單位：{scale.unit}。</p>
              <div className="cash-chart">
                {data.cash_bars.map((b) => (
                  <div className="cash-chart__col" key={b.month}>
                    <div className="cash-chart__value num">
                      {(Number(b.value) / scale.div).toFixed(scale.digits)}
                    </div>
                    <div
                      className="cash-chart__bar"
                      style={{ height: `${Math.max((Number(b.value) / peak) * 100, 2)}%` }}
                    />
                  </div>
                ))}
              </div>
              <div className="cash-chart__labels">
                {data.cash_bars.map((b) => (
                  <div className="cash-chart__month" key={b.month}>
                    {b.month.slice(5)} 月
                  </div>
                ))}
              </div>
            </>
          )}
        </section>

        <section>
          <h2 className="section-title">最近的操作紀錄</h2>
          <div className="card">
            {data.recent.map((a, i) => (
              <div className="ai-item" key={i}>
                <div className="ai-item__head">
                  <span>{a.what}</span>
                  <span className="ai-item__time num">{a.time}</span>
                </div>
                <div className="ai-item__who">
                  {a.channel_label} · {a.who}
                  {a.outcome === 'blocked' && ' · 已被權限擋下'}
                  {a.outcome === 'error' && ' · 未完成'}
                </div>
              </div>
            ))}
            <div className="ai-foot">
              <a
                href="#mcp"
                onClick={(e) => {
                  e.preventDefault();
                  onGo('mcp');
                }}
              >
                檢視 MCP 工具與完整稽核紀錄 →
              </a>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
