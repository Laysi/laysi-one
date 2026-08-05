import { useEffect, useState } from 'react';
import { callOp, type Dashboard as DashboardData } from '../api/client';
import type { ScreenId } from '../nav';

/** Chart ticks are in millions; a column of seven-digit numbers would drown
 *  the chart itself. */
function millions(v: string): string {
  return (Number(v) / 1_000_000).toFixed(2);
}

interface Props {
  onGo: (screen: ScreenId) => void;
  reloadKey: number;
}

export function Dashboard({ onGo, reloadKey }: Props) {
  const [data, setData] = useState<DashboardData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    callOp<DashboardData>('get_dashboard')
      .then((d) => live && setData(d))
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
    };
  }, [reloadKey]);

  if (error) return <p className="state-note state-note--error">{error}</p>;
  if (!data) return <p className="state-note">讀取中…</p>;

  // Bar heights are scaled to the peak in the range so changes are visible.
  const values = data.cash_bars.map((b) => Number(b.value));
  const peak = Math.max(...values, 1);

  return (
    <div>
      <h1 className="page-title page-title--hero">財務總覽</h1>
      <p className="page-lede">
        所有數字均由已過帳傳票推導，未過帳傳票不計入。
      </p>

      <div className="stat-grid">
        {data.stats.map((s) => (
          <div className="card stat-card" key={s.kicker}>
            <div className="stat-card__kicker">{s.kicker}</div>
            <div className="stat-card__value num">{s.value}</div>
            <div className="stat-card__note">{s.note}</div>
          </div>
        ))}
      </div>

      <div className="dash-columns">
        <section>
          <h2 className="section-title">待處理事項</h2>
          <div className="todo-list">
            {data.todos.map((t) => (
              <button className="todo" key={t.label} onClick={() => onGo(t.screen as ScreenId)}>
                <span className="todo__count num">{t.count}</span>
                <span className="todo__label">{t.label}</span>
                <span className="todo__due">{t.due}</span>
              </button>
            ))}
          </div>

          <h2 className="section-title section-title--spaced">近半年現金水位</h2>
          <p className="section-note">單位：百萬元。</p>
          <div className="cash-chart">
            {data.cash_bars.map((b) => (
              <div className="cash-chart__col" key={b.month}>
                <div className="cash-chart__value num">{millions(b.value)}</div>
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
        </section>

        <section>
          <h2 className="section-title">AI 操作紀錄</h2>
          <p className="section-note">
            透過 MCP 連線的操作皆留存紀錄，包含被權限擋下的呼叫。
          </p>
          <div className="card">
            {data.ai_recent.map((a, i) => (
              <div className="ai-item" key={i}>
                <div className="ai-item__head">
                  <span>{a.what}</span>
                  <span className="ai-item__time num">{a.time}</span>
                </div>
                <div className="ai-item__who">
                  {a.who}
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
                檢視 MCP 工具與完整稽核軌跡 →
              </a>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
