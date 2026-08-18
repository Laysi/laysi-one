import { useCallback, useEffect, useState } from 'react';
import {
  callOp,
  money,
  OpFailed,
  type ChartOfAccounts,
  type CompanySettingRow,
  type DimensionList,
} from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  reloadKey: number;
}

const ACCOUNT_KINDS: { value: string; label: string }[] = [
  { value: 'asset', label: '資產' },
  { value: 'liability', label: '負債' },
  { value: 'equity', label: '權益' },
  { value: 'revenue', label: '收益' },
  { value: 'expense', label: '費用' },
];

const REPORT_GROUPS_BY_KIND: Record<string, string[]> = {
  asset: ['current_asset', 'non_current_asset'],
  liability: ['current_liability', 'non_current_liability'],
  equity: ['equity'],
  revenue: ['revenue', 'other_income'],
  expense: ['cogs', 'operating_expense', 'other_expense'],
};

const CASH_FLOW_SECTIONS_BY_KIND: Record<string, string[]> = {
  asset: ['cash', 'operating', 'investing', 'financing'],
  liability: ['operating', 'investing', 'financing'],
  equity: ['operating', 'investing', 'financing'],
  revenue: [],
  expense: [],
};

const GROUP_LABEL: Record<string, string> = {
  current_asset: '流動資產',
  non_current_asset: '非流動資產',
  current_liability: '流動負債',
  non_current_liability: '非流動負債',
  equity: '權益',
  revenue: '營業收入',
  cogs: '銷貨成本',
  operating_expense: '營業費用',
  other_income: '營業外收入',
  other_expense: '營業外支出',
};

const SECTION_LABEL: Record<string, string> = {
  cash: '現金本身',
  operating: '營業活動',
  investing: '投資活動',
  financing: '籌資活動',
};

const DEFAULT_NORMAL_SIDE: Record<string, 'debit' | 'credit'> = {
  asset: 'debit',
  liability: 'credit',
  equity: 'credit',
  revenue: 'credit',
  expense: 'debit',
};

interface FullAccount {
  code: string;
  name: string;
  kind: string;
  normal_side: string;
  is_postable: boolean;
  is_active: boolean;
  parent_code: string | null;
  sort_order: number;
  report_group: string;
  cash_flow_section: string | null;
}

interface AccountForm {
  code: string;
  name: string;
  kind: string;
  report_group: string;
  cash_flow_section: string;
  contra: boolean;
  is_postable: boolean;
  is_active: boolean;
  parent_code: string;
  sort_order: string;
}

function emptyAccountForm(): AccountForm {
  return {
    code: '',
    name: '',
    kind: 'asset',
    report_group: REPORT_GROUPS_BY_KIND['asset']?.[0] ?? '',
    cash_flow_section: CASH_FLOW_SECTIONS_BY_KIND['asset']?.[0] ?? '',
    contra: false,
    is_postable: true,
    is_active: true,
    parent_code: '',
    sort_order: '0',
  };
}

export function Setup({ onToast, reloadKey }: Props) {
  const [settings, setSettings] = useState<CompanySettingRow[] | null>(null);
  const [coa, setCoa] = useState<ChartOfAccounts | null>(null);
  const [dims, setDims] = useState<DimensionList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [onlyUsed, setOnlyUsed] = useState(false);

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      callOp<{ settings: CompanySettingRow[] }>('get_company_settings'),
      callOp<DimensionList>('list_dimensions'),
    ])
      .then(([s, d]) => {
        setSettings(s.settings);
        setDims(d);
      })
      .catch((e: Error) => setError(e.message));
    callOp<ChartOfAccounts>('list_chart_of_accounts')
      .then(setCoa)
      .catch(() => setCoa(null));
  }, []);

  useEffect(load, [load, reloadKey]);

  async function save(code: string) {
    try {
      await callOp<unknown>('update_company_setting', { code, value: draft });
      onToast(`已更新「${settings?.find((s) => s.code === code)?.label ?? code}」`);
      setEditing(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  const shown = coa?.accounts.filter((a) => !onlyUsed || a.line_count > 0) ?? [];

  const [accountDrawer, setAccountDrawer] = useState<'create' | 'edit' | null>(null);
  const [accountForm, setAccountForm] = useState<AccountForm>(emptyAccountForm());
  const [savingAccount, setSavingAccount] = useState(false);
  const [fullAccounts, setFullAccounts] = useState<FullAccount[] | null>(null);

  function loadFullAccounts(): Promise<FullAccount[]> {
    if (fullAccounts) return Promise.resolve(fullAccounts);
    return callOp<{ accounts: FullAccount[] }>('list_accounts').then((d) => {
      setFullAccounts(d.accounts);
      return d.accounts;
    });
  }

  function openCreateAccount() {
    setAccountForm(emptyAccountForm());
    setAccountDrawer('create');
  }

  async function openEditAccount(code: string) {
    try {
      const accounts = await loadFullAccounts();
      const a = accounts.find((x) => x.code === code);
      if (!a) {
        onToast(`無法讀取科目 ${code} 的完整資料`);
        return;
      }
      const defaultSide = DEFAULT_NORMAL_SIDE[a.kind] ?? 'debit';
      setAccountForm({
        code: a.code,
        name: a.name,
        kind: a.kind,
        report_group: a.report_group,
        cash_flow_section: a.cash_flow_section ?? '',
        contra: a.normal_side !== defaultSide,
        is_postable: a.is_postable,
        is_active: a.is_active,
        parent_code: a.parent_code ?? '',
        sort_order: String(a.sort_order),
      });
      setAccountDrawer('edit');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    }
  }

  function setAccountKind(kind: string) {
    setAccountForm((f) => ({
      ...f,
      kind,
      report_group: REPORT_GROUPS_BY_KIND[kind]?.[0] ?? '',
      cash_flow_section: CASH_FLOW_SECTIONS_BY_KIND[kind]?.[0] ?? '',
      contra: false,
    }));
  }

  function setAccountField<K extends keyof AccountForm>(key: K, value: AccountForm[K]) {
    setAccountForm((f) => ({ ...f, [key]: value }));
  }

  async function saveAccount() {
    if (!accountForm.code.trim() || !accountForm.name.trim()) {
      onToast('科目代號與名稱都要填寫');
      return;
    }
    const isPl = accountForm.kind === 'revenue' || accountForm.kind === 'expense';
    const defaultSide = DEFAULT_NORMAL_SIDE[accountForm.kind] ?? 'debit';
    const oppositeSide = defaultSide === 'debit' ? 'credit' : 'debit';
    setSavingAccount(true);
    try {
      const input = {
        code: accountForm.code.trim(),
        name: accountForm.name.trim(),
        kind: accountForm.kind,
        report_group: accountForm.report_group,

        cash_flow_section: isPl ? null : accountForm.cash_flow_section || null,
        ...(accountForm.contra ? { normal_side: oppositeSide } : {}),
        is_postable: accountForm.is_postable,
        is_active: accountForm.is_active,
        parent_code: accountForm.parent_code.trim() || null,
        sort_order: parseInt(accountForm.sort_order, 10) || 0,
      };
      const op = accountDrawer === 'edit' ? 'update_account' : 'create_account';
      await callOp<unknown>(
        op,
        input,
        accountDrawer === 'edit' ? `更新科目 ${accountForm.code}` : `新增科目 ${accountForm.name}`,
      );
      onToast(`科目 ${accountForm.code} 已儲存`);
      setAccountDrawer(null);
      setFullAccounts(null);
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingAccount(false);
    }
  }

  const [dimCode, setDimCode] = useState('');
  const [valueCode, setValueCode] = useState('');
  const [valueLabel, setValueLabel] = useState('');
  const [savingDim, setSavingDim] = useState(false);

  useEffect(() => {
    if (!dimCode && dims && dims.dimensions.length > 0) {
      setDimCode(dims.dimensions[0]?.code ?? '');
    }
  }, [dims, dimCode]);

  async function saveDimensionValue() {
    if (!dimCode || !valueCode.trim() || !valueLabel.trim()) {
      onToast('維度、代號與名稱都要填寫');
      return;
    }
    setSavingDim(true);
    try {
      await callOp<unknown>(
        'create_dimension_value',
        { dimension_code: dimCode, code: valueCode.trim(), label: valueLabel.trim() },
        `於 ${dimCode} 新增維度值`,
      );
      onToast(`已新增「${valueLabel.trim()}」`);
      setValueCode('');
      setValueLabel('');
      load();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingDim(false);
    }
  }

  const isPlKind = accountForm.kind === 'revenue' || accountForm.kind === 'expense';

  return (
    <div>
      <h1 className="page-title">基礎設定</h1>
      <p className="page-lede">變更會影響全系統，僅系統管理員可異動。</p>

      {error && <p className="state-note state-note--error">{error}</p>}
      {!settings && !error && <p className="state-note">讀取中…</p>}

      {settings && dims && (
        <>
          <div className="split-columns">
            <section>
              <h3 className="section-title">公司與帳務</h3>
              <div className="card fact-card">
                {settings.map((s) => (
                  <div className="fact" key={s.code}>
                    <span className="fact__key">{s.label}</span>
                    {editing === s.code ? (
                      <span className="fact__edit">
                        <input
                          className="entry-input"
                          value={draft}
                          autoFocus
                          onChange={(e) => setDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') void save(s.code);
                            if (e.key === 'Escape') setEditing(null);
                          }}
                        />
                        <button className="btn btn--compact" onClick={() => void save(s.code)}>
                          儲存
                        </button>
                        <button className="btn--icon" onClick={() => setEditing(null)}>
                          ×
                        </button>
                      </span>
                    ) : (
                      <span className="fact__value">
                        {s.value || '（尚未設定）'}
                        {s.source === 'regulation' && (
                          <span className="tag tag--pale tag--static">
                            來自法規 {s.ruleset?.version}
                          </span>
                        )}
                        {s.editable && (
                          <button
                            className="btn--icon fact__pencil"
                            title="編輯此項"
                            onClick={() => {
                              setEditing(s.code);
                              setDraft(s.value);
                            }}
                          >
                            ✎
                          </button>
                        )}
                        {s.note && <span className="fact__note">{s.note}</span>}
                      </span>
                    )}
                  </div>
                ))}
              </div>

              <p className="recon">營業稅率依適用日自動帶入，不可變更。</p>
              <p className="hint">標有編輯圖示的項目僅系統管理員可修改。</p>

              <h3 className="section-title section-title--spaced">維度與條件</h3>
              <div className="card fact-card">
                {dims.dimensions.map((d) => (
                  <div className="fact fact--stacked" key={d.code}>
                    <span className="fact__key">{d.label}</span>
                    <span className="fact__value">
                      {d.values.length > 0 ? d.values.map((v) => v.label).join('　') : '（尚無值）'}
                      {d.note && <span className="fact__note">{d.note}</span>}
                    </span>
                  </div>
                ))}
              </div>

              <h3 className="section-title section-title--spaced">新增維度值</h3>
              <div className="card form-card">
                <div className="field-grid">
                  <label className="field">
                    維度
                    <select className="input" value={dimCode} onChange={(e) => setDimCode(e.target.value)}>
                      {dims.dimensions.map((d) => (
                        <option key={d.code} value={d.code}>
                          {d.label}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label className="field">
                    代號
                    <input
                      className="input num"
                      value={valueCode}
                      placeholder="sales"
                      onChange={(e) => setValueCode(e.target.value)}
                    />
                  </label>
                  <label className="field field--full">
                    顯示名稱
                    <input
                      className="input"
                      value={valueLabel}
                      placeholder="業務部"
                      onChange={(e) => setValueLabel(e.target.value)}
                    />
                  </label>
                </div>
                <button className="btn btn--compact" disabled={savingDim} onClick={() => void saveDimensionValue()}>
                  {savingDim ? '存檔中…' : '＋ 新增'}
                </button>
              </div>

              {!dims.applied_to_entries && (
                <p className="hint">維度尚未掛載至分錄，現階段填寫不影響任何報表。</p>
              )}
            </section>

            {coa && (
              <section>
                <h3 className="section-title">會計科目表</h3>
                <p className="section-note">
                  每個科目都要指定它在報表中的段落，以及在現金流量表中的活動別。缺少這兩欄的科目不會出現在報表上。
                </p>
                <div className="toolbar">
                  <button
                    className={`chip${onlyUsed ? ' chip--on' : ''}`}
                    onClick={() => setOnlyUsed((v) => !v)}
                  >
                    僅顯示有分錄者
                  </button>
                  <span className="spacer" />
                  <span className="section-note">
                    {coa.total} 個科目，{coa.used} 個有分錄
                  </span>
                  <button className="btn btn--compact" onClick={openCreateAccount}>
                    ＋ 新增科目
                  </button>
                </div>
                <div className="table-scroll">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>代號</th>
                        <th>科目</th>
                        <th>報表位置</th>
                        <th className="align-right">餘額</th>
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {shown.map((a) => (
                        <tr key={a.code} className={a.line_count === 0 ? 'row--waiting' : undefined}>
                          <td className="num cell-nowrap">{a.code}</td>
                          <td>
                            {a.name}
                            <span className="cell-hint">
                              {a.kind_label} · {a.normal_side_label}
                            </span>
                          </td>
                          <td className="cell-accounts">
                            {a.report_group_label}
                            <span className="cell-hint cell-hint--block">
                              現金流量：{a.cash_flow_label}
                            </span>
                          </td>
                          <td className="num align-right">
                            {a.line_count === 0 ? (
                              <span className="cell-muted">—</span>
                            ) : (
                              money(a.balance)
                            )}
                          </td>
                          <td className="align-right">
                            <button
                              className="btn btn--compact btn--secondary"
                              onClick={() => void openEditAccount(a.code)}
                            >
                              編輯
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <p className="hint">此處餘額與財務報表為同一組數字。</p>
              </section>
            )}
          </div>
        </>
      )}

      {accountDrawer && (
        <div className="overlay" onClick={() => setAccountDrawer(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <div className="drawer__head">
              <div>
                <div className="drawer__title">
                  {accountDrawer === 'edit' ? `編輯科目 ${accountForm.code}` : '新增科目'}
                </div>
                <div className="drawer__subtitle">報表分組與現金流量活動別為必填，決定科目出現在報表的哪一段</div>
              </div>
              <button className="btn btn--close" onClick={() => setAccountDrawer(null)} aria-label="關閉">
                ×
              </button>
            </div>

            <div className="drawer__body">
              <div className="field-grid">
                <label className="field">
                  科目代號
                  <input
                    className="input num"
                    value={accountForm.code}
                    disabled={accountDrawer === 'edit'}
                    placeholder="1103"
                    onChange={(e) => setAccountField('code', e.target.value)}
                  />
                  {accountDrawer === 'edit' && <span className="field__hint">編輯時不可變更代號</span>}
                </label>
                <label className="field">
                  科目名稱
                  <input
                    className="input"
                    value={accountForm.name}
                    onChange={(e) => setAccountField('name', e.target.value)}
                  />
                </label>

                <label className="field">
                  類別
                  <select
                    className="input"
                    value={accountForm.kind}
                    onChange={(e) => setAccountKind(e.target.value)}
                  >
                    {ACCOUNT_KINDS.map((k) => (
                      <option key={k.value} value={k.value}>
                        {k.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  報表分組
                  <select
                    className="input"
                    value={accountForm.report_group}
                    onChange={(e) => setAccountField('report_group', e.target.value)}
                  >
                    {(REPORT_GROUPS_BY_KIND[accountForm.kind] ?? []).map((g) => (
                      <option key={g} value={g}>
                        {GROUP_LABEL[g] ?? g}
                      </option>
                    ))}
                  </select>
                </label>

                {isPlKind ? (
                  <div className="field field--full">
                    <span>現金流量活動別</span>
                    <p className="field__hint">
                      損益科目一律併入本期損益，不出現在現金流量表的三段活動中，本欄不需填寫。
                    </p>
                  </div>
                ) : (
                  <label className="field">
                    現金流量活動別
                    <select
                      className="input"
                      value={accountForm.cash_flow_section}
                      onChange={(e) => setAccountField('cash_flow_section', e.target.value)}
                    >
                      {(CASH_FLOW_SECTIONS_BY_KIND[accountForm.kind] ?? []).map((sec) => (
                        <option key={sec} value={sec}>
                          {SECTION_LABEL[sec] ?? sec}
                        </option>
                      ))}
                    </select>
                    {accountForm.kind === 'asset' && (
                      <span className="field__hint">僅資產科目可選「現金本身」</span>
                    )}
                  </label>
                )}

                <label className="field">
                  父科目代號（選填）
                  <input
                    className="input num"
                    value={accountForm.parent_code}
                    placeholder="可留空"
                    onChange={(e) => setAccountField('parent_code', e.target.value)}
                  />
                </label>
                <label className="field">
                  顯示順序
                  <input
                    className="input num"
                    inputMode="numeric"
                    value={accountForm.sort_order}
                    onChange={(e) => setAccountField('sort_order', e.target.value)}
                  />
                </label>
              </div>

              <div className="toolbar">
                <button
                  className={`chip${accountForm.is_postable ? ' chip--on' : ''}`}
                  onClick={() => setAccountField('is_postable', !accountForm.is_postable)}
                >
                  {accountForm.is_postable ? '可直接開立分錄' : '不可開立分錄（彙總科目）'}
                </button>
                <button
                  className={`chip${accountForm.is_active ? ' chip--on' : ''}`}
                  onClick={() => setAccountField('is_active', !accountForm.is_active)}
                >
                  {accountForm.is_active ? '啟用中' : '已停用'}
                </button>
                <button
                  className={`chip${accountForm.contra ? ' chip--on' : ''}`}
                  onClick={() => setAccountField('contra', !accountForm.contra)}
                >
                  備抵科目（餘額方向相反）
                </button>
              </div>
              <p className="field__hint">
                正常餘額方向依類別自動帶入（
                {DEFAULT_NORMAL_SIDE[accountForm.kind] === 'debit' ? '借方' : '貸方'}）；
                僅「累計折舊」等備抵科目需開啟上方切換，將方向反轉。
              </p>
            </div>

            <div className="drawer__foot">
              <span className="drawer__foot-note">
                {accountDrawer === 'edit' ? '完整更新本科目，不提供刪除' : '建立會計科目'}
              </span>
              <button
                className={`btn${savingAccount ? ' btn--off' : ''}`}
                disabled={savingAccount}
                onClick={() => void saveAccount()}
              >
                {savingAccount ? '存檔中…' : '存檔'}
              </button>
            </div>
          </aside>
        </div>
      )}
    </div>
  );
}
