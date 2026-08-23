import { Fragment, useCallback, useEffect, useState, type ReactNode } from 'react';
import {
  callOp,
  isZero,
  money,
  type VatReturn,
  type WithholdingList,
} from '../api/client';

interface Props {
  reloadKey: number;
}

function shift(v: VatReturn, direction: -1 | 1): string {
  const base = direction < 0 ? v.period_from : v.period_to;
  const [y, m, d] = base.split('-').map((s) => Number(s));
  const step = direction * v.period_months;
  const month = (m ?? 1) + step;
  const year = (y ?? 2026) + Math.floor((month - 1) / 12);
  const norm = ((month - 1) % 12 + 12) % 12 + 1;

  return `${year}-${String(norm).padStart(2, '0')}-${String(Math.min(d ?? 15, 15)).padStart(2, '0')}`;
}

function pendingVat(v: VatReturn): ReactNode | null {
  const parts: [string, string][] = [];
  if (!isZero(v.pending_sales)) parts.push(['銷項', v.pending_sales]);
  if (!isZero(v.pending_output_tax)) parts.push(['銷項稅額', v.pending_output_tax]);
  if (!isZero(v.pending_input_tax)) parts.push(['進項稅額', v.pending_input_tax]);
  if (parts.length === 0) return null;

  return parts.map(([label, value], i) => (
    <Fragment key={label}>
      {i > 0 && '、'}
      {label} <span className="num">{money(value)}</span>
    </Fragment>
  ));
}

export function Tax({ reloadKey }: Props) {
  const [asOf, setAsOf] = useState<string | undefined>(undefined);
  const [vat, setVat] = useState<VatReturn | null>(null);
  const [wht, setWht] = useState<WithholdingList | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<VatReturn>('get_vat_return', asOf ? { as_of: asOf } : {}),
      callOp<WithholdingList>('list_withholding'),
    ])
      .then(([v, w]) => {
        setVat(v);
        setWht(w);
      })
      .catch((e: Error) => setError(e.message));
  }, [asOf]);

  useEffect(load, [load, reloadKey]);

  const pending = vat ? pendingVat(vat) : null;

  return (
    <div>
      <h1 className="page-title">稅務</h1>
      {vat && (
        <p className="page-lede">
          401 營業稅每 {vat.period_months} 個月一期。未過帳傳票不計入。
        </p>
      )}

      {error && <p className="state-note state-note--error">{error}</p>}
      {!vat && !error && <p className="state-note">讀取中…</p>}

      {vat && wht && (
        <div className="split-columns split-columns--aside">
          <section className="card doc-card">
            <div className="doc-card__head">
              <h3 className="section-title doc-card__title">401 申報書　{vat.period_label}</h3>
              <span className="doc-card__badge">試算</span>
            </div>

            {vat.rows.map((r) => (
              <div
                key={r.label}
                className={`doc-row${r.kind ? ` doc-row--${r.kind}` : ''}`}
              >
                <span>{r.label}</span>
                <span className="num">{money(r.value)}</span>
              </div>
            ))}

            {!isZero(vat.carry_forward) && (
              <p className="hint">
                進項大於銷項，本期無應納稅額；留抵 <span className="num">{money(vat.carry_forward)}</span>{' '}
                轉次期扣抵。
              </p>
            )}

            {isZero(vat.output_tax) && isZero(vat.input_tax) && (
              <p className="hint">
                本期尚無已過帳的銷項或進項，各欄皆為零。
                {pending ? (
                  <>
                    {' '}將該 <span className="num">{vat.pending_vouchers}</span> 張待過帳傳票過帳，
                    {pending} 就會列入。
                  </>
                ) : (
                  ' 開立發票並把傳票過帳後就會有數字。'
                )}
              </p>
            )}

            <div className="toolbar doc-card__actions">
              <button className="chip" onClick={() => setAsOf(shift(vat, -1))}>
                ← 上一期
              </button>
              <button className="chip" onClick={() => setAsOf(shift(vat, 1))}>
                下一期 →
              </button>
            </div>
          </section>

          <section>
            <h3 className="section-title">各類所得扣繳</h3>
            <div className="wht-list">
              {wht.rows.map((w, i) => (
                <div className="wht-row" key={`${w.payee}-${w.income_code}-${i}`}>
                  <div className="wht-row__who">
                    <div>{w.payee}</div>
                    <div className="wht-row__code">{w.code_label}</div>
                  </div>
                  <div className="wht-row__amounts">
                    <div className="num">{money(w.gross_amount)}</div>
                    <div className="wht-row__code">
                      扣繳 <span className="num">{money(w.withheld_amount)}</span>
                      {w.below_threshold && (
                        <>
                          {' '}· 未達起扣點 <span className="num">{money(w.threshold)}</span>
                        </>
                      )}
                      {w.basis === 'table' && <> · {w.basis_label}</>}
                      {!isZero(w.supplementary_amount) && (
                        <>
                          {' '}· 補充保費 <span className="num">{money(w.supplementary_amount)}</span>
                        </>
                      )}
                    </div>
                  </div>
                </div>
              ))}
              {wht.rows.length === 0 && (
                <p className="wht-row cell-muted">本月尚無應扣繳之給付。</p>
              )}
            </div>

            <div className="wht-total">
              <span className="cell-muted">本月扣繳稅款合計</span>
              <span className="num">{money(wht.tax_total)}</span>
            </div>
            {!isZero(wht.supplementary_total) && (
              <div className="wht-total">
                <span className="cell-muted">二代健保補充保費</span>
                <span className="num">{money(wht.supplementary_total)}</span>
              </div>
            )}

            <p className="hint">
              上月扣繳稅款應於次月 10 日前繳納。扣繳率與起扣點依給付日適用的
              扣繳規則計算（{wht.ruleset_versions.join('、') || '—'}）。
            </p>
          </section>
        </div>
      )}

      {vat && (
        <>

          <p className={`recon${vat.reconciled ? '' : ' recon--off'}`}>
            {vat.reconciled ? (
              <>
                申報書的銷項稅額 <span className="num">{money(vat.output_tax)}</span> 等於總帳{' '}
                <span className="num">2211 代收營業稅</span> 在{' '}
                <span className="num">{vat.period_from}</span> 到{' '}
                <span className="num">{vat.period_to}</span> 的發生額；用{' '}
                <span className="num">4111 銷貨收入</span> 乘 {vat.ruleset.rate_percent}% 也是這個數
                ，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                不相符：<span className="num">4111</span> 的{' '}
                <span className="num">{money(vat.sales)}</span> 乘 {vat.ruleset.rate_percent}% 應該是{' '}
                <span className="num">{money(vat.expected_output_tax)}</span>，但{' '}
                <span className="num">2211</span> 的當期發生額是{' '}
                <span className="num">{money(vat.output_tax)}</span>，差{' '}
                <span className="num">{money(vat.output_tax_gap)}</span> 元。常見原因為某張開票
                傳票未拆出代收營業稅，請先暫緩申報。
              </>
            )}
            {vat.pending_vouchers > 0 && (
              <>
                {' '}另有 <span className="num">{vat.pending_vouchers}</span> 張傳票尚未過帳
                {pending ? (
                  <>，含 {pending}；過帳後才會列入申報書。</>
                ) : (
                  <>，不含銷項與進項。</>
                )}
              </>
            )}
          </p>
          <p className="hint">
            稅率 {vat.ruleset.rate_percent}% 與「每 {vat.period_months} 個月一期」均取自營業稅規則
            （<span className="num">{vat.ruleset.version}</span>，{vat.ruleset.effective_from} 起）
            。依據：{vat.ruleset.source_note}
          </p>
        </>
      )}
    </div>
  );
}
