import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  isZero,
  money,
  OpFailed,
  type DepreciationPreview,
  type DimensionList,
  type FixedAssetList,
} from '../api/client';
import {
  EMPTY_TAX_TRIPLE,
  TaxAmountFields,
  type TaxTriple,
} from '../components/TaxAmountFields';
import { useCurrentUser } from '../auth';
import { DateField } from '../components/DateField';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
  onChanged: () => void;
}

interface PostableAccount {
  code: string;
  name: string;
  kind: string;
  normal_side: string;
  is_postable: boolean;
  is_active: boolean;
  report_group: string;
  cash_flow_section: string | null;
}

function fixedAssetAccounts(accounts: PostableAccount[], side: string): PostableAccount[] {
  return accounts.filter((a) => a.report_group === 'non_current_asset' && a.normal_side === side);
}

export function FixedAssets({ onToast, reloadKey, onChanged }: Props) {
  const me = useCurrentUser();
  const canRegister = me.permissions.includes('create_fixed_asset');
  const [data, setData] = useState<FixedAssetList | null>(null);
  const [preview, setPreview] = useState<DepreciationPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [drawer, setDrawer] = useState(false);
  const [accounts, setAccounts] = useState<PostableAccount[]>([]);
  const [depts, setDepts] = useState<DimensionList | null>(null);
  const [name, setName] = useState('');
  const [category, setCategory] = useState('');
  const [acquiredOn, setAcquiredOn] = useState('');
  const [amounts, setAmounts] = useState<TaxTriple>(EMPTY_TAX_TRIPLE);
  const [paidFrom, setPaidFrom] = useState('');
  const [glCode, setGlCode] = useState('1601');
  const [accumGlCode, setAccumGlCode] = useState('1602');
  const [voucherNo, setVoucherNo] = useState('');
  const [deptCode, setDeptCode] = useState('');
  const [assetMemo, setAssetMemo] = useState('');
  const [saving, setSaving] = useState(false);

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

  useEffect(() => {
    if (!drawer || !canRegister) return;
    if (accounts.length === 0) {
      callOp<{ accounts: PostableAccount[] }>('list_accounts')
        .then((d) => setAccounts(d.accounts.filter((a) => a.is_postable && a.is_active)))
        .catch(() => setAccounts([]));
    }
    if (!depts) {
      callOp<DimensionList>('list_dimensions')
        .then(setDepts)
        .catch(() => setDepts(null));
    }
  }, [drawer, canRegister, accounts.length, depts]);

  function openDrawer() {
    setName('');
    setCategory(data?.categories[0]?.code ?? '');
    setAcquiredOn('');
    setAmounts(EMPTY_TAX_TRIPLE);
    setPaidFrom('');
    setGlCode('1601');
    setAccumGlCode('1602');
    setVoucherNo('');
    setDeptCode('');
    setAssetMemo('');
    setDrawer(true);
  }

  async function register() {
    setSaving(true);
    try {
      const d = await callOp<{
        asset: { code: string; name: string; cost: string; origin_voucher_no: string | null };
        life_years: number;
        monthly: string;
      }>(
        'create_fixed_asset',
        {
          name: name.trim(),
          category,
          ...(acquiredOn ? { acquired_on: acquiredOn } : {}),
          cost: amounts.net.trim(),
          ...(amounts.tax.trim() ? { tax_amount: amounts.tax.trim() } : {}),
          gl_code: glCode,
          accum_gl_code: accumGlCode,
          ...(paidFrom ? { paid_from_account_code: paidFrom } : {}),
          ...(paidFrom ? {} : { origin_voucher_no: voucherNo.trim() }),
          ...(deptCode ? { dept_code: deptCode } : {}),
          memo: assetMemo.trim(),
        },
        `登記固定資產 ${name.trim()}`,
      );
      onToast(
        d.asset.origin_voucher_no && paidFrom
          ? `已登記 ${d.asset.code} ${d.asset.name}，每月折舊 ${money(d.monthly)}；傳票 ${d.asset.origin_voucher_no} 待過帳，過帳後才計入成本並開始提列`
          : `已登記 ${d.asset.code} ${d.asset.name}，每月折舊 ${money(d.monthly)}`,
      );
      setDrawer(false);
      onChanged();
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSaving(false);
    }
  }

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
                成本合計 <span className="num">{money(data.cost_total)}</span> 等於總帳{' '}
                <span className="num">{data.cost_accounts_label}</span> 的餘額，累計折舊{' '}
                <span className="num">{money(data.accumulated_total)}</span> 等於{' '}
                <span className="num">{data.accum_accounts_label}</span> 的餘額，帳面淨額{' '}
                <span className="num">{money(data.book_total)}</span> 因此與總帳一致。
              </>
            ) : (
              <>
                不相符：成本合計 <span className="num">{money(data.cost_total)}</span> 對{' '}
                <span className="num">{data.cost_accounts_label}</span> 的{' '}
                <span className="num">{money(data.gl_cost)}</span>
                （差 <span className="num">{money(data.cost_difference)}</span>），累計折舊{' '}
                <span className="num">{money(data.accumulated_total)}</span> 對{' '}
                <span className="num">{data.accum_accounts_label}</span> 的{' '}
                <span className="num">{money(data.gl_accumulated)}</span>（差{' '}
                <span className="num">{money(data.accumulated_difference)}</span>）。請先暫緩本月折舊提列。
              </>
            )}
            {' '}總帳餘額計入全部已過帳傳票，不分日期；資產負債表只結算至當天。
            {!isZero(data.not_yet_posted) && (
              <>
                {' '}另有 <span className="num">{money(data.not_yet_posted)}</span> 的資產，
                其取得傳票尚未過帳，故不計入成本也不提折舊。
              </>
            )}
          </p>

          {canRegister && (
            <div className="toolbar">
              <span className="spacer" />
              <button className="btn btn--compact" onClick={openDrawer}>
                ＋ 登記資產
              </button>
            </div>
          )}

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
                      {canRegister
                        ? '尚未登記固定資產。請由上方「＋ 登記資產」建立第一筆。'
                        : '尚未登記固定資產。登記由會計維護。'}
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
            <span className="num">1602 累計折舊</span> 的傳票，狀態為待過帳。
            同一期僅能提列一次。
          </p>
          <p className="hint">
            年限依資產類別查耐用年數表。每筆提列保存當時查得的年數與規則版本。
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
                          {r.ruleset_version ?? '—（未依規則版本提列）'}
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

      {drawer && data && (
        <div className="overlay overlay--center" onClick={() => setDrawer(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">登記資產</div>
                <div className="drawer__subtitle">耐用年數查自耐用年數表</div>
              </div>
              <button className="btn btn--close" onClick={() => setDrawer(false)} aria-label="關閉">
                ×
              </button>
            </div>
            <div className="drawer__body">
              <div className="field-grid">
                <label className="field field--full">
                  資產名稱
                  <input className="input" value={name} onChange={(e) => setName(e.target.value)} />
                </label>
                <label className="field">
                  類別
                  <select
                    className="input"
                    value={category}
                    onChange={(e) => setCategory(e.target.value)}
                  >
                    {data.categories.map((c) => (
                      <option key={c.code} value={c.code}>
                        {c.label}（{c.life_label}）
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  取得日
                  <DateField
                    className="input"
                    value={acquiredOn}
                    onChange={(v) => setAcquiredOn(v)}
                  />
                </label>
                <TaxAmountFields
                  label="取得成本"
                  taxLabel="進項稅額"
                  value={amounts}
                  rate={data.vat.rate}
                  ratePercent={data.vat.rate_percent}
                  onChange={setAmounts}
                />
                <label className="field">
                  付款科目
                  <select
                    className="input"
                    value={paidFrom}
                    onChange={(e) => setPaidFrom(e.target.value)}
                  >
                    <option value="">賒購，填既有傳票號</option>
                    {accounts
                      .filter((a) => a.cash_flow_section === 'cash')
                      .map((a) => (
                        <option key={a.code} value={a.code}>
                          {a.code} {a.name}
                        </option>
                      ))}
                  </select>
                </label>
                <label className="field">
                  取得傳票號
                  <input
                    className="input num"
                    value={paidFrom ? '' : voucherNo}
                    disabled={paidFrom !== ''}
                    onChange={(e) => setVoucherNo(e.target.value)}
                  />
                </label>
                <label className="field">
                  資產科目
                  <select className="input" value={glCode} onChange={(e) => setGlCode(e.target.value)}>
                    {fixedAssetAccounts(accounts, 'debit').map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  累計折舊科目
                  <select
                    className="input"
                    value={accumGlCode}
                    onChange={(e) => setAccumGlCode(e.target.value)}
                  >
                    {fixedAssetAccounts(accounts, 'credit').map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  部門
                  <select
                    className="input"
                    value={deptCode}
                    onChange={(e) => setDeptCode(e.target.value)}
                  >
                    <option value="">未指定</option>
                    {(depts?.dimensions ?? [])
                      .filter((d) => d.code === 'department')
                      .flatMap((d) => d.values)
                      .filter((v) => v.active)
                      .map((v) => (
                        <option key={v.code} value={v.code}>
                          {v.label}
                        </option>
                      ))}
                  </select>
                </label>
                <label className="field field--full">
                  備註
                  <input
                    className="input"
                    value={assetMemo}
                    onChange={(e) => setAssetMemo(e.target.value)}
                  />
                </label>
              </div>
            </div>
            <div className="drawer__foot">
              <span className="drawer__foot-note">
                {paidFrom
                  ? '同時產生待過帳傳票（借資產科目與 1284 進項稅額／貸付款科目）'
                  : '賒購請先在「應付帳款」以 1601 為借方科目開單，再填入該傳票號'}
              </span>
              <button
                className={`btn${saving ? ' btn--off' : ''}`}
                disabled={saving || !name.trim() || !amounts.net.trim()}
                onClick={() => void register()}
              >
                {saving ? '存檔中…' : '登記資產'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
