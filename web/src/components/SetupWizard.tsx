import { useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  type CompanySettingRow,
  type DimensionList,
  type NewUserAccount,
  type OidcSetting,
  type UserAccountList,
} from '../api/client';
import { TimeField } from './TimeField';

interface Props {
  onToast: (msg: string) => void;
  onProgress: () => void;
  onExit: () => void;
}

type Step = 'company' | 'bank' | 'dept' | 'hours' | 'sso' | 'owner' | 'done';

type HourCode = 'work_start' | 'work_end' | 'late_grace_minutes';

const STEPS: { id: Step; label: string }[] = [
  { id: 'company', label: '① 公司資料' },
  { id: 'bank', label: '② 銀行帳戶' },
  { id: 'dept', label: '③ 部門' },
  { id: 'hours', label: '④ 上下班時間' },
  { id: 'sso', label: '⑤ 外部登入' },
  { id: 'owner', label: '⑥ 負責人帳號' },
  { id: 'done', label: '⑦ 完成' },
];

const GOOGLE_ISSUER = 'https://accounts.google.com';
const CALLBACK_PATH = '/v1/auth/oauth/callback';

interface Account {
  code: string;
  name: string;
  kind: string;
  is_postable: boolean;
  is_active: boolean;
  cash_flow_section: string | null;
}

const BANK_GL = '1102';

export function SetupWizard({ onToast, onProgress, onExit }: Props) {
  const [step, setStep] = useState<Step>('company');

  const [settings, setSettings] = useState<CompanySettingRow[] | null>(null);
  const [companyName, setCompanyName] = useState('');
  const [taxId, setTaxId] = useState('');
  const [fiscalMonth, setFiscalMonth] = useState('1');
  const [savingCompany, setSavingCompany] = useState(false);

  useEffect(() => {
    callOp<{ settings: CompanySettingRow[] }>('get_company_settings')
      .then((d) => {
        setSettings(d.settings);
        setCompanyName(d.settings.find((s) => s.code === 'company_name')?.value ?? '');
        setTaxId(d.settings.find((s) => s.code === 'tax_id')?.value ?? '');
        setFiscalMonth(d.settings.find((s) => s.code === 'fiscal_year')?.value || '1');
        const stored = {
          work_start: d.settings.find((s) => s.code === 'work_start')?.value ?? '',
          work_end: d.settings.find((s) => s.code === 'work_end')?.value ?? '',
          late_grace_minutes:
            d.settings.find((s) => s.code === 'late_grace_minutes')?.value ?? '',
        };
        setSavedHours(stored);
        setWorkStart(stored.work_start);
        setWorkEnd(stored.work_end);
        setGraceMinutes(stored.late_grace_minutes);
      })
      .catch(() => setSettings([]));
  }, []);

  async function saveCompany() {
    if (!companyName.trim()) {
      onToast('請輸入公司名稱。');
      return;
    }
    if (taxId.trim() && !/^\d{8}$/.test(taxId.trim())) {
      onToast('統一編號是 8 位數字。');
      return;
    }
    setSavingCompany(true);
    try {
      await callOp<unknown>('update_company_setting', {
        code: 'company_name',
        value: companyName.trim(),
      });
      if (taxId.trim()) {
        await callOp<unknown>('update_company_setting', { code: 'tax_id', value: taxId.trim() });
      }
      await callOp<unknown>('update_company_setting', {
        code: 'fiscal_year',
        value: fiscalMonth,
      });
      onToast('公司資料已儲存');
      onProgress();
      setStep('bank');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingCompany(false);
    }
  }

  const [accounts, setAccounts] = useState<Account[] | null>(null);
  const [bankName, setBankName] = useState('');
  const [bankNo, setBankNo] = useState('');
  const [glCode, setGlCode] = useState('');
  const [savingBank, setSavingBank] = useState(false);

  useEffect(() => {
    if (step !== 'bank' || accounts) return;
    callOp<{ accounts: Account[] }>('list_accounts')
      .then((d) => {
        const usable = d.accounts.filter(
          (a) =>
            a.kind === 'asset' &&
            a.is_postable &&
            a.is_active &&
            a.cash_flow_section === 'cash',
        );
        setAccounts(usable);
        setGlCode((code) => code || (usable.some((a) => a.code === BANK_GL) ? BANK_GL : usable[0]?.code) || '');
      })
      .catch(() => setAccounts([]));
  }, [step, accounts]);

  async function saveBank() {
    if (!bankName.trim() || !bankNo.trim() || !glCode) {
      onToast('帳戶名稱、帳號與總帳科目都要填寫');
      return;
    }
    setSavingBank(true);
    try {
      await callOp<{ bank_account: { name: string; account_no: string } }>(
        'create_bank_account',
        { name: bankName.trim(), account_no: bankNo.trim(), gl_code: glCode },
        '設定第一個銀行帳戶',
      );
      onToast(`已建立銀行帳戶「${bankName.trim()}」`);
      onProgress();
      setStep('dept');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingBank(false);
    }
  }

  const [workStart, setWorkStart] = useState('');
  const [workEnd, setWorkEnd] = useState('');
  const [graceMinutes, setGraceMinutes] = useState('');
  const [savedHours, setSavedHours] = useState<Record<HourCode, string>>({
    work_start: '',
    work_end: '',
    late_grace_minutes: '',
  });
  const [savingHours, setSavingHours] = useState(false);

  const [ownerExists, setOwnerExists] = useState<boolean | null>(null);
  const [ownerDisplayName, setOwnerDisplayName] = useState('');
  const [ownerEmail, setOwnerEmail] = useState('');
  const [ownerSecret, setOwnerSecret] = useState<NewUserAccount | null>(null);
  const [savingOwner, setSavingOwner] = useState(false);
  const [ownerCopied, setOwnerCopied] = useState(false);

  const [oidc, setOidc] = useState<OidcSetting | null>(null);
  const [ssoClientId, setSsoClientId] = useState('');
  const [ssoSecret, setSsoSecret] = useState('');
  const [ssoDomain, setSsoDomain] = useState('');
  const [ssoRole, setSsoRole] = useState('');
  const [ssoPasswordOff, setSsoPasswordOff] = useState(false);
  const [savingSso, setSavingSso] = useState(false);

  useEffect(() => {
    if (step !== 'sso' || oidc) return;
    callOp<OidcSetting>('get_oidc_setting')
      .then((d) => {
        setOidc(d);
        setSsoClientId(d.client_id);
        setSsoDomain(d.allowed_domain);
        setSsoRole(d.provision_role || 'staff');
        setSsoPasswordOff(d.password_login_disabled);
      })
      .catch(() => setOidc(null));
  }, [step, oidc]);

  const ssoRedirect = `${window.location.origin}${CALLBACK_PATH}`;
  const ssoDomainReady = (oidc?.enabled ?? false) && oidc!.provision_role !== '' && oidc!.allowed_domain !== '';
  const ownerInDomain =
    ssoDomainReady && ownerEmail.trim().toLowerCase().endsWith(`@${oidc!.allowed_domain.toLowerCase()}`);
  const passwordIsDead = (oidc?.password_login_disabled ?? false) && !ownerInDomain;

  async function saveSso() {
    if (!ssoClientId.trim() || !ssoDomain.trim()) {
      onToast('用戶端 ID 與公司電子郵件網域都要填寫');
      return;
    }
    if (!oidc?.has_client_secret && !ssoSecret.trim()) {
      onToast('請貼上用戶端密鑰');
      return;
    }
    setSavingSso(true);
    try {
      const input: Record<string, unknown> = {
        issuer: GOOGLE_ISSUER,
        client_id: ssoClientId.trim(),
        redirect_url: ssoRedirect,
        allowed_domain: ssoDomain.trim(),
        provision_role: ssoRole,
        enabled: true,
        password_login_disabled: ssoPasswordOff,
      };
      if (ssoSecret.trim()) input.client_secret = ssoSecret.trim();
      await callOp<unknown>('update_oidc_setting', input, '設定外部登入');
      const fresh = await callOp<OidcSetting>('get_oidc_setting');
      setOidc(fresh);
      setSsoSecret('');
      if (fresh.verify_error) {
        onToast(fresh.verify_error);
        return;
      }
      onToast(`已啟用外部登入，限 @${fresh.allowed_domain}`);
      onProgress();
      setStep('owner');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingSso(false);
    }
  }

  const [dims, setDims] = useState<DimensionList | null>(null);
  const [deptLabel, setDeptLabel] = useState('');
  const [savingDept, setSavingDept] = useState(false);

  useEffect(() => {
    if (step !== 'dept' || dims) return;
    callOp<DimensionList>('list_dimensions').then(setDims).catch(() => setDims(null));
  }, [step, dims]);

  const deptDimension = dims?.dimensions.find((d) => d.code === 'department') ?? null;

  async function saveDept() {
    if (!deptLabel.trim()) {
      onToast('請填寫部門名稱');
      return;
    }
    setSavingDept(true);
    try {
      await callOp<unknown>(
        'create_dimension_value',
        { dimension_code: 'department', label: deptLabel.trim() },
        '設定第一個部門',
      );
      onToast(`已建立部門「${deptLabel.trim()}」`);
      onProgress();
      setStep('hours');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingDept(false);
    }
  }

  useEffect(() => {
    callOp<UserAccountList>('list_user_accounts')
      .then((d) =>
        setOwnerExists(d.accounts.some((a) => a.role_code === 'boss' && a.status === 'active')),
      )
      .catch(() => setOwnerExists(null));
  }, []);

  async function createOwner() {
    if (ownerEmail.trim() === '' || ownerDisplayName.trim() === '') {
      onToast('電子郵件與姓名都要填寫。');
      return;
    }
    setSavingOwner(true);
    try {
      const d = await callOp<NewUserAccount>(
        'create_user_account',
        {
          email: ownerEmail.trim(),
          display_name: ownerDisplayName.trim(),
          role_code: 'boss',
          employee_no: '',
          external_login_only: ownerInDomain,
        },
        `建立負責人帳號 ${ownerEmail.trim()}`,
      );
      setOwnerSecret(d);
      setOwnerCopied(false);
      setOwnerExists(true);
      onProgress();
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingOwner(false);
    }
  }

  async function copyOwnerPassword() {
    if (!ownerSecret?.temporary_password) return;
    try {
      await navigator.clipboard.writeText(ownerSecret.temporary_password);
      setOwnerCopied(true);
      onToast(`已複製 ${ownerSecret.user.username} 的密碼`);
    } catch {
      onToast('這個瀏覽器不允許代為複製，請手動選取密碼');
    }
  }

  async function saveHours() {
    if (graceMinutes.trim() && !/^\d{1,3}$/.test(graceMinutes.trim())) {
      onToast('遲到寬限請填分鐘數。');
      return;
    }
    const fields: { code: HourCode; label: string; value: string; unit: string }[] = [
      { code: 'work_start', label: '上班時間', value: workStart.trim(), unit: '' },
      { code: 'work_end', label: '下班時間', value: workEnd.trim(), unit: '' },
      { code: 'late_grace_minutes', label: '遲到寬限', value: graceMinutes.trim(), unit: ' 分鐘' },
    ];
    const changed = fields.filter((f) => f.value !== savedHours[f.code]);
    if (changed.length === 0) {
      onToast(
        fields.every((f) => f.value === '')
          ? '還沒填上下班時間，打卡不判定遲到；之後可在「基礎設定」補上。'
          : '上下班時間沒有變更。',
      );
      setStep('sso');
      return;
    }

    setSavingHours(true);
    try {
      for (const f of changed) {
        await callOp<unknown>('update_company_setting', { code: f.code, value: f.value });
      }
      setSavedHours(Object.fromEntries(fields.map((f) => [f.code, f.value])) as Record<
        HourCode,
        string
      >);
      onToast(
        `已儲存：${changed
          .map((f) => (f.value ? `${f.label} ${f.value}${f.unit}` : `${f.label}清為尚未設定`))
          .join('、')}`,
      );
      onProgress();
      setStep('sso');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingHours(false);
    }
  }

  return (
    <div>
      <div className="field-row" style={{ alignItems: 'baseline', justifyContent: 'space-between' }}>
        <div>
          <h1 className="page-title">首次設定</h1>
          <p className="page-lede">可隨時略過，稍後從左側繼續。</p>
        </div>
        <button className="btn btn--ghost" onClick={onExit}>
          略過設定
        </button>
      </div>

      <div className="wizard-steps">
        {STEPS.map((s, i) => (
          <div
            key={s.id}
            className={`wizard-step${
              s.id === step ? ' wizard-step--on' : i < STEPS.findIndex((x) => x.id === step) ? ' wizard-step--done' : ''
            }`}
          >
            {s.label}
          </div>
        ))}
      </div>

      {step === 'company' && (
        <div className="card wizard-panel">
          <h3 className="section-title">公司資料</h3>
          {!settings && <p className="state-note">讀取中…</p>}
          {settings && (
            <>
              <label className="field field--full">
                公司名稱（必填）
                <input
                  className="input"
                  value={companyName}
                  autoFocus
                  placeholder="範例貿易有限公司"
                  onChange={(e) => setCompanyName(e.target.value)}
                />
              </label>
              <label className="field field--full">
                統一編號
                <input
                  className="input num"
                  value={taxId}
                  placeholder="12345675"
                  onChange={(e) => setTaxId(e.target.value)}
                />
              </label>

              <label className="field field--full">
                會計年度起始月份
                <select
                  className="input"
                  value={fiscalMonth}
                  onChange={(e) => setFiscalMonth(e.target.value)}
                >
                  {Array.from({ length: 12 }, (_, i) => String(i + 1)).map((m) => (
                    <option key={m} value={m}>
                      {m} 月
                    </option>
                  ))}
                </select>
                <span className="field__hint">財務報表的預設期間依此設定。</span>
              </label>

              <div className="wizard-actions">
                <span className="section-note">下一步：建立第一個銀行帳戶</span>
                <button className="btn" disabled={savingCompany} onClick={() => void saveCompany()}>
                  {savingCompany ? '存檔中…' : '下一步'}
                </button>
              </div>
            </>
          )}
        </div>
      )}

      {step === 'bank' && (
        <div className="card wizard-panel">
          <h3 className="section-title">第一個銀行帳戶</h3>
          <p className="section-note">
            收款、付款與薪資都要指定資金進出帳戶，之後可於「銀行與現金」續增。
          </p>
          {!accounts && <p className="state-note">讀取中…</p>}
          {accounts && accounts.length === 0 && (
            <p className="form-warn">
              目前沒有啟用中的資產科目可以綁定，請先到「基礎設定」新增一個現金或銀行存款科目。
            </p>
          )}
          {accounts && accounts.length > 0 && (
            <>
              <div className="field-grid">
                <label className="field">
                  帳戶名稱
                  <input
                    className="input"
                    value={bankName}
                    placeholder="示範銀行 城中分行"
                    onChange={(e) => setBankName(e.target.value)}
                  />
                </label>
                <label className="field">
                  銀行帳號
                  <input
                    className="input num"
                    value={bankNo}
                    placeholder="123-456-789012"
                    onChange={(e) => setBankNo(e.target.value)}
                  />
                </label>
                <label className="field field--full">
                  綁定的總帳科目（必須是啟用中的資產科目，供收付款與核銷使用）
                  <select className="input" value={glCode} onChange={(e) => setGlCode(e.target.value)}>
                    {accounts.map((a) => (
                      <option key={a.code} value={a.code}>
                        {a.code} {a.name}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={() => setStep('company')}>
                  ← 上一步
                </button>
                <span className="spacer" />
                <button className="btn btn--ghost" onClick={() => setStep('dept')}>
                  略過此步
                </button>
                <button className="btn" disabled={savingBank} onClick={() => void saveBank()}>
                  {savingBank ? '存檔中…' : '建立並繼續'}
                </button>
              </div>
            </>
          )}
        </div>
      )}

      {step === 'dept' && (
        <div className="card wizard-panel">
          <h3 className="section-title">第一個部門</h3>
          <p className="section-note">
            員工名冊與部門損益都要使用部門，之後可於「基礎設定」續增。
          </p>
          {deptDimension && deptDimension.values.length > 0 && (
            <p className="section-note">
              目前已有：{deptDimension.values.map((v) => v.label).join('、')}
            </p>
          )}
          <div className="field-grid">
            <label className="field">
              部門名稱
              <input
                className="input"
                value={deptLabel}
                placeholder="業務部"
                onChange={(e) => setDeptLabel(e.target.value)}
              />
            </label>
          </div>
          <div className="wizard-actions">
            <button className="btn btn--ghost" onClick={() => setStep('bank')}>
              ← 上一步
            </button>
            <span className="spacer" />
            <button className="btn btn--ghost" onClick={() => setStep('hours')}>
              略過此步
            </button>
            <button className="btn" disabled={savingDept} onClick={() => void saveDept()}>
              {savingDept ? '存檔中…' : '建立並繼續'}
            </button>
          </div>
        </div>
      )}

      {step === 'hours' && (
        <div className="card wizard-panel">
          <h3 className="section-title">上下班時間</h3>
          <p className="section-note">
            填了上下班時間才判定遲到、才計加班時數；留空只記錄打卡時間，之後可在「基礎設定」補上。
          </p>
          <div className="field-grid">
            <label className="field">
              上班時間
              <TimeField label="上班時間" value={workStart} onChange={setWorkStart} />
            </label>
            <label className="field">
              下班時間
              <TimeField label="下班時間" value={workEnd} onChange={setWorkEnd} />
            </label>
            <label className="field">
              遲到寬限（分鐘）
              <input
                className="input num"
                inputMode="numeric"
                value={graceMinutes}
                placeholder="10"
                onChange={(e) => setGraceMinutes(e.target.value)}
              />
              <span className="field__hint">寬限僅免除遲到判定，下班時間不順延。</span>
            </label>
          </div>
          <div className="wizard-actions">
            <button className="btn btn--ghost" onClick={() => setStep('dept')}>
              ← 上一步
            </button>
            <span className="spacer" />
            <button className="btn btn--ghost" onClick={() => setStep('sso')}>
              略過此步
            </button>
            <button className="btn" disabled={savingHours} onClick={() => void saveHours()}>
              {savingHours ? '存檔中…' : '儲存並繼續'}
            </button>
          </div>
        </div>
      )}

      {step === 'sso' && (
        <div className="card wizard-panel">
          <h3 className="section-title">Google Workspace／Cloud Identity 登入</h3>
          <div className="field-grid">
            <label className="field field--full">
              用戶端 ID
              <input
                className="input"
                value={ssoClientId}
                autoFocus
                placeholder="…apps.googleusercontent.com"
                onChange={(e) => setSsoClientId(e.target.value)}
              />
            </label>
            <label className="field field--full">
              用戶端密鑰
              <input
                className="input"
                type="password"
                value={ssoSecret}
                placeholder={oidc?.has_client_secret ? '已設定，留空表示不變更' : 'GOCSPX-…'}
                onChange={(e) => setSsoSecret(e.target.value)}
              />
            </label>
            <label className="field">
              公司電子郵件網域
              <input
                className="input"
                value={ssoDomain}
                placeholder="example.com"
                onChange={(e) => setSsoDomain(e.target.value)}
              />
            </label>
            <label className="field">
              首次登入
              <select className="input" value={ssoRole} onChange={(e) => setSsoRole(e.target.value)}>
                <option value="">不自動建立帳號</option>
                {(oidc?.roles ?? []).map((r) => (
                  <option key={r.code} value={r.code}>
                    自動建立帳號，角色為{r.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              帳號密碼登入
              <select
                className="input"
                value={ssoPasswordOff ? 'off' : 'on'}
                onChange={(e) => setSsoPasswordOff(e.target.value === 'off')}
              >
                <option value="on">開放</option>
                <option value="off">停用</option>
              </select>
              <span className="field__hint">系統管理員仍可用密碼登入。</span>
            </label>
            <div className="field field--full">
              轉址網址
              <input className="input" value={ssoRedirect} readOnly />
              <span className="field__hint">
                <button
                  className="link-cell"
                  onClick={() => void navigator.clipboard.writeText(ssoRedirect)}
                >
                  複製
                </button>
                　貼進 Google Cloud console 的轉址白名單。
              </span>
            </div>
          </div>
          <div className="wizard-actions">
            <button className="btn btn--ghost" onClick={() => setStep('hours')}>
              ← 上一步
            </button>
            <span className="spacer" />
            <button className="btn btn--ghost" onClick={() => setStep('owner')}>
              略過此步
            </button>
            <button className="btn" disabled={savingSso} onClick={() => void saveSso()}>
              {savingSso ? '存檔中…' : '啟用並繼續'}
            </button>
          </div>
        </div>
      )}

      {step === 'owner' && (
        <div className="card wizard-panel">
          <h3 className="section-title">負責人帳號</h3>
          <p className="section-note">
            帳務、薪資與簽核門檻只有負責人看得到。
          </p>

          {ownerSecret ? (
            <>
              {ownerSecret.temporary_password === null ? (
                <div className="card secret-card">
                  <div className="secret-card__head">
                    <strong>
                      <span className="num">{ownerSecret.user.username}</span> 已建立
                    </strong>
                  </div>
                  <div className="fact">
                    <span className="fact__key">登入方式</span>
                    <span className="fact__value">{oidc?.provider_label ?? 'Google'}</span>
                  </div>
                  <div className="fact">
                    <span className="fact__key">密碼</span>
                    <span className="fact__value">無</span>
                  </div>
                </div>
              ) : (
                <div className="card secret-card">
                  <div className="secret-card__head">
                    <strong>
                      <span className="num">{ownerSecret.user.username}</span> 的初始密碼
                    </strong>
                  </div>
                  <code className="secret-card__value secret-card__value--lead num">
                    {ownerSecret.temporary_password}
                  </code>
                  <p className="form-warn">
                    離開本步驟後無法再查看，請立即交付{ownerSecret.user.display_name}本人。
                    首次登入會要求變更密碼。
                  </p>
                  <button
                    className="btn btn--compact btn--secondary"
                    onClick={() => void copyOwnerPassword()}
                  >
                    {ownerCopied ? '已複製' : '複製密碼'}
                  </button>
                </div>
              )}
              <div className="wizard-actions">
                <span className="spacer" />
                <button className="btn" onClick={() => setStep('done')}>
                  我已交付，繼續
                </button>
              </div>
            </>
          ) : ownerExists ? (
            <>
              <p className="state-note">公司已經有一個啟用中的負責人帳號。</p>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={() => setStep('hours')}>
                  ← 上一步
                </button>
                <span className="spacer" />
                <button className="btn" onClick={() => setStep('done')}>
                  繼續
                </button>
              </div>
            </>
          ) : (
            <>
              <div className="field-grid">
                <label className="field">
                  電子郵件（也是登入帳號）
                  <input
                    className="input"
                    type="email"
                    placeholder="chen@example.com"
                    value={ownerEmail}
                    onChange={(e) => setOwnerEmail(e.target.value)}
                  />
                </label>
                <label className="field">
                  姓名
                  <input
                    className="input"
                    placeholder="陳大明"
                    value={ownerDisplayName}
                    onChange={(e) => setOwnerDisplayName(e.target.value)}
                  />
                </label>
                <label className="field">
                  登入方式
                  <input
                    className={`input${passwordIsDead ? ' input--dead' : ''}`}
                    value={
                      ownerInDomain
                        ? (oidc?.provider_label ?? 'Google')
                        : passwordIsDead
                          ? '密碼（已停用）'
                          : '密碼'
                    }
                    readOnly
                  />
                  {ssoDomainReady && (
                    <span className="field__hint">@{oidc!.allowed_domain} 的地址不必設密碼。</span>
                  )}
                </label>
              </div>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={() => setStep('sso')}>
                  ← 上一步
                </button>
                <span className="spacer" />
                <button className="btn btn--ghost" onClick={() => setStep('done')}>
                  略過此步
                </button>
                <button
                  className="btn"
                  disabled={savingOwner || passwordIsDead}
                  onClick={() => void createOwner()}
                >
                  {savingOwner ? '建立中…' : '建立帳號'}
                </button>
              </div>
            </>
          )}
        </div>
      )}

      {step === 'done' && (
        <div className="card wizard-panel">
          <h3 className="section-title">首次設定完成</h3>
          {ownerExists === false && (
            <p className="section-note">
              帳務、薪資與簽核門檻只有負責人看得到，請到「帳號管理」建立負責人帳號。
            </p>
          )}
          <p className="section-note">員工名冊、會計科目與簽核流程之後隨時可以補。</p>
          <div className="wizard-actions">
            <button className="btn btn--ghost" onClick={() => setStep('owner')}>
              ← 上一步
            </button>
            <span className="spacer" />
            <button className="btn" onClick={onExit}>
              進入系統
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
