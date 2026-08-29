import { useCallback, useEffect, useState } from 'react';
import {
  balanceSheet,
  callOp,
  cashFlow,
  incomeStatement,
  isZero,
  money,
  reportAmount,
  type BalanceSheet,
  type CashFlow,
  type IncomeStatement,
  type ReportRow,
} from '../api/client';

interface Props {
  reloadKey: number;
}

function rangeLabel(from: string, to: string): string {
  const start = from.replace(/-/g, '/');
  const end =
    from.slice(0, 4) === to.slice(0, 4) ? to.slice(5).replace('-', '/') : to.replace(/-/g, '/');

  return `${start}–${end}`;
}

function Sheet({ title, period, rows }: { title: string; period: string; rows: ReportRow[] }) {
  return (
    <section className="card doc-card report-card">
      <div className="doc-card__head">
        <h3 className="section-title doc-card__title">{title}</h3>
        <span className="doc-card__badge">{period}</span>
      </div>
      {rows.map((r, i) =>
        r.kind === 'head' ? (
          <div className="report-head" key={`${r.label}-${i}`}>
            {r.label}
          </div>
        ) : (
          <div className={`doc-row${r.kind ? ` doc-row--${r.kind}` : ''}`} key={`${r.label}-${i}`}>
            <span>
              {r.code && <span className="num report-code">{r.code}</span>}
              {r.label}
              {r.hint && <span className="cell-hint">{r.hint}</span>}
            </span>
            <span className="num">{r.value === null ? '' : reportAmount(r.value)}</span>
          </div>
        ),
      )}
    </section>
  );
}

export function Reports({ reloadKey }: Props) {
  const [bs, setBs] = useState<BalanceSheet | null>(null);
  const [pl, setPl] = useState<IncomeStatement | null>(null);
  const [cf, setCf] = useState<CashFlow | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp('get_balance_sheet', {}, undefined, balanceSheet),
      callOp('get_income_statement', {}, undefined, incomeStatement),
      callOp('get_cash_flow', {}, undefined, cashFlow),
    ])
      .then(([b, p, c]) => {
        setBs(b);
        setPl(p);
        setCf(c);
      })
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  const unposted = bs?.unposted;

  return (
    <div>
      <h1 className="page-title">財務報表</h1>
      {unposted && (unposted.count > 0 || unposted.after_period.count > 0) && (
        <p className="page-lede">
          {unposted.count > 0 && (
            <>
              本表未計入 <span className="num">{unposted.count}</span> 張未過帳傳票（
              <span className="num">{money(unposted.amount)}</span>）。
            </>
          )}
          {unposted.after_period.count > 0 && (
            <>
              {' '}另有 <span className="num">{unposted.after_period.count}</span> 張已過帳傳票（
              <span className="num">{money(unposted.after_period.amount)}</span>）日期在{' '}
              <span className="num">{unposted.after_period.through}</span> 之後，不在本表期間內。
            </>
          )}
        </p>
      )}

      {error && <p className="state-note state-note--error">{error}</p>}
      {!bs && !error && <p className="state-note">讀取中…</p>}

      {bs && pl && cf && (
        <>
          <div className="report-columns">
            <section>
              <Sheet title="資產負債表" period={bs.period_label} rows={bs.rows} />

              <p className={`recon${bs.balanced ? '' : ' recon--off'}`}>
                {bs.balanced ? (
                  <>
                    資產 <span className="num">{money(bs.assets)}</span> ＝ 負債{' '}
                    <span className="num">{money(bs.liabilities)}</span> ＋ 權益{' '}
                    <span className="num">{money(bs.equity)}</span>，兩者分開計算，結果相等。
                    權益裡含尚未結轉的本期損益{' '}
                    <span className="num">{money(bs.profit)}</span>；沒計入就不會平衡。
                  </>
                ) : (
                  <>
                    不平：資產 <span className="num">{money(bs.assets)}</span>，負債＋權益{' '}
                    <span className="num">
                      {money(bs.liabilities)} ＋ {money(bs.equity)}
                    </span>
                    ，差 <span className="num">{money(bs.difference)}</span> 元。
                    此情形表示總帳本身有誤，請先暫緩出具報表。
                  </>
                )}
              </p>
            </section>

            <section>
              <Sheet title="損益表" period={`${rangeLabel(pl.from, pl.to)} 累計`} rows={pl.rows} />
              <p className="recon">
                本期損益 <span className="num">{money(pl.profit)}</span>，
                與資產負債表滾進權益的金額相同，兩張報表採用同一個定義計算。
                {isZero(pl.revenue) && <> 本期尚無已過帳的收入。</>}
              </p>
            </section>

            <section>
              <Sheet title="現金流量表" period={cf.period_label} rows={cf.rows} />

              <p className={`recon${cf.reconciled ? '' : ' recon--off'}`}>
                {cf.reconciled ? (
                  <>
                    期末現金 <span className="num">{money(cf.closing)}</span> 有三種計算方式：
                    三段活動加總、現金科目分錄，以及各資金帳戶餘額（
                    {[
                      ...cf.cash_accounts.map((a) => `${a.name} ${money(a.balance)}`),
                      ...(Number(cf.cash_accounts_unattributed) !== 0
                        ? [`尚未指定資金帳戶 ${money(cf.cash_accounts_unattributed)}`]
                        : []),
                    ].join('、')}
                    ），三者分開計算，結果一致。
                  </>
                ) : (
                  <>
                    不相符：三段活動推出 <span className="num">{money(cf.closing)}</span>，現金科目
                    的分錄算出 <span className="num">{money(cf.ledger_closing)}</span>（差{' '}
                    <span className="num">{money(cf.difference)}</span>），各資金帳戶合計{' '}
                    <span className="num">{money(cf.cash_accounts_total)}</span>（差{' '}
                    <span className="num">{money(cf.cash_accounts_difference)}</span>）。
                  </>
                )}
              </p>

              <p className="hint">
                不涉及現金但跨活動別的分錄會在兩段各出現一次、一正一負相抵，該兩段的組成金額偏大。
              </p>
            </section>
          </div>
        </>
      )}
    </div>
  );
}
