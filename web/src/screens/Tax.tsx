import { useCallback, useEffect, useState } from 'react';
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

/** Step one filing period back or forward, derived from the period start and
 *  end; the length in months is the backend's decision. */
function shift(v: VatReturn, direction: -1 | 1): string {
  const base = direction < 0 ? v.period_from : v.period_to;
  const [y, m, d] = base.split('-').map((s) => Number(s));
  const step = direction * v.period_months;
  const month = (m ?? 1) + step;
  const year = (y ?? 2026) + Math.floor((month - 1) / 12);
  const norm = ((month - 1) % 12 + 12) % 12 + 1;
  // Use the 15th to avoid differing month lengths; landing anywhere inside
  // the period is enough.
  return `${year}-${String(norm).padStart(2, '0')}-${String(Math.min(d ?? 15, 15)).padStart(2, '0')}`;
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

  return (
    <div>
      <h1 className="page-title">稅務</h1>
      <p className="page-lede">
        {vat
          ? `401 營業稅每 ${vat.period_months} 個月一期。本頁數字均由已過帳傳票計算，未過帳者不計入。`
          : '401 營業稅申報試算。'}
      </p>

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

            {/* An all-zero return is not a fault; there are no posted
                transactions in this period yet. Say so, and explain why. */}
            {isZero(vat.output_tax) && isZero(vat.input_tax) && (
              <p className="hint">
                本期尚無已過帳的銷項或進項，各欄皆為零。
                {vat.pending_vouchers > 0 ? (
                  <>
                    {' '}把那 <span className="num">{vat.pending_vouchers}</span> 張待過帳傳票過帳，
                    銷項 <span className="num">{money(vat.pending_sales)}</span> 與稅額{' '}
                    <span className="num">{money(vat.pending_output_tax)}</span> 即會列入；
                    申報書僅採計總帳，不採計傳票草稿。
                  </>
                ) : (
                  ' 開立發票並將傳票過帳後即會產生數字。'
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
              <span className="spacer" />
              <button className="btn btn--compact btn--off" disabled>
                產生媒體申報檔
              </button>
              <button className="btn btn--compact btn--secondary btn--off" disabled>
                送給會計師覆核
              </button>
            </div>
            <p className="form-note">
              媒體申報檔要照財政部的格式產生，會計師覆核要搭配簽核流程；
              兩項功能尚未接上，故按鈕暫時停用。
            </p>
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
              上月扣繳稅款應於次月 10 日前繳納，系統將提醒。扣繳率與起扣點依給付日適用的
              規則計算（withholding {wht.ruleset_versions.join('、') || '—'}），非由畫面設定。
            </p>
          </section>
        </div>
      )}

      {vat && (
        <>
          {/* Output tax on the return and account 2211 are two independently
              computed paths. Equality is a conclusion, not an assumption. */}
          <p className={`recon${vat.reconciled ? '' : ' recon--off'}`}>
            {vat.reconciled ? (
              <>
                申報書的銷項稅額 <span className="num">{money(vat.output_tax)}</span> 就是總帳{' '}
                <span className="num">2211 代收營業稅</span> 在{' '}
                <span className="num">{vat.period_from}</span> 到{' '}
                <span className="num">{vat.period_to}</span> 的發生額；用{' '}
                <span className="num">4111 銷貨收入</span> 乘 {vat.ruleset.rate_percent}% 也是這個數
                ，兩者分開計算，結果相符。
              </>
            ) : (
              <>
                對不上：<span className="num">4111</span> 的{' '}
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
                {' '}另有 <span className="num">{vat.pending_vouchers}</span> 張傳票尚未過帳，
                含銷項 <span className="num">{money(vat.pending_sales)}</span>、銷項稅額{' '}
                <span className="num">{money(vat.pending_output_tax)}</span>；
                總帳尚未異動，申報書也沒有計入。
              </>
            )}
          </p>
          <p className="hint">
            稅率 {vat.ruleset.rate_percent}% 與「每 {vat.period_months} 個月一期」均取自法規規則庫
            （<span className="num">vat {vat.ruleset.version}</span>，{vat.ruleset.effective_from} 起）
            ，非寫於程式碼。依據：{vat.ruleset.source_note}
          </p>
        </>
      )}
    </div>
  );
}
