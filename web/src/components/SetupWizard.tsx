import { useEffect, useState } from 'react';
import { callOp, OpFailed, type CompanySettingRow, type DimensionList } from '../api/client';

interface Props {
  onToast: (msg: string) => void;
  onProgress: () => void;
  onExit: () => void;
}

type Step = 'company' | 'bank' | 'dept' | 'done';

const STEPS: { id: Step; label: string }[] = [
  { id: 'company', label: '① 公司資料' },
  { id: 'bank', label: '② 銀行帳戶' },
  { id: 'dept', label: '③ 部門' },
  { id: 'done', label: '④ 完成' },
];

interface Account {
  code: string;
  name: string;
  kind: string;
  is_postable: boolean;
  is_active: boolean;
}

export function SetupWizard({ onToast, onProgress, onExit }: Props) {
  const [step, setStep] = useState<Step>('company');

  const [settings, setSettings] = useState<CompanySettingRow[] | null>(null);
  const [companyName, setCompanyName] = useState('');
  const [taxId, setTaxId] = useState('');
  const [savingCompany, setSavingCompany] = useState(false);

  useEffect(() => {
    callOp<{ settings: CompanySettingRow[] }>('get_company_settings')
      .then((d) => {
        setSettings(d.settings);
        setCompanyName(d.settings.find((s) => s.code === 'company_name')?.value ?? '');
        setTaxId(d.settings.find((s) => s.code === 'tax_id')?.value ?? '');
      })
      .catch(() => setSettings([]));
  }, []);

  async function saveCompany() {
    if (!companyName.trim()) {
      onToast('請輸入公司名稱。');
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
        const usable = d.accounts.filter((a) => a.kind === 'asset' && a.is_postable && a.is_active);
        setAccounts(usable);
        setGlCode((code) => code || usable[0]?.code || '');
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
      setStep('done');
    } catch (e) {
      onToast(e instanceof OpFailed ? e.error.message : String(e));
    } finally {
      setSavingDept(false);
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

              {settings.some((s) => !s.editable) && (
                <p className="field__hint">
                  {settings
                    .filter((s) => !s.editable)
                    .map((s) => s.label)
                    .join('、')}
                  以上項目無法在此變更，請至「基礎設定」處理。
                </p>
              )}

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
            收款、付款與薪資都要指定資金進出帳戶，未建立帳戶前無法使用。
            之後可於「銀行與現金」續增。
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
            員工名冊與部門損益都要使用部門。請先建立一個常用部門（例如「業務部」），
            之後可於「基礎設定」續增。
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
                placeholder="sales"
                onChange={(e) => setDeptCode(e.target.value)}
              />
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
            <button className="btn btn--ghost" onClick={() => setStep('done')}>
              略過此步
            </button>
            <button className="btn" disabled={savingDept} onClick={() => void saveDept()}>
              {savingDept ? '存檔中…' : '建立並繼續'}
            </button>
          </div>
        </div>
      )}

      {step === 'done' && (
        <div className="card wizard-panel">
          <h3 className="section-title">首次設定完成</h3>
          <p className="section-note">
            員工名冊、會計科目與簽核流程可隨時於「員工名冊」「基礎設定」「角色與核准」
            補齊，不用在此一次完成。
          </p>
          <div className="wizard-actions">
            <span />
            <button className="btn" onClick={onExit}>
              進入系統
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
