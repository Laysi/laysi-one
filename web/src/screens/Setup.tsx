import { useCallback, useEffect, useState } from 'react';
import { useCurrentUser } from '../auth';
import { TimeField } from '../components/TimeField';
import {
  callOp,
  money,
  OpFailed,
  type ChartAccount,
  type ChartOfAccounts,
  type CompanySettingRow,
  type DimensionList,
  type UserAccountRow,
} from '../api/client';
import type { ScreenId } from '../nav';

interface Props {
  onGo: (screen: ScreenId) => void;
  onToast: (msg: string) => void;
  reloadKey: number;
}

const CLOCK_SETTINGS = new Set(['work_start', 'work_end']);
const TIMEZONE_SETTING = 'business_utc_offset';
const UTC_OFFSETS = [
  '-12:00', '-11:00', '-10:00', '-09:30', '-09:00', '-08:00', '-07:00', '-06:00',
  '-05:00', '-04:00', '-03:30', '-03:00', '-02:00', '-01:00', '+00:00', '+01:00',
  '+02:00', '+03:00', '+03:30', '+04:00', '+04:30', '+05:00', '+05:30', '+05:45',
  '+06:00', '+06:30', '+07:00', '+08:00', '+08:45', '+09:00', '+09:30', '+10:00',
  '+10:30', '+11:00', '+12:00', '+12:45', '+13:00', '+14:00',
];

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

const SIDE_LABEL: Record<string, string> = { debit: '借方', credit: '貸方' };

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

function structureOnly(a: FullAccount): ChartAccount {
  return {
    code: a.code,
    name: a.name,
    kind: a.kind,
    kind_label: ACCOUNT_KINDS.find((k) => k.value === a.kind)?.label ?? a.kind,
    normal_side: a.normal_side,
    normal_side_label: SIDE_LABEL[a.normal_side] ?? a.normal_side,
    is_postable: a.is_postable,
    report_group: a.report_group,
    report_group_label: GROUP_LABEL[a.report_group] ?? a.report_group,
    cash_flow_section: a.cash_flow_section,
    cash_flow_label: a.cash_flow_section
      ? (SECTION_LABEL[a.cash_flow_section] ?? '')
      : '併入本期損益',
    balance: '0',
    line_count: 0,
  };
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

export function Setup({ onGo, onToast, reloadKey }: Props) {
  const me = useCurrentUser();
  const canEditSettings = me.permissions.includes('update_company_setting');
  const canSetTimezone = me.permissions.includes('set_company_timezone');
  const canAddDimension = me.permissions.includes('create_dimension_value');
  const showBalances = me.permissions.includes('list_chart_of_accounts');
  const [settings, setSettings] = useState<CompanySettingRow[] | null>(null);
  const [coa, setCoa] = useState<ChartOfAccounts | null>(null);
  const [dims, setDims] = useState<DimensionList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [onlyUsed, setOnlyUsed] = useState(false);
  const [ownerMissing, setOwnerMissing] = useState(false);

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
    if (me.permissions.includes('list_user_accounts')) {
      callOp<{ accounts: UserAccountRow[] }>('list_user_accounts')
        .then((d) =>
          setOwnerMissing(
            !d.accounts.some((a) => a.role_code === 'boss' && a.status === 'active'),
          ),
        )
        .catch(() => setOwnerMissing(false));
    }
    if (showBalances) {
      callOp<ChartOfAccounts>('list_chart_of_accounts')
        .then(setCoa)
        .catch(() => setCoa(null));
      return;
    }
    callOp<{ accounts: FullAccount[] }>('list_accounts')
      .then((d) => {
        setFullAccounts(d.accounts);
        setCoa({
          accounts: d.accounts.map(structureOnly),
          total: d.accounts.length,
          used: 0,
          as_of: '',
        });
      })
      .catch(() => setCoa(null));
  }, [showBalances, me.permissions]);

  useEffect(load, [load, reloadKey]);

  async function save(code: string) {
    try {
      if (code === TIMEZONE_SETTING) {
        await callOp<unknown>('set_company_timezone', { offset: draft });
        onToast(`記帳時區已改成 UTC${draft}`);
        setEditing(null);
        load();
        return;
      }
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

  const [adding, setAdding] = useState<string | null>(null);
  const [valueLabel, setValueLabel] = useState('');
  const [savingDim, setSavingDim] = useState(false);

  async function saveDimensionValue(dimensionCode: string) {
    const label = valueLabel.trim();
    if (!label) {
      onToast('請填寫名稱');
      return;
    }
    setSavingDim(true);
    try {
      await callOp<unknown>(
        'create_dimension_value',
        { dimension_code: dimensionCode, label },
        '新增分類項目',
      );
      onToast(`已新增「${label}」`);
      setAdding(null);
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
      {ownerMissing && (
        <div className="placeholder placeholder--inline">
          <div className="placeholder__title">下一步：建立負責人帳號</div>
          <p>帳務、薪資與簽核門檻屬負責人權限，本帳號無法查看，也不能將自己變更為負責人。</p>
          <button className="btn" onClick={() => onGo('acc')}>
            前往帳號管理
          </button>
        </div>
      )}
      {error && <p className="state-note state-note--error">{error}</p>}
      {!settings && !error && <p className="state-note">讀取中…</p>}

      {settings && dims && (
        <>
          <div className="split-columns split-columns--wide-right">
            <section>
              <h3 className="section-title">公司與帳務</h3>
              <div className="card fact-card">
                {settings.map((s) => (
                  <div className="fact" key={s.code}>
                    <span className="fact__key">{s.label}</span>
                    {editing === s.code ? (
                      <span className="fact__edit">
                        {s.code === TIMEZONE_SETTING ? (
                          <select
                            className="entry-input"
                            value={draft}
                            autoFocus
                            onChange={(e) => setDraft(e.target.value)}
                          >
                            {UTC_OFFSETS.map((o) => (
                              <option key={o} value={o}>
                                UTC{o}
                              </option>
                            ))}
                          </select>
                        ) : s.options?.length ? (
                          <select
                            className="entry-input"
                            value={draft}
                            autoFocus
                            onChange={(e) => setDraft(e.target.value)}
                          >
                            {s.options.map((o) => (
                              <option key={o} value={o}>
                                {o}
                              </option>
                            ))}
                          </select>
                        ) : CLOCK_SETTINGS.has(s.code) ? (
                          <TimeField label={s.label} value={draft} onChange={setDraft} />
                        ) : (
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
                        )}
                        <button className="btn btn--compact" onClick={() => void save(s.code)}>
                          儲存
                        </button>
                        <button className="btn--icon" onClick={() => setEditing(null)}>
                          ×
                        </button>
                      </span>
                    ) : (
                      <span className="fact__value">
                        {s.code === TIMEZONE_SETTING
                          ? `UTC${s.value}`
                          : s.value || '（尚未設定）'}
                        {s.source === 'regulation' && (
                          <span className="tag tag--pale tag--static">
                            來自法規 {s.ruleset?.version}
                          </span>
                        )}
                        {((s.editable && canEditSettings) ||
                          (s.code === TIMEZONE_SETTING && canSetTimezone)) && (
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

              <p className="hint">
                {canEditSettings
                  ? '標有編輯圖示的項目可修改，變更立即套用到全系統。'
                  : '公司設定由系統管理員維護。'}
              </p>

              <h3 className="section-title section-title--spaced">部門</h3>
              <div className="card fact-card">
                {dims.dimensions.map((d) => (
                  <div className="fact fact--stacked" key={d.code}>
                    <span className="fact__key">{d.label}</span>
                    {adding === d.code ? (
                      <span className="fact__edit">
                        <input
                          className="entry-input"
                          value={valueLabel}
                          autoFocus
                          onChange={(e) => setValueLabel(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') void saveDimensionValue(d.code);
                            if (e.key === 'Escape') setAdding(null);
                          }}
                        />
                        <button
                          className="btn btn--compact"
                          disabled={savingDim}
                          onClick={() => void saveDimensionValue(d.code)}
                        >
                          {savingDim ? '存檔中…' : '新增'}
                        </button>
                        <button className="btn--icon" onClick={() => setAdding(null)}>
                          ×
                        </button>
                      </span>
                    ) : (
                      <span className="fact__value">
                        {d.values.length > 0
                          ? d.values.map((v) => v.label).join('　')
                          : '（尚未建立）'}
                        {canAddDimension && (
                          <button
                            className="btn--icon fact__pencil"
                            title={`新增${d.label}`}
                            onClick={() => {
                              setAdding(d.code);
                              setValueLabel('');
                            }}
                          >
                            ＋
                          </button>
                        )}
                        {d.note && <span className="fact__note">{d.note}</span>}
                      </span>
                    )}
                  </div>
                ))}
              </div>

            </section>

            {coa && (
              <section>
                <h3 className="section-title">會計科目表</h3>
                <p className="section-note">
                  每個科目都要指定報表段落與現金流量活動別。缺少這兩欄的科目不會出現在報表上。
                </p>
                <div className="toolbar">
                  {showBalances && (
                    <button
                      className={`chip${onlyUsed ? ' chip--on' : ''}`}
                      onClick={() => setOnlyUsed((v) => !v)}
                    >
                      僅顯示有分錄者
                    </button>
                  )}
                  <span className="spacer" />
                  <span className="section-note">
                    {coa.total} 個科目{showBalances && `，${coa.used} 個有分錄`}
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
                        {showBalances && <th className="align-right">餘額</th>}
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {shown.map((a) => (
                        <tr
                          key={a.code}
                          className={
                            showBalances && a.line_count === 0 ? 'row--waiting' : undefined
                          }
                        >
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
                          {showBalances && (
                            <td className="num align-right">
                              {a.line_count === 0 ? (
                                <span className="cell-muted">—</span>
                              ) : (
                                money(a.balance)
                              )}
                            </td>
                          )}
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
                <div className="drawer__subtitle">報表分組與現金流量活動別為必填，決定科目在報表中的段落</div>
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
                      損益科目不需填寫本欄。
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
                {accountDrawer === 'edit' ? '更新會計科目' : '建立會計科目'}
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
