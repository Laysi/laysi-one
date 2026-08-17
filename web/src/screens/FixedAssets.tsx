import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  OpFailed,
  type DepreciationPreview,
  type FixedAssetList,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

export function FixedAssets({ onToast, reloadKey, onChanged }: Props) {
  const [data, setData] = useState<FixedAssetList | null>(null);
  const [preview, setPreview] = useState<DepreciationPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<FixedAssetList>('list_fixed_assets'),
      callOp<DepreciationPreview>('preview_depreciation'),
    ])
      .then(([a, p]) => {
        setData(a);
        setPreview(p);
      })
      .catch((e: Error) => setError(e.message));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function run() {
    if (!preview) return;
    setBusy(true);
    try {
      const d = await callOp<{
        run: { voucher_no: string; total: string; ruleset_version: string | null };
      }>('run_depreciation', { period: preview.period }, `提列 ${preview.period} 折舊`);
      onToast(
        `已產生折舊傳票 ${d.run.voucher_no}，${money(d.run.total)} 待過帳；過帳後累計折舊才會增加`,
      );
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const monthLabel = preview ? `${Number(preview.period.slice(5))} 月` : '';
  const done = preview?.already_run ?? null;

  return (
    <div>
      <h1 className="page-title">固定資產與折舊</h1>
      <p className="page-lede">每月折舊由系統計算並產生傳票。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!data && !error && <p className="state-note">讀取中…</p>}

      {data && preview && (
        <>
          <div className="card pay-bar">
            <div>
              <div className="pay-bar__kicker">{monthLabel}折舊金額試算</div>
              <div className="pay-bar__total num">{money(preview.total)}</div>
            </div>
            <span className="spacer" />
            <div className="pay-bar__note">
              {done ? (
                <>
                  已產生傳票 <span className="num">{done.voucher_no}</span>（
                  {done.voucher_status === 'posted' ? '已過帳' : '待過帳'}）
                </>
              ) : (
                <>
                  {preview.count} 項待提列，年限查自耐用年數表（
                  <span className="num">{preview.ruleset_version}</span>）
                </>
              )}
            </div>
            <button
              className={`btn${done ? ' btn--off' : ''}`}
              disabled={busy || done !== null || preview.count === 0}
              onClick={() => void run()}
            >
              {busy ? '提列中…' : done ? '已完成' : `執行 ${monthLabel}折舊`}
            </button>
          </div>

          <p className={`recon${data.reconciled ? '' : ' recon--off'}`}>
            {data.reconciled ? (
              <>
                成本合計 <span className="num">{money(data.cost_total)}</span> 就是總帳{' '}
                <span className="num">1601 辦公設備</span> 的餘額，累計折舊{' '}
                <span className="num">{money(data.accumulated_total)}</span> 就是{' '}
                <span className="num">1602 累計折舊</span> 的餘額，帳面淨額{' '}
                <span className="num">{money(data.book_total)}</span> 因此與總帳一致。
              </>
            ) : (
              <>
                對不上：成本合計 <span className="num">{money(data.cost_total)}</span> 對{' '}
                <span className="num">1601</span> 的 <span className="num">{money(data.gl_cost)}</span>
                （差 <span className="num">{money(data.cost_difference)}</span>），累計折舊{' '}
                <span className="num">{money(data.accumulated_total)}</span> 對{' '}
                <span className="num">1602</span> 的{' '}
                <span className="num">{money(data.gl_accumulated)}</span>（差{' '}
                <span className="num">{money(data.accumulated_difference)}</span>）。請先暫緩本月折舊提列。
              </>
            )}
            {!isZero(data.not_yet_posted) && (
              <>
                {' '}另有 <span className="num">{money(data.not_yet_posted)}</span> 的資產，
                其取得傳票尚未過帳，故不計入成本也不提折舊。
              </>
            )}
          </p>

          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>資產</th>
                  <th>取得日</th>
                  <th>年限</th>
                  <th className="align-right">成本</th>
                  <th className="align-right">已折舊</th>
                  <th className="align-right">帳面價值</th>
                </tr>
              </thead>
              <tbody>
                {data.assets.map((a) => (
                  <tr key={a.code} className={a.recognised ? undefined : 'row--waiting'}>
                    <td>
                      {a.name}
                      {!a.recognised && (
                        <span className="tag tag--pale">取得傳票未過帳</span>
                      )}
                      {a.fully_depreciated && a.recognised && (
                        <span className="tag tag--pale">已提列完畢</span>
                      )}
                    </td>
                    <td className="num cell-accounts cell-nowrap">{a.acquired_on.replace(/-/g, '/')}</td>
                    <td className="cell-accounts">
                      {a.life_label}
                      <span className="cell-hint">{a.category_label}</span>
                    </td>
                    <td className="num align-right">{money(a.cost)}</td>
                    <td className="num align-right cell-muted">
                      {money(a.accumulated)}
                      {!isZero(a.in_transit) && (
                        <span className="cell-hint cell-hint--block">＋{money(a.in_transit)} 待過帳</span>
                      )}
                    </td>
                    <td className="num align-right">{money(a.book_value)}</td>
                  </tr>
                ))}
                {data.assets.length === 0 && (
                  <tr>
                    <td colSpan={6} className="cell-muted">
                      尚未登記固定資產。
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <h3 className="section-title section-title--spaced">本期應提列項目</h3>
          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th>資產</th>
                  <th>年限</th>
                  <th className="align-right">本期折舊</th>
                  <th>依據</th>
                </tr>
              </thead>
              <tbody>
                {preview.assets.map((a) => (
                  <tr key={a.code} className={a.skip_reason ? 'row--waiting' : undefined}>
                    <td>{a.name}</td>
                    <td className="cell-accounts">{a.life_label}</td>
                    <td className="num align-right">
                      {a.skip_reason ? <span className="cell-muted">—</span> : money(a.amount)}
                    </td>
                    <td className="cell-accounts">
                      {a.skip_reason ?? (
                        <>
                          成本 <span className="num">{money(a.cost)}</span> ÷{' '}
                          <span className="num">{a.life_years}</span> 年 ÷ 12（依耐用年數表{' '}
                          <span className="num">{a.life_ruleset_version}</span>）
                        </>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <p className="hint">
            「執行折舊」會開立一張借 <span className="num">6152 折舊費用</span>、貸{' '}
            <span className="num">1602 累計折舊</span> 的傳票，狀態為待過帳。同一期僅能提列一次；
            重複提列會使累計折舊多計一個月，該誤差通常要到資產提列完畢時才會顯現。
          </p>
          <p className="hint">
            年限依資產類別查耐用年數表，不逐筆設定。每筆提列均保存當時查得的年數與規則版本。
          </p>

          {data.runs.length > 0 && (
            <>
              <h3 className="section-title section-title--spaced">提列紀錄</h3>
              <div className="table-scroll">
                <table className="table">
                  <thead>
                    <tr>
                      <th>期別</th>
                      <th>傳票</th>
                      <th>規則版本</th>
                      <th className="align-right">金額</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.runs.map((r) => (
                      <tr key={r.period}>
                        <td>
                          {r.period}
                          {r.kind === 'opening' && (
                            <span className="tag tag--pale">期初還原</span>
                          )}
                        </td>
                        <td className="num cell-accounts cell-nowrap">
                          {r.voucher_no}
                          <span className="cell-hint">
                            {r.voucher_status === 'posted' ? '已過帳' : '待過帳'}
                          </span>
                        </td>
                        <td className="cell-accounts">
                          {r.ruleset_version ?? '—（由淨額反推，非依規則提列）'}
                        </td>
                        <td className="num align-right">{money(r.total)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </>
      )}
    </div>
  );
}
