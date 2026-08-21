import { useEffect, useState } from 'react';
import {
  callOp,
  OpFailed,
  type CompanySettingRow,
  type DimensionList,
  type NewUserAccount,
  type UserAccountList,
} from '../api/client';
import { TimeField } from './TimeField';

interface Props {
  onToast: (msg: string) => void;
  onProgress: () => void;
  onExit: () => void;
}

type Step = 'company' | 'bank' | 'dept' | 'hours' | 'owner' | 'done';

type HourCode = 'work_start' | 'work_end' | 'late_grace_minutes';

const STEPS: { id: Step; label: string }[] = [
  { id: 'company', label: '① 公司資料' },
  { id: 'bank', label: '② 銀行帳戶' },
  { id: 'dept', label: '③ 部門' },
  { id: 'hours', label: '④ 上下班時間' },
  { id: 'owner', label: '⑤ 負責人帳號' },
  { id: 'done', label: '⑥ 完成' },
];

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
  const [ownerUsername, setOwnerUsername] = useState('');
  const [ownerDisplayName, setOwnerDisplayName] = useState('');
  const [ownerEmail, setOwnerEmail] = useState('');
  const [ownerSecret, setOwnerSecret] = useState<NewUserAccount | null>(null);
  const [savingOwner, setSavingOwner] = useState(false);
  const [ownerCopied, setOwnerCopied] = useState(false);

  const [dims, setDims] = useState<DimensionList | null>(null);
  const [deptCode, setDeptCode] = useState('');
  const [deptLabel, setDeptLabel] = useState('');
  const [savingDept, setSavingDept] = useState(false);

  useEffect(() => {
    if (step !== 'dept' || dims) return;
    callOp<DimensionList>('list_dimensions').then(setDims).catch(() => setDims(null));
  }, [step, dims]);

  const deptDimension = dims?.dimensions.find((d) => d.code === 'department') ?? null;

  async function saveDept() {
    if (!deptCode.trim() || !deptLabel.trim()) {
      onToast('部門代號與名稱都要填寫');
      return;
    }
    setSavingDept(true);
    try {
      await callOp<unknown>(
        'create_dimension_value',
        { dimension_code: 'department', code: deptCode.trim(), label: deptLabel.trim() },
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
    if (ownerUsername.trim() === '' || ownerDisplayName.trim() === '') {
      onToast('登入名稱與姓名都要填寫。');
      return;
    }
    setSavingOwner(true);
    try {
      const d = await callOp<NewUserAccount>(
        'create_user_account',
        {
          username: ownerUsername.trim(),
          display_name: ownerDisplayName.trim(),
          email: ownerEmail.trim(),
          role_code: 'boss',
          employee_no: '',
        },
        `建立負責人帳號 ${ownerUsername.trim()}`,
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
    if (!ownerSecret) return;
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
      setStep('owner');
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
      setStep('owner');
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
                <span className="field__hint">財務報表的預設期間跟著它走。</span>
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
            收款、付款與薪資都要指定資金進出帳戶，未建立帳戶前無法使用。之後可於「銀行與現金」續增。
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
            員工名冊與部門損益都要使用部門。請先建立一個常用部門（例如「業務部」），之後可於「基礎設定」續增。
          </p>
          {deptDimension && deptDimension.values.length > 0 && (
            <p className="section-note">
              目前已有：{deptDimension.values.map((v) => v.label).join('、')}
            </p>
          )}
          <div className="field-grid">
            <label className="field">
              部門代號
              <input
                className="input num"
                value={deptCode}
                placeholder="A01"
                onChange={(e) => setDeptCode(e.target.value)}
              />
              <span className="field__hint">
                報表與匯入檔用它對應部門，建立後不再更動；名稱之後可以改。
              </span>
            </label>
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
            打卡要有這兩個時間才判定遲到、才計加班時數。留空就只記錄打卡時間。這一項只有系統管理員改得動，之後在「基礎設定」。
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
              <span className="field__hint">寬限原諒遲到，下班時間不跟著往後。</span>
            </label>
          </div>
          <div className="wizard-actions">
            <button className="btn btn--ghost" onClick={() => setStep('dept')}>
              ← 上一步
            </button>
            <span className="spacer" />
            <button className="btn btn--ghost" onClick={() => setStep('owner')}>
              略過此步
            </button>
            <button className="btn" disabled={savingHours} onClick={() => void saveHours()}>
              {savingHours ? '存檔中…' : '儲存並繼續'}
            </button>
          </div>
        </div>
      )}

      {step === 'owner' && (
        <div className="card wizard-panel">
          <h3 className="section-title">負責人帳號</h3>
          <p className="section-note">
            帳務、薪資與簽核門檻歸負責人，你現在這個帳號看不到，也不能把自己改成負責人。
          </p>

          {ownerSecret ? (
            <>
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
                  離開這一步之後無法再查看，請立即交付{ownerSecret.user.display_name}本人。
                  首次登入會要求變更密碼。
                </p>
                <button
                  className="btn btn--compact btn--secondary"
                  onClick={() => void copyOwnerPassword()}
                >
                  {ownerCopied ? '已複製' : '複製密碼'}
                </button>
              </div>
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
                  登入名稱
                  <input
                    className="input"
                    placeholder="chen.owner"
                    value={ownerUsername}
                    onChange={(e) => setOwnerUsername(e.target.value)}
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
                <label className="field field--full">
                  電子郵件（選填）
                  <input
                    className="input"
                    type="email"
                    value={ownerEmail}
                    onChange={(e) => setOwnerEmail(e.target.value)}
                  />
                </label>
              </div>
              <p className="field__hint">
                密碼由系統產生、只顯示一次，首次登入時由本人變更。名冊上會同時建立一筆
                員工主檔，部門與薪資留給人資填。
              </p>
              <div className="wizard-actions">
                <button className="btn btn--ghost" onClick={() => setStep('hours')}>
                  ← 上一步
                </button>
                <span className="spacer" />
                <button className="btn btn--ghost" onClick={() => setStep('done')}>
                  略過此步
                </button>
                <button className="btn" disabled={savingOwner} onClick={() => void createOwner()}>
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
              還沒有負責人帳號。帳務、薪資與簽核門檻歸負責人，你現在這個帳號看不到；
              之後在「帳號管理」建立。
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
