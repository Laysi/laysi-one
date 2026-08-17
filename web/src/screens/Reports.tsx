import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
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

type Tab = 'bs' | 'pl' | 'cf';

const TABS: { id: Tab; label: string }[] = [
  { id: 'bs', label: '資產負債表' },
  { id: 'pl', label: '損益表' },
  { id: 'cf', label: '現金流量表' },
];

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
  const [tab, setTab] = useState<Tab>('bs');
  const [bs, setBs] = useState<BalanceSheet | null>(null);
  const [pl, setPl] = useState<IncomeStatement | null>(null);
  const [cf, setCf] = useState<CashFlow | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<BalanceSheet>('get_balance_sheet'),
      callOp<IncomeStatement>('get_income_statement'),
      callOp<CashFlow>('get_cash_flow'),
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
      <p className="page-lede">
        {unposted && unposted.count > 0 ? (
          <>
            尚有 {unposted.count} 張傳票（{money(unposted.amount)}）未過帳，未計入本表。
            報表僅採計總帳，如需完整數字請先過帳。
          </>
        ) : (
          '三張報表的每一格均由已過帳分錄彙總，系統未另存任何報表數字。'
        )}
      </p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!bs && !error && <p className="state-note">讀取中…</p>}

      {bs && pl && cf && (
        <>
          <div className="toolbar">
            {TABS.map((t) => (
              <button
                key={t.id}
                className={`chip${tab === t.id ? ' chip--on' : ''}`}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>

          {tab === 'bs' && (
            <>
              <Sheet title="資產負債表" period={bs.period_label} rows={bs.rows} />

              <p className={`recon${bs.balanced ? '' : ' recon--off'}`}>
                {bs.balanced ? (
                  <>
                    資產 <span className="num">{money(bs.assets)}</span> ＝ 負債{' '}
                    <span className="num">{money(bs.liabilities)}</span> ＋ 權益{' '}
                    <span className="num">{money(bs.equity)}</span>，兩者分開計算，結果相等。
                    權益裡含尚未結轉的本期損益{' '}
                    <span className="num">{money(bs.profit)}</span>；未計入即無法平衡。
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
            </>
          )}

          {tab === 'pl' && (
            <>
              <Sheet title="損益表" period={pl.period_label} rows={pl.rows} />
              <p className="recon">
                本期損益 <span className="num">{money(pl.profit)}</span> 與資產負債表滾進權益的
                為同一數字，兩張報表採用相同定義計算。
                {isZero(pl.revenue) && (
                  <>
                    {' '}本期無已過帳收入：開票傳票尚未過帳，銷貨收入尚未進入總帳。
                  </>
                )}
              </p>
            </>
          )}

          {tab === 'cf' && (
            <>
              <Sheet title="現金流量表" period={cf.period_label} rows={cf.rows} />

              <p className={`recon${cf.reconciled ? '' : ' recon--off'}`}>
                {cf.reconciled ? (
                  <>
                    期末現金 <span className="num">{money(cf.closing)}</span> 有三種計算方式：
                    三段活動加總、現金科目分錄，以及各銀行帳戶餘額（
                    {cf.cash_accounts.map((a) => `${a.name} ${money(a.balance)}`).join('、')}
                    ），三者分開計算，結果一致。
                  </>
                ) : (
                  <>
                    對不上：三段活動推出 <span className="num">{money(cf.closing)}</span>，現金科目
                    的分錄算出 <span className="num">{money(cf.ledger_closing)}</span>（差{' '}
                    <span className="num">{money(cf.difference)}</span>），銀行帳戶合計{' '}
                    <span className="num">{money(cf.cash_accounts_total)}</span>（差{' '}
                    <span className="num">{money(cf.cash_accounts_difference)}</span>）。
                  </>
                )}
              </p>

              <p className="hint">
                採間接法：自本期損益出發，逐項加回非現金科目的變動。每個科目在科目表上
                只屬於一種活動，故三段加總等於現金的實際變動。
              </p>
              <p className="hint">
                限制：不涉及現金但跨活動別的分錄會同時出現在兩段、一正一負相抵，
                合計仍正確，但該兩段的組成金額會被放大。
              </p>
            </>
          )}

          <div className="toolbar doc-card__actions">
            <button className="btn btn--compact btn--off" disabled>
              匯出 Excel
            </button>
            <button className="btn btn--compact btn--secondary btn--off" disabled>
              分享給會計師
            </button>
            <span className="spacer" />
            <span className="section-note">匯出與分享功能尚未提供。</span>
          </div>
        </>
      )}
    </div>
  );
}
